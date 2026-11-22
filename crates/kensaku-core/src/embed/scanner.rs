use crate::embed::model::ModelRegistry;
use crate::embed::units::{Unit, UnitStore};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct SemanticHit {
    pub node_id: u64,
    pub score: f32,
    pub best_unit: Unit,
}

pub struct ExactScanner;

impl ExactScanner {
    /// Exact parallel top-k scan grouped by node_id; `allowed` filters before scoring.
    pub fn scan(
        query_vec: &[i8],
        store: &UnitStore,
        allowed: Option<&HashSet<u64>>,
        limit: usize,
    ) -> Vec<SemanticHit> {
        Self::scan_with_model(query_vec, store, allowed, limit, None)
    }

    /// Like [`Self::scan`], skipping units from a model other than `expected_model_id`.
    pub fn scan_with_model(
        query_vec: &[i8],
        store: &UnitStore,
        allowed: Option<&HashSet<u64>>,
        limit: usize,
        expected_model_id: Option<u32>,
    ) -> Vec<SemanticHit> {
        if store.is_empty() || query_vec.is_empty() || query_vec.len() != store.dim {
            return Vec::new();
        }

        let dim = store.dim;
        let units = &store.units;
        let vectors = &store.vectors;

        let scored_units: Vec<(usize, f32)> = (0..units.len())
            .into_par_iter()
            .filter_map(|idx| {
                let unit = &units[idx];
                if let Some(expected_id) = expected_model_id
                    && unit.model_id != expected_id
                {
                    return None;
                }

                let node_id = unit.node_id;
                if let Some(allowed) = allowed
                    && !allowed.contains(&node_id)
                {
                    return None;
                }

                let start = idx * dim;
                let end = start + dim;
                let unit_vec = &vectors[start..end];
                let sim = ModelRegistry::cosine_similarity_i8(query_vec, unit_vec);
                Some((idx, sim))
            })
            .filter(|&(_, sim)| sim > 0.05)
            .collect();

        let mut node_best: HashMap<u64, (f32, usize)> = HashMap::new();
        for (unit_idx, sim) in scored_units {
            let node_id = units[unit_idx].node_id;
            node_best
                .entry(node_id)
                .and_modify(|existing| {
                    if sim > existing.0 {
                        *existing = (sim, unit_idx);
                    }
                })
                .or_insert((sim, unit_idx));
        }

        // Clone a Unit only for survivors; most nodes are cut by `limit`.
        let mut ranked: Vec<(u64, f32, usize)> = node_best
            .into_iter()
            .map(|(node_id, (score, best_idx))| (node_id, score, best_idx))
            .collect();
        let by_score = |a: &(u64, f32, usize), b: &(u64, f32, usize)| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        };
        if ranked.len() > limit {
            ranked.select_nth_unstable_by(limit, by_score);
            ranked.truncate(limit);
        }
        ranked.sort_by(by_score);

        ranked
            .into_iter()
            .map(|(node_id, score, best_idx)| SemanticHit {
                node_id,
                score,
                best_unit: units[best_idx].clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: u64, node_id: u64) -> Unit {
        Unit {
            id,
            node_id,
            byte_start: 0,
            byte_end: 10,
            model_id: 1,
        }
    }

    #[test]
    fn prefilter_skips_non_candidate_nodes_before_scoring() {
        let mut store = UnitStore::new(4);
        store.add_unit(unit(0, 1), &[127, 0, 0, 0]);
        store.add_unit(unit(1, 2), &[0, 127, 0, 0]);

        let query = [0i8, 127, 0, 0];

        let all = ExactScanner::scan(&query, &store, None, 10);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].node_id, 2);

        let allowed: HashSet<u64> = [1u64].into_iter().collect();
        let filtered = ExactScanner::scan(&query, &store, Some(&allowed), 10);
        assert!(filtered.is_empty(), "node 2 should be skipped by prefilter");

        let allowed: HashSet<u64> = [1u64, 2u64].into_iter().collect();
        let both = ExactScanner::scan(&query, &store, Some(&allowed), 10);
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].node_id, 2);
    }

    #[test]
    fn model_id_isolation_skips_units_from_different_models() {
        let mut store = UnitStore::new(4);
        let mut u1 = unit(0, 1);
        u1.model_id = 1;
        let mut u2 = unit(1, 2);
        u2.model_id = 2; // Different model

        store.add_unit(u1, &[127, 0, 0, 0]);
        store.add_unit(u2, &[127, 0, 0, 0]);

        let query = [127i8, 0, 0, 0];

        let hits = ExactScanner::scan_with_model(&query, &store, None, 10, Some(1));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node_id, 1);

        let hits = ExactScanner::scan_with_model(&query, &store, None, 10, Some(2));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node_id, 2);

        let hits = ExactScanner::scan_with_model(&query, &store, None, 10, Some(3));
        assert!(hits.is_empty());
    }

    #[test]
    fn limit_is_respected_when_many_nodes_match() {
        let mut store = UnitStore::new(4);
        for node_id in 1u64..=10 {
            store.add_unit(unit(node_id, node_id), &[127i8, 0, 0, 0]);
        }
        let query = [127i8, 0, 0, 0];
        let hits = ExactScanner::scan(&query, &store, None, 5);
        assert_eq!(hits.len(), 5, "limit must cap results to 5");
    }

    #[test]
    fn results_are_ordered_by_score_descending() {
        let mut store = UnitStore::new(4);
        store.add_unit(unit(0, 1), &[127i8, 0, 0, 0]);
        store.add_unit(unit(1, 2), &[100i8, 50, 0, 0]);
        store.add_unit(unit(2, 3), &[60i8, 60, 60, 0]);

        let query = [127i8, 0, 0, 0];
        let hits = ExactScanner::scan(&query, &store, None, 10);

        for window in hits.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "results not in descending score order"
            );
        }
        assert_eq!(hits[0].node_id, 1);
    }

    #[test]
    fn score_threshold_filters_low_similarity_units() {
        let mut store = UnitStore::new(4);
        store.add_unit(unit(0, 1), &[0i8, 127, 0, 0]);
        store.add_unit(unit(1, 2), &[127i8, 0, 0, 0]);

        let query = [127i8, 0, 0, 0];
        let hits = ExactScanner::scan(&query, &store, None, 10);

        assert!(
            hits.iter().all(|h| h.node_id != 1),
            "node 1 should be filtered by the 0.05 score threshold"
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node_id, 2);
    }

    #[test]
    fn dim_mismatch_returns_empty() {
        let mut store = UnitStore::new(4);
        store.add_unit(unit(0, 1), &[127i8, 0, 0, 0]);

        let query = [127i8, 0, 0];
        let hits = ExactScanner::scan(&query, &store, None, 10);
        assert!(hits.is_empty(), "dim mismatch must return empty");
    }

    #[test]
    fn empty_store_returns_empty() {
        let store = UnitStore::new(4);
        let query = [127i8, 0, 0, 0];
        let hits = ExactScanner::scan(&query, &store, None, 10);
        assert!(hits.is_empty());
    }

    #[test]
    fn allowed_filter_with_no_matches_returns_empty() {
        let mut store = UnitStore::new(4);
        store.add_unit(unit(0, 1), &[127i8, 0, 0, 0]);
        store.add_unit(unit(1, 2), &[127i8, 0, 0, 0]);

        let allowed: HashSet<u64> = [99u64].into_iter().collect();
        let hits = ExactScanner::scan(&query_vec(), &store, Some(&allowed), 10);
        assert!(hits.is_empty());
    }

    fn query_vec() -> [i8; 4] {
        [127i8, 0, 0, 0]
    }
}
