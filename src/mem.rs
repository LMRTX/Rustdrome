//! Allocator tuning — keeping resident memory low without costing speed.
//!
//! glibc's malloc creates up to `8 × cores` arenas for multi-threaded
//! programs and is slow to hand freed pages back, so a mostly-idle app
//! that does occasional bursts of work (decode a cover, parse a 5000-song
//! library) ends up holding on to peak memory forever. Two cheap fixes:
//! cap the arena count at startup, and `malloc_trim` after the bursts.
//! Both are no-ops on platforms without glibc.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn tune() {
    // SAFETY: mallopt only adjusts allocator parameters and is called once,
    // before any other threads exist.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 128 * 1024);
    }
}

/// Return freed heap pages to the OS. Call after a burst of allocation.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn trim() {
    // SAFETY: malloc_trim is always safe to call.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn tune() {}
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn trim() {}
