use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

static MAX_SEMANTIC_UNITS: AtomicUsize = AtomicUsize::new(20);

pub fn set_max_semantic_units(n: usize) {
    MAX_SEMANTIC_UNITS.store(n.max(1), Ordering::SeqCst);
}

pub fn max_semantic_units() -> usize {
    MAX_SEMANTIC_UNITS.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    pub id: u64,
    pub node_id: u64,
    pub byte_start: usize,
    pub byte_end: usize,
    pub model_id: u32,
}

/// <= 240 tokens: one doc-level unit; longer: 240-token windows, 48 overlap, capped units.
pub fn chunk_text(text: &str, file_size: usize) -> Vec<(usize, usize, String)> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let max_units = max_semantic_units();
    // Only scan enough words to fill `max_units` windows.
    let scan_cap = (max_units.saturating_sub(1)) * 192 + 240 + 1;

    let mut word_offsets: Vec<(usize, usize)> = Vec::with_capacity(scan_cap.min(4096));
    let mut in_word = false;
    let mut word_start = 0;
    let mut fully_scanned = true;

    for (byte_idx, ch) in text.char_indices() {
        if word_offsets.len() >= scan_cap {
            fully_scanned = false;
            break;
        }
        if ch.is_whitespace() {
            if in_word {
                word_offsets.push((word_start, byte_idx));
                in_word = false;
            }
        } else if !in_word {
            word_start = byte_idx;
            in_word = true;
        }
    }
    if in_word {
        if word_offsets.len() < scan_cap {
            word_offsets.push((word_start, text.len()));
        } else {
            fully_scanned = false;
        }
    }

    if fully_scanned && word_offsets.len() <= 240 {
        return vec![(0, file_size, text.to_string())];
    }

    let mut chunks = Vec::new();
    let window_size = 240;
    let step = 240 - 48; // 192 tokens step with 48 tokens overlap

    let mut start_idx = 0;
    while start_idx < word_offsets.len() && chunks.len() < max_units {
        let end_idx = (start_idx + window_size).min(word_offsets.len());
        let byte_start = word_offsets[start_idx].0;
        let byte_end = word_offsets[end_idx - 1].1;
        let chunk_slice = &text[byte_start..byte_end];

        chunks.push((byte_start, byte_end, chunk_slice.to_string()));

        if end_idx >= word_offsets.len() {
            break;
        }
        start_idx += step;
    }

    chunks
}

#[derive(Default)]
pub struct UnitStore {
    pub units: Vec<Unit>,
    pub vectors: Vec<i8>,
    pub node_to_units: HashMap<u64, Vec<usize>>,
    pub dim: usize,
}

impl UnitStore {
    pub fn new(dim: usize) -> Self {
        Self {
            units: Vec::new(),
            vectors: Vec::new(),
            node_to_units: HashMap::new(),
            dim,
        }
    }

    pub fn add_unit(&mut self, unit: Unit, vector: &[i8]) {
        if vector.len() != self.dim {
            return;
        }
        let unit_idx = self.units.len();
        self.node_to_units
            .entry(unit.node_id)
            .or_default()
            .push(unit_idx);
        self.units.push(unit);
        self.vectors.extend_from_slice(vector);
    }

    pub fn remove_units_for_node(&mut self, node_id: u64) {
        let mut set = std::collections::HashSet::new();
        set.insert(node_id);
        self.remove_units_for_nodes(&set);
    }

    /// Removes all units of `node_ids` in one pass (O(units), not O(units × files)).
    pub fn remove_units_for_nodes(&mut self, node_ids: &std::collections::HashSet<u64>) {
        if node_ids.is_empty() || self.units.is_empty() {
            return;
        }

        let mut new_units = Vec::with_capacity(self.units.len());
        let mut new_vectors = Vec::with_capacity(self.vectors.len());
        let mut new_node_map: HashMap<u64, Vec<usize>> = HashMap::new();

        for (old_idx, unit) in self.units.drain(..).enumerate() {
            if node_ids.contains(&unit.node_id) {
                continue;
            }
            let new_idx = new_units.len();
            new_node_map.entry(unit.node_id).or_default().push(new_idx);
            new_units.push(unit);
            let v_start = old_idx * self.dim;
            let v_end = v_start + self.dim;
            new_vectors.extend_from_slice(&self.vectors[v_start..v_end]);
        }

        self.units = new_units;
        self.vectors = new_vectors;
        self.node_to_units = new_node_map;
    }

