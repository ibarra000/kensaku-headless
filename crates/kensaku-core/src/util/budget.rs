//! Byte budget bounding decoded image buffers in flight; oversized requests
//! are clamped to the whole budget and run alone instead of deadlocking.

use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

/// Default decode budget (768 MiB); override with `KENSAKU_DECODE_BUDGET_MB`.
pub const DEFAULT_DECODE_BUDGET_BYTES: u64 = 768 * 1024 * 1024;

/// Counting semaphore measured in bytes.
pub struct DecodeBudget {
    total: u64,
    in_use: Mutex<u64>,
    released: Condvar,
}

#[must_use = "dropping the permit immediately releases the reservation"]
pub struct Permit {
    budget: &'static DecodeBudget,
    bytes: u64,
}

impl DecodeBudget {
    /// Zero is treated as one byte, so requests serialize rather than fail.
    pub fn new(total: u64) -> Self {
        Self {
            total: total.max(1),
            in_use: Mutex::new(0),
            released: Condvar::new(),
        }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn in_use(&self) -> u64 {
        *self.lock()
    }

    /// Blocks until `bytes` (clamped to the budget, min 1) can be reserved.
    pub fn acquire(&'static self, bytes: u64) -> Permit {
        let wanted = bytes.clamp(1, self.total);
        let mut in_use = self.lock();
        while *in_use + wanted > self.total {
            in_use = self
                .released
                .wait(in_use)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *in_use += wanted;
        Permit {
            budget: self,
            bytes: wanted,
        }
    }

    fn lock(&self) -> MutexGuard<'_, u64> {
        // Poison is harmless: the counter needs no repair.
        self.in_use.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn release(&self, bytes: u64) {
        let mut in_use = self.lock();
        *in_use = in_use.saturating_sub(bytes);
        drop(in_use);
        self.released.notify_all();
    }
}

impl Permit {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

pub fn decode_budget() -> &'static DecodeBudget {
    static BUDGET: OnceLock<DecodeBudget> = OnceLock::new();
    BUDGET.get_or_init(|| {
        let bytes = std::env::var("KENSAKU_DECODE_BUDGET_MB")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|mb| *mb > 0)
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(DEFAULT_DECODE_BUDGET_BYTES);
        DecodeBudget::new(bytes)
    })
}

pub fn rgba_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    fn leak(total: u64) -> &'static DecodeBudget {
        Box::leak(Box::new(DecodeBudget::new(total)))
    }

    #[test]
    fn reserves_and_releases_on_drop() {
        let budget = leak(100);
        let a = budget.acquire(40);
        assert_eq!(budget.in_use(), 40);
        let b = budget.acquire(60);
        assert_eq!(budget.in_use(), 100);
        drop(a);
        assert_eq!(budget.in_use(), 60);
        drop(b);
        assert_eq!(budget.in_use(), 0);
    }

    #[test]
    fn oversized_request_is_clamped_and_runs_alone() {
        let budget = leak(100);
        let huge = budget.acquire(10_000);
        assert_eq!(huge.bytes(), 100);
        assert_eq!(budget.in_use(), 100);
        drop(huge);
        assert_eq!(budget.in_use(), 0);
    }

    #[test]
    fn zero_request_still_reserves_one_byte() {
        let budget = leak(100);
        let p = budget.acquire(0);
        assert_eq!(p.bytes(), 1);
        drop(p);
    }

    #[test]
    fn concurrent_holders_never_exceed_the_budget() {
        // At most three 30-byte permits fit in 100, so the peak stays <= 100.
        let budget = leak(100);
        let live = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..12)
            .map(|_| {
                let live = Arc::clone(&live);
                let peak = Arc::clone(&peak);
                thread::spawn(move || {
                    let permit = budget.acquire(30);
                    let now = live.fetch_add(permit.bytes(), Ordering::SeqCst) + permit.bytes();
                    peak.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(5));
                    live.fetch_sub(permit.bytes(), Ordering::SeqCst);
                    drop(permit);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
        assert!(
            peak.load(Ordering::SeqCst) <= 100,
            "peak reserved bytes {} exceeded the budget",
            peak.load(Ordering::SeqCst)
        );
        assert_eq!(budget.in_use(), 0);
    }

    #[test]
    fn a_waiter_proceeds_once_space_is_released() {
        let budget = leak(100);
        let held = budget.acquire(100);
        let waiter = thread::spawn(move || {
            let started = std::time::Instant::now();
            let _p = budget.acquire(50);
            started.elapsed()
        });
        thread::sleep(Duration::from_millis(30));
        drop(held);
        let waited = waiter.join().unwrap();
        assert!(waited >= Duration::from_millis(20), "waiter did not block");
    }

    #[test]
    fn rgba_bytes_is_four_per_pixel() {
        assert_eq!(rgba_bytes(10, 10), 400);
        assert_eq!(rgba_bytes(10_000, 10_000), 400_000_000);
    }
}
