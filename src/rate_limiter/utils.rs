//! # Utility Functions (utils.rs)
//!
//! Platform-specific optimizations and helper functions for the rate limiter.
//! This module provides low-level utilities that make the rate limiter fast
//! and efficient on different CPU architectures.
//!
//! ## Platform Optimizations
//!
//! ```text
//!     Platform-Specific Features:
//!     
//!     x86_64 (Intel/AMD):
//!     ├─ Cache line: 64 bytes
//!     ├─ PAUSE instruction for spin loops
//!     └─ SSE2 optimizations
//!     
//!     AArch64 (ARM):
//!     ├─ Cache line: 128 bytes
//!     ├─ YIELD instruction for spin loops
//!     └─ Different memory model
//!     
//!     Generic (Fallback):
//!     ├─ Cache line: 64 bytes (assumed)
//!     └─ Standard spin loop hints
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// Monotonic time base to prevent issues when the system clock jumps.
// Microseconds are used as the base unit to avoid u64 overflow.
static START_TIME_BASE: OnceLock<TimeBase> = OnceLock::new();

// Coarsely cached millisecond timestamp. Updated on every call to
// current_time_ms(). This avoids redundant Instant::elapsed() calls
// within the same millisecond (common under high throughput).
static CACHED_TIME_MS: AtomicU64 = AtomicU64::new(0);

struct TimeBase {
    start: Instant,
    base_us: u64,
    base_ms: u64,
}

fn init_time_base() -> TimeBase {
    let epoch_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    TimeBase {
        start: Instant::now(),
        base_us: epoch_us,
        base_ms: epoch_us / 1_000,
    }
}

/// CPU-specific relaxation hint for spin loops.
///
/// This function tells the CPU that we're in a spin loop, allowing it to:
/// - Reduce power consumption
/// - Give resources to other threads
/// - Avoid memory order violations in spin loops
///
/// ## Platform Behavior
///
/// - **x86_64**: Compiles to PAUSE instruction (reduces power, improves performance)
/// - **ARM64**: Compiles to YIELD instruction (hints to give up time slice)
/// - **Others**: Falls back to standard spin loop hint
///
/// ## Usage in Spin Loops
///
/// ```rust
/// use rater::cpu_relax;
///
/// // In a spin loop waiting for a condition
/// let mut retries = 0;
/// let condition_met = true;
/// while !condition_met {
///     if retries > 10 {
///         // After many retries, yield to OS scheduler
///         std::thread::yield_now();
///     } else {
///         // Light-weight CPU relaxation
///         cpu_relax();
///     }
///     retries += 1;
/// }
/// ```
#[inline(always)]
pub fn cpu_relax() {
    std::hint::spin_loop();
}

/// Returns the current time in milliseconds since UNIX epoch.
///
/// Used for tracking refill times and access patterns.
/// Millisecond precision is sufficient for rate limiting.
///
/// # Example
///
/// ```rust
/// use rater::current_time_ms;
///
/// let now = current_time_ms();
/// println!("Current timestamp: {} ms", now);
/// ```
#[inline(always)]
pub fn current_time_ms() -> u64 {
    let tb = START_TIME_BASE.get_or_init(init_time_base);
    let now = tb.base_ms.wrapping_add(tb.start.elapsed().as_millis() as u64);
    CACHED_TIME_MS.store(now, Ordering::Relaxed);
    now
}

/// Returns the last value written by `current_time_ms()` without
/// performing a new time measurement. Zero if never called.
#[inline(always)]
pub(crate) fn cached_time_ms() -> u64 {
    CACHED_TIME_MS.load(Ordering::Relaxed)
}

/// Returns the current time in microseconds since UNIX epoch.
///
/// Higher precision timing for performance measurements.
///
/// # Example
///
/// ```rust
/// use rater::current_time_us;
///
/// let start = current_time_us();
/// // ... some operation ...
/// let elapsed = current_time_us() - start;
/// println!("Operation took {} microseconds", elapsed);
/// ```
#[inline(always)]
pub fn current_time_us() -> u64 {
    let tb = START_TIME_BASE.get_or_init(init_time_base);
    tb.base_us.saturating_add(tb.start.elapsed().as_micros() as u64)
}

/// Returns the current time in nanoseconds since UNIX epoch.
///
/// Highest precision timing for fine-grained measurements.
/// Note: Actual precision depends on the OS and hardware.
///
/// # Example
///
/// ```rust
/// use rater::current_time_ns;
///
/// let start = current_time_ns();
/// // ... some fast operation ...
/// let elapsed = current_time_ns() - start;
/// println!("Operation took {} nanoseconds", elapsed);
/// ```
#[inline(always)]
pub fn current_time_ns() -> u64 {
    let tb = START_TIME_BASE.get_or_init(init_time_base);
    tb.base_us
        .saturating_mul(1_000)
        .saturating_add(tb.start.elapsed().as_nanos() as u64)
}