    pub fn clear(&mut self) {
        self.units.clear();
        self.vectors.clear();
        self.node_to_units.clear();
    }

    /// Drops units not produced by `expected_model_id`.
    pub fn retain_model(&mut self, expected_model_id: u32) {
        if self.units.iter().all(|u| u.model_id == expected_model_id) {
            return;
        }
        let mut new_units = Vec::new();
        let mut new_vectors = Vec::new();
        let mut new_node_to_units: HashMap<u64, Vec<usize>> = HashMap::new();
        for (idx, unit) in self.units.iter().enumerate() {
            if unit.model_id == expected_model_id {
                let start = idx * self.dim;
                let end = start + self.dim;
                let new_idx = new_units.len();
                new_node_to_units
                    .entry(unit.node_id)
                    .or_default()
                    .push(new_idx);
                new_units.push(unit.clone());
                new_vectors.extend_from_slice(&self.vectors[start..end]);
            }
        }
        self.units = new_units;
        self.vectors = new_vectors;
        self.node_to_units = new_node_to_units;
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    pub fn get_unit(&self, idx: usize) -> Option<&Unit> {
        self.units.get(idx)
    }

    pub fn get_vector(&self, idx: usize) -> Option<&[i8]> {
        let start = idx * self.dim;
        let end = start + self.dim;
        if end <= self.vectors.len() {
            Some(&self.vectors[start..end])
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_text_short_doc_is_single_unit() {
        let text = "the quick brown fox jumps over the lazy dog now";
        let file_size = text.len() + 10; // simulate file slightly larger than slice
        let chunks = chunk_text(text, file_size);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, 0, "byte_start must be 0 for short doc");
        assert_eq!(chunks[0].1, file_size, "byte_end must equal file_size");
        assert_eq!(chunks[0].2, text);
    }

    #[test]
    fn chunk_text_exactly_240_words_is_single_unit() {
        let text = (0..240)
            .map(|i| format!("word{}", i))
            .collect::<Vec<_>>()
            .join(" ");
        let file_size = text.len();
        let chunks = chunk_text(&text, file_size);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, 0);
        assert_eq!(chunks[0].1, file_size);
    }

    /// 241 words: windows 0..240 and 192..241.
    #[test]
    fn chunk_text_241_words_produces_two_windows() {
        let text = (0..241)
            .map(|i| format!("word{}", i))
            .collect::<Vec<_>>()
            .join(" ");
        let file_size = text.len();
        let chunks = chunk_text(&text, file_size);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0, 0);
        assert_ne!(chunks[0].2, chunks[1].2);
    }

    #[test]
    fn chunk_text_respects_max_units_cap() {
        let text = (0..10_000)
            .map(|i| format!("word{}", i))
            .collect::<Vec<_>>()
            .join(" ");
        let file_size = text.len();

        set_max_semantic_units(3);
        let chunks = chunk_text(&text, file_size);
        assert!(
            chunks.len() <= 3,
            "expected at most 3 chunks, got {}",
            chunks.len()
        );

        // Restore the default for other tests.
        set_max_semantic_units(20);
    }

    #[test]
    fn chunk_text_empty_and_whitespace_return_empty() {
        assert!(chunk_text("", 0).is_empty());
        assert!(chunk_text("   \t\n\r\n  ", 100).is_empty());
    }

