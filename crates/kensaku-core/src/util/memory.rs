//! Best-effort return of freed heap pages to the OS; no-op where unsupported.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn release_memory() {
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(target_os = "macos")]
pub fn release_memory() {
    // Not in libc's macOS bindings; declared here to avoid a C shim.
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut core::ffi::c_void, goal: usize) -> usize;
        fn malloc_default_zone() -> *mut core::ffi::c_void;
    }
    unsafe {
        let _ = malloc_zone_pressure_relief(malloc_default_zone(), 0);
    }
}

#[cfg(windows)]
pub fn release_memory() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static LAST_RELEASE: AtomicU64 = AtomicU64::new(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST_RELEASE.load(Ordering::Relaxed);
    if now >= last + 5
        && LAST_RELEASE
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        #[link(name = "psapi")]
        unsafe extern "system" {
            fn EmptyWorkingSet(h_process: *mut core::ffi::c_void) -> i32;
            fn GetCurrentProcess() -> *mut core::ffi::c_void;
        }
        unsafe {
            let _ = EmptyWorkingSet(GetCurrentProcess());
        }
    }
}

#[cfg(not(any(
    all(target_os = "linux", target_env = "gnu"),
    target_os = "macos",
    windows
)))]
pub fn release_memory() {}

#[cfg(test)]
mod tests {
    #[test]
    fn release_memory_is_callable() {
        super::release_memory();
        super::release_memory();
    }
}