/// Cache-aligned wrapper for values to prevent false sharing.
///
/// False sharing occurs when multiple threads access different variables
/// that happen to be on the same cache line. This causes unnecessary
/// cache invalidation and performance degradation.
///
/// ## How It Works
///
/// ```text
///     Without Cache Alignment:
///     ┌─────────────────────────┐
///     │ Thread A var │Thread B var│ ← Same cache line
///     └─────────────────────────┘
///     Problem: Modifying A invalidates B's cache
///     
///     With Cache Alignment:
///     ┌─────────────────────────┐
///     │     Thread A variable    │ ← Own cache line
///     └─────────────────────────┘
///     ┌─────────────────────────┐
///     │     Thread B variable    │ ← Own cache line
///     └─────────────────────────┘
///     Result: No false sharing!
/// ```
///
#[cfg(target_arch = "x86_64")]
#[repr(C, align(64))]
pub(crate) struct CacheAligned<T>(pub T);
#[cfg(target_arch = "aarch64")]
#[repr(C, align(128))]
pub(crate) struct CacheAligned<T>(pub T);
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[repr(C, align(64))]
pub(crate) struct CacheAligned<T>(pub T);

impl<T> CacheAligned<T> {
    /// Creates a new cache-aligned value.
    #[inline(always)]
    pub(crate) const fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T: Default> Default for CacheAligned<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

impl<T: Clone> Clone for CacheAligned<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for CacheAligned<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    const CACHE_LINE_SIZE: usize = 64;
    #[cfg(target_arch = "aarch64")]
    const CACHE_LINE_SIZE: usize = 128;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const CACHE_LINE_SIZE: usize = 64;

    #[derive(Debug, Clone)]
    struct Backoff {
        step: u32,
        max_step: u32,
    }

    impl Backoff {
        fn new(max_step: u32) -> Self {
            Self { step: 0, max_step }
        }

        fn backoff(&mut self) {
            if self.step < 4 {
                for _ in 0..(1 << self.step) {
                    cpu_relax();
                }
            } else {
                std::thread::yield_now();
            }
            self.step = (self.step + 1).min(self.max_step);
        }

        fn reset(&mut self) {
            self.step = 0;
        }

        fn is_at_max(&self) -> bool {
            self.step >= self.max_step
        }
    }

    impl<T> CacheAligned<T> {
        fn get(&self) -> &T {
            &self.0
        }

        fn get_mut(&mut self) -> &mut T {
            &mut self.0
        }

        fn into_inner(self) -> T {
            self.0
        }
    }

    #[test]
    fn test_cache_line_size() {
        let size = CACHE_LINE_SIZE;
        assert!(size >= 32);
        assert!(size <= 256);
        assert!(size.is_power_of_two());
    }

    #[test]
    fn test_time_functions() {
        let ms1 = current_time_ms();
        let us1 = current_time_us();
        let ns1 = current_time_ns();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let ms2 = current_time_ms();
        let us2 = current_time_us();
        let ns2 = current_time_ns();

        assert!(ms2 >= ms1);
        assert!(us2 > us1);
        assert!(ns2 > ns1);

        // Check reasonable relationships
        assert!(us1 >= ms1 * 1000);
        assert!(ns1 >= us1 * 1000);
    }

    #[test]
    fn test_cache_aligned() {
        use std::sync::atomic::AtomicU64;

        let aligned = CacheAligned::new(AtomicU64::new(42));

        // Verify the value is accessible
        assert_eq!(aligned.0.load(std::sync::atomic::Ordering::Relaxed), 42);
    }

    #[test]
    fn test_cpu_relax() {
        // Just ensure it doesn't panic
        for _ in 0..100 {
            cpu_relax();
        }
    }

    #[test]
    fn test_backoff() {
        let mut backoff = Backoff::new(5);

        assert!(!backoff.is_at_max());

        for _ in 0..5 {
            backoff.backoff();
        }

        assert!(backoff.is_at_max());

        backoff.reset();
        assert!(!backoff.is_at_max());
    }
    #[test]
    fn test_cache_aligned_methods() {
        let mut aligned = CacheAligned::new(42u64);

        assert_eq!(*aligned.get(), 42);
        *aligned.get_mut() = 100;
        assert_eq!(*aligned.get(), 100);

        let inner = aligned.into_inner();
        assert_eq!(inner, 100);
    }

    #[test]
    fn test_cache_aligned_default() {
        let aligned: CacheAligned<u64> = CacheAligned::default();
        assert_eq!(*aligned.get(), 0);
    }