    #[test]
    fn chunk_text_single_huge_word_is_one_unit() {
        let word = "a".repeat(10_000);
        let file_size = word.len();
        let chunks = chunk_text(&word, file_size);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].2, word);
    }

    #[test]
    fn chunk_text_multibyte_byte_offsets_are_valid() {
        // 300 three-byte chars exceed the single-unit threshold.
        let text = (0..300).map(|_| "日").collect::<Vec<_>>().join(" ");
        let file_size = text.len();
        let chunks = chunk_text(&text, file_size);
        for (start, end, content) in &chunks {
            assert!(*start <= *end);
            assert!(*end <= text.len());
            assert!(text.is_char_boundary(*start), "start not on char boundary");
            assert!(text.is_char_boundary(*end), "end not on char boundary");
            assert_eq!(&text[*start..*end], content.as_str());
        }
    }

    fn make_unit(id: u64, node_id: u64) -> Unit {
        Unit {
            id,
            node_id,
            byte_start: 0,
            byte_end: 8,
            model_id: 1,
        }
    }

    #[test]
    fn unit_store_add_unit_dim_guard() {
        let mut store = UnitStore::new(4);
        store.add_unit(make_unit(0, 1), &[1i8, 2, 3]); // 3 ≠ 4
        assert!(store.is_empty());

        store.add_unit(make_unit(1, 1), &[1i8, 2, 3, 4]);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn unit_store_get_vector_round_trips() {
        let mut store = UnitStore::new(3);
        store.add_unit(make_unit(0, 10), &[10i8, 20, 30]);
        store.add_unit(make_unit(1, 10), &[-5i8, -10, -15]);

        assert_eq!(store.get_vector(0), Some([10i8, 20, 30].as_slice()));
        assert_eq!(store.get_vector(1), Some([-5i8, -10, -15].as_slice()));
        assert_eq!(store.get_vector(2), None);
    }

    #[test]
    fn unit_store_remove_units_for_nodes_single_pass_reindex() {
        let mut store = UnitStore::new(2);
        store.add_unit(make_unit(0, 1), &[1i8, 0]);
        store.add_unit(make_unit(1, 2), &[0i8, 1]);
        store.add_unit(make_unit(2, 1), &[2i8, 0]);
        store.add_unit(make_unit(3, 3), &[0i8, 2]);

        let mut to_remove = std::collections::HashSet::new();
        to_remove.insert(1u64);
        store.remove_units_for_nodes(&to_remove);

        assert_eq!(store.len(), 2);
        assert!(store.node_to_units.contains_key(&2));
        assert!(store.node_to_units.contains_key(&3));
        assert!(!store.node_to_units.contains_key(&1));

        for (i, unit) in store.units.iter().enumerate() {
            let vec = store.get_vector(i).expect("vector must exist");
            assert_eq!(vec.len(), 2);
            match unit.node_id {
                2 => assert_eq!(vec, &[0i8, 1]),
                3 => assert_eq!(vec, &[0i8, 2]),
                _ => panic!("unexpected node_id"),
            }
        }
    }

    #[test]
    fn unit_store_remove_empty_set_is_noop() {
        let mut store = UnitStore::new(2);
        store.add_unit(make_unit(0, 1), &[1i8, 2]);
        let empty = std::collections::HashSet::new();
        store.remove_units_for_nodes(&empty);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn unit_store_retain_model_filters_by_model_id() {
        let mut store = UnitStore::new(2);
        let mut u1 = make_unit(0, 1);
        u1.model_id = 1;
        let mut u2 = make_unit(1, 2);
        u2.model_id = 2;
        let mut u3 = make_unit(2, 3);
        u3.model_id = 1;

        store.add_unit(u1, &[1i8, 0]);
        store.add_unit(u2, &[0i8, 1]);
        store.add_unit(u3, &[2i8, 0]);

        store.retain_model(1);
        assert_eq!(store.len(), 2);
        assert!(store.units.iter().all(|u| u.model_id == 1));
        assert!(!store.node_to_units.contains_key(&2));
    }

    #[test]
    fn unit_store_retain_model_all_match_is_noop() {
        let mut store = UnitStore::new(2);
        store.add_unit(make_unit(0, 1), &[1i8, 0]);
        store.add_unit(make_unit(1, 2), &[0i8, 1]);
        store.retain_model(1); // all model_id = 1
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn unit_store_clear_empties_all_fields() {
        let mut store = UnitStore::new(2);
        store.add_unit(make_unit(0, 1), &[1i8, 2]);
        store.clear();
        assert!(store.is_empty());
        assert!(store.vectors.is_empty());
        assert!(store.node_to_units.is_empty());
    }
}
