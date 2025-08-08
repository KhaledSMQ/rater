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

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// Architecture-specific cache line sizes
// These values are critical for preventing false sharing between CPU cores

/// Cache line size for x86_64 processors (Intel/AMD).
///
/// Most modern x86_64 CPUs use 64-byte cache lines.
#[cfg(target_arch = "x86_64")]
pub const CACHE_LINE_SIZE: usize = 64;

/// Cache line size for ARM64 processors.
///
/// Many ARM processors use 128-byte cache lines for better performance.
#[cfg(target_arch = "aarch64")]
pub const CACHE_LINE_SIZE: usize = 128;

// Monotonic time base to prevent issues when the system clock jumps.
// We capture the wall-clock epoch milliseconds at process start,
// then advance using a monotonic Instant to compute 'now'.
static START_TIME_BASE: OnceLock<(Instant, u64)> = OnceLock::new();

/// Default cache line size for other architectures.
///
/// We assume 64 bytes as a reasonable default.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const CACHE_LINE_SIZE: usize = 64;

/// CPU-specific relaxation hint for spin loops.
///
/// This function tells the CPU that we're in a spin loop, allowing it to:
/// - Reduce power consumption
/// - Give resources to other threads
/// - Avoid memory order violations in spin loops
///
/// ## Platform Behavior
///
/// - **x86_64**: Uses PAUSE instruction (reduces power, improves performance)
/// - **ARM64**: Uses YIELD instruction (hints to give up time slice)
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
    #[cfg(target_arch = "x86_64")]
    {
        #[cfg(any(target_feature = "sse2", target_feature = "sse"))]
        unsafe {
            std::arch::x86_64::_mm_pause();
        }
        #[cfg(not(any(target_feature = "sse2", target_feature = "sse")))]
        {
            std::hint::spin_loop();
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::hint::spin_loop();
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        std::hint::spin_loop();
    }
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
    let (start, base_ms) = START_TIME_BASE.get_or_init(|| {
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        (Instant::now(), epoch_ms)
    });
    base_ms.saturating_add(start.elapsed().as_millis() as u64)
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
    let (start, base_ms) = START_TIME_BASE.get_or_init(|| {
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        (Instant::now(), epoch_ms)
    });
    base_ms
        .saturating_mul(1000)
        .saturating_add(start.elapsed().as_micros() as u64)
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
    let (start, base_ms) = START_TIME_BASE.get_or_init(|| {
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        (Instant::now(), epoch_ms)
    });
    base_ms
        .saturating_mul(1_000_000)
        .saturating_add(start.elapsed().as_nanos() as u64)
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
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// Gets a reference to the inner value.
    #[inline(always)]
    pub fn get(&self) -> &T {
        &self.0
    }

    /// Gets a mutable reference to the inner value.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.0
    }

    /// Consumes the wrapper and returns the inner value.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.0
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

/// Exponential backoff helper for retry operations.
///
/// Implements an exponential backoff strategy to reduce contention
/// in high-concurrency scenarios. This is more efficient than
/// constant retrying or sleeping.
///
/// ## Backoff Strategy
///
/// ```text
///     Retry attempts and backoff:
///     
///     Attempt 1: No wait
///     Attempt 2: Spin 2 times
///     Attempt 3: Spin 4 times
///     Attempt 4: Spin 8 times
///     Attempt 5+: Yield to scheduler
/// ```
///
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    /// Current backoff step (increases with each retry)
    step: u32,
    /// Maximum step before giving up
    max_step: u32,
}

impl Backoff {
    /// Creates a new backoff helper with specified maximum steps.
    pub fn new(max_step: u32) -> Self {
        Self { step: 0, max_step }
    }

    /// Performs backoff with increasing delay.
    ///
    /// The delay increases exponentially with each call:
    /// - Steps 0-3: Spin with cpu_relax() for 2^step iterations
    /// - Steps 4+: Yield to the OS scheduler
    #[inline]
    pub fn backoff(&mut self) {
        if self.step < 4 {
            // Exponential spinning: 1, 2, 4, 8 iterations
            for _ in 0..(1 << self.step) {
                cpu_relax();
            }
        } else {
            // After 4 attempts, yield to scheduler for longer waits
            std::thread::yield_now();
        }
        self.step = (self.step + 1).min(self.max_step);
    }

    /// Resets the backoff counter to start over.
    #[inline]
    pub fn reset(&mut self) {
        self.step = 0;
    }

    /// Checks if we've reached the maximum backoff level.
    #[inline]
    pub fn is_at_max(&self) -> bool {
        self.step >= self.max_step
    }
}

/// Helper for likely branch hints (when available).
///
/// Hints to the compiler that a branch is likely to be taken.
/// On stable Rust, this is a no-op, but the compiler's branch
/// predictor is usually good enough.
///
#[inline(always)]
pub(crate) fn likely(b: bool) -> bool {
    // On nightly with intrinsics, we could use core::intrinsics::likely
    // For stable Rust, the compiler's branch predictor handles it
    b
}

/// Helper for unlikely branch hints (when available).
///
/// Hints to the compiler that a branch is unlikely to be taken.
/// Useful for error paths and exceptional cases.
#[inline(always)]
pub(crate) fn unlikely(b: bool) -> bool {
    // On nightly with intrinsics, we could use core::intrinsics::unlikely
    // For stable Rust, the compiler's branch predictor handles it
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_line_size() {
        assert!(CACHE_LINE_SIZE >= 32);
        assert!(CACHE_LINE_SIZE <= 256);
        assert!(CACHE_LINE_SIZE.is_power_of_two());
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
    fn test_likely_unlikely() {
        // Just ensure they work and return the value
        assert!(likely(true));
        assert!(!likely(false));
        assert!(unlikely(true));
        assert!(!unlikely(false));
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
}