    #[test]
    fn test_cache_aligned_clone() {
        let original = CacheAligned::new(42u64);
        let cloned = original.clone();
        assert_eq!(*cloned.get(), 42);
    }

    #[test]
    fn test_cache_aligned_debug() {
        let aligned = CacheAligned::new(42u64);
        let debug_str = format!("{:?}", aligned);
        assert_eq!(debug_str, "42");
    }

    #[test]
    fn test_backoff_progression() {
        let mut backoff = Backoff::new(3);

        assert!(!backoff.is_at_max());
        backoff.backoff(); // step 1
        backoff.backoff(); // step 2
        backoff.backoff(); // step 3
        assert!(backoff.is_at_max());

        // Should stay at max
        backoff.backoff();
        assert!(backoff.is_at_max());
    }

    #[test]
    fn test_time_monotonicity() {
        let mut last_ms = 0;
        let mut last_us = 0;
        let mut last_ns = 0;

        for _ in 0..10 {
            let ms = current_time_ms();
            let us = current_time_us();
            let ns = current_time_ns();

            assert!(ms >= last_ms);
            assert!(us >= last_us);
            assert!(ns >= last_ns);

            last_ms = ms;
            last_us = us;
            last_ns = ns;

            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn test_time_no_overflow() {
        let ms = current_time_ms();
        let us = current_time_us();
        let ns = current_time_ns();

        // None should be u64::MAX (overflow sentinel from saturating_mul)
        assert_ne!(ms, u64::MAX);
        assert_ne!(us, u64::MAX);
        assert_ne!(ns, u64::MAX);

        // All should be reasonable epoch values (after year 2020)
        assert!(ms > 1_577_836_800_000); // 2020-01-01 in ms
        assert!(us > 1_577_836_800_000_000); // 2020-01-01 in us
        assert!(ns > 1_577_836_800_000_000_000); // 2020-01-01 in ns
    }

    #[test]
    fn test_time_precision_relationships() {
        let ms = current_time_ms();
        let us = current_time_us();
        let ns = current_time_ns();

        // us should be roughly ms * 1000 (within a small delta for elapsed time between calls)
        let us_from_ms = ms * 1000;
        assert!(
            us >= us_from_ms && us < us_from_ms + 10_000,
            "us={} vs ms*1000={}",
            us,
            us_from_ms
        );

        // ns should be roughly us * 1000
        let ns_from_us = us * 1000;
        assert!(
            ns >= ns_from_us && ns < ns_from_us + 10_000_000,
            "ns={} vs us*1000={}",
            ns,
            ns_from_us
        );
    }

    #[test]
    fn test_time_elapsed_accuracy() {
        let before_ms = current_time_ms();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let after_ms = current_time_ms();

        let elapsed = after_ms - before_ms;
        assert!(
            (40..=100).contains(&elapsed),
            "Expected ~50ms elapsed, got {}ms",
            elapsed
        );
    }

    #[test]
    fn test_concurrent_time_calls() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;

        let max_ms = Arc::new(AtomicU64::new(0));
        let min_ms = Arc::new(AtomicU64::new(u64::MAX));
        let mut handles = vec![];

        for _ in 0..8 {
            let max = max_ms.clone();
            let min = min_ms.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let ms = current_time_ms();
                    max.fetch_max(ms, Ordering::Relaxed);
                    min.fetch_min(ms, Ordering::Relaxed);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let max_val = max_ms.load(Ordering::Relaxed);
        let min_val = min_ms.load(Ordering::Relaxed);

        // Under Miri / tarpaulin, threads run much slower so allow wider spread
        let threshold = if cfg!(miri) { 30_000 } else { 1_000 };
        assert!(
            max_val - min_val < threshold,
            "Time spread too large: {}ms",
            max_val - min_val
        );
    }

    #[test]
    fn test_cache_aligned_size() {
        use std::sync::atomic::AtomicU64;

        let size = std::mem::size_of::<CacheAligned<AtomicU64>>();
        assert!(
            size >= CACHE_LINE_SIZE,
            "CacheAligned should be at least {} bytes, got {}",
            CACHE_LINE_SIZE,
            size
        );
    }

    #[test]
    fn test_cache_aligned_alignment() {
        use std::sync::atomic::AtomicU64;

        let align = std::mem::align_of::<CacheAligned<AtomicU64>>();
        assert!(
            align >= CACHE_LINE_SIZE,
            "CacheAligned should have alignment of at least {} bytes, got {}",
            CACHE_LINE_SIZE,
            align
        );
    }

    #[test]
    fn test_backoff_yield_phase() {
        let mut backoff = Backoff::new(10);

        // Go past the spinning phase into yield phase
        for _ in 0..6 {
            backoff.backoff();
        }

        // Should not panic in yield phase
        backoff.backoff();
        backoff.backoff();
    }
}
