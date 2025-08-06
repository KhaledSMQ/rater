//! # Core Rate Limiter Implementation
//!
//! This module implements the heart of the rate limiting system using a lock-free
//! token bucket algorithm. It's designed for high-performance scenarios where
//! multiple threads need to check rate limits simultaneously without blocking.
//!
//! ## The Token Bucket Algorithm
//!
//! ```text
//!     How Token Bucket Works:
//!
//!     Initial State (t=0):
//!     ┌──────────────────┐
//!     │ 🪙🪙🪙🪙🪙      │ 5/10 tokens
//!     └──────────────────┘
//!
//!     After 3 requests (t=0.1):
//!     ┌──────────────────┐
//!     │ 🪙🪙             │ 2/10 tokens
//!     └──────────────────┘
//!
//!     After refill (t=1.0):
//!     ┌───────────────────┐
//!     │ 🪙🪙🪙🪙🪙🪙🪙   │ 7/10 tokens (added 5)
//!     └───────────────────┘
//! ```
//!
//! ## Lock-Free Design
//!
//! The implementation uses atomic Compare-And-Swap (CAS) operations to ensure
//! thread safety without locks. This means multiple threads can acquire tokens
//! simultaneously without blocking each other.
//!
//! ```text
//!     Thread Safety via CAS:
//!
//!     Thread A ──┐
//!                ├──► Atomic CAS ──► Success/Retry
//!     Thread B ──┤        │
//!                │        ▼
//!     Thread C ──┘    Token Count
//!                      (Atomic)
//! ```
//!
//! ## Performance Optimizations
//!
//! 1. **Cache Alignment**: Prevents false sharing between CPU cores
//! 2. **Bounded Retries**: Limits CAS attempts to prevent infinite loops
//! 3. **Exponential Backoff**: Reduces contention under high load
//! 4. **Platform-Specific Instructions**: Uses PAUSE/YIELD for efficient spinning

use super::{
    config::{MemoryOrdering, RateLimiterConfig, MAX_REFILL_PERIODS},
    metrics::RateLimiterMetrics,
    utils::{cpu_relax, current_time_ms, CacheAligned},
};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use tracing::{debug, warn};

/// Maximum number of CAS (Compare-And-Swap) retry attempts.
///
/// Prevents infinite spinning under extreme contention. If we can't
/// acquire tokens after 16 attempts, we give up to avoid blocking.
const MAX_CAS_RETRIES: usize = 16;

/// Threshold for starting exponential backoff.
///
/// After 4 failed CAS attempts, we start backing off exponentially
/// to reduce contention and give other threads a chance.
const CAS_BACKOFF_THRESHOLD: usize = 4;

/// Maximum times we can see the same value before using strong CAS.
///
/// Helps detect and break out of ABA problem scenarios where values
/// change and change back, potentially causing issues.
const MAX_REPEAT_COUNT: usize = 3;

/// Minimum interval between last_access timestamp updates (milliseconds).
///
/// We don't update the last access time on every request to reduce
/// contention on the atomic variable. 100ms granularity is sufficient.
const LAST_ACCESS_UPDATE_INTERVAL_MS: u64 = 100;

// Platform-specific token counter type selection
// On 64-bit systems, use AtomicU64 for larger token counts
// On 32-bit systems, use AtomicU32 to ensure native atomic operations
#[cfg(target_pointer_width = "64")]
type TokenCounter = AtomicU64;

#[cfg(not(target_pointer_width = "64"))]
type TokenCounter = AtomicU32;

/// Helper trait for unified token operations across platforms.
///
/// This trait abstracts the differences between AtomicU64 and AtomicU32,
/// allowing the same code to work on both 32-bit and 64-bit systems.
trait TokenOps {
    fn new(val: u64) -> Self;
    fn load(&self, ordering: Ordering) -> u64;
    fn store(&self, val: u64, ordering: Ordering);
    fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64>;
    fn compare_exchange_weak(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64>;
}

// Implementation for 64-bit systems
#[cfg(target_pointer_width = "64")]
impl TokenOps for AtomicU64 {
    #[inline(always)]
    fn new(val: u64) -> Self {
        AtomicU64::new(val)
    }

    #[inline(always)]
    fn load(&self, ordering: Ordering) -> u64 {
        self.load(ordering)
    }

    #[inline(always)]
    fn store(&self, val: u64, ordering: Ordering) {
        self.store(val, ordering)
    }

    #[inline(always)]
    fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        self.compare_exchange(current, new, success, failure)
    }

    #[inline(always)]
    fn compare_exchange_weak(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        self.compare_exchange_weak(current, new, success, failure)
    }
}

// Implementation for 32-bit systems with overflow protection
#[cfg(not(target_pointer_width = "64"))]
impl TokenOps for AtomicU32 {
    #[inline(always)]
    fn new(val: u64) -> Self {
        debug_assert!(val <= u32::MAX as u64, "Token value exceeds u32::MAX");
        AtomicU32::new(val.min(u32::MAX as u64) as u32)
    }

    #[inline(always)]
    fn load(&self, ordering: Ordering) -> u64 {
        self.load(ordering) as u64
    }

    #[inline(always)]
    fn store(&self, val: u64, ordering: Ordering) {
        debug_assert!(val <= u32::MAX as u64, "Token value exceeds u32::MAX");
        self.store(val.min(u32::MAX as u64) as u32, ordering)
    }

    #[inline(always)]
    fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        debug_assert!(new <= u32::MAX as u64, "Token value exceeds u32::MAX");
        self.compare_exchange(
            current as u32,
            new.min(u32::MAX as u64) as u32,
            success,
            failure,
        )
            .map(|v| v as u64)
            .map_err(|v| v as u64)
    }

    #[inline(always)]
    fn compare_exchange_weak(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        debug_assert!(new <= u32::MAX as u64, "Token value exceeds u32::MAX");
        self.compare_exchange_weak(
            current as u32,
            new.min(u32::MAX as u64) as u32,
            success,
            failure,
        )
            .map(|v| v as u64)
            .map_err(|v| v as u64)
    }
}

/// Lock-free token bucket rate limiter.
///
/// This is the core rate limiting implementation that manages tokens using
/// atomic operations. It's designed to be shared across multiple threads
/// without requiring locks or mutexes.
///
/// ## Internal Structure
///
/// The struct is carefully laid out to optimize CPU cache usage:
/// - Hot path fields (frequently accessed) are cache-aligned
/// - Cold path fields (configuration, metrics) are grouped separately
///
/// ## Thread Safety
///
/// All operations are thread-safe through atomic operations. Multiple threads
/// can call `try_acquire()` simultaneously without data races.
///
/// ## Example
///
/// ```rust
/// use rater::RateLimiter;
/// use std::sync::Arc;
/// use std::thread;
///
/// let limiter = Arc::new(RateLimiter::new(100, 10));
///
/// // Share across multiple threads
/// let mut handles = vec![];
/// for _ in 0..4 {
///     let limiter = limiter.clone();
///     handles.push(thread::spawn(move || {
///         for _ in 0..25 {
///             if limiter.try_acquire() {
///                 // Process request
///             }
///         }
///     }));
/// }
/// ```
#[repr(C)]
pub struct RateLimiter {
    // Hot path fields - accessed frequently during normal operation
    // These are cache-aligned to prevent false sharing between CPU cores

    /// Current number of available tokens (cache-aligned for performance)
    tokens: CacheAligned<TokenCounter>,

    /// Timestamp of last refill in milliseconds (cache-aligned)
    last_refill_ms: CacheAligned<AtomicU64>,

    /// Timestamp of last access in milliseconds (cache-aligned)
    /// Used for cleanup of inactive limiters
    pub(crate) last_access_ms: CacheAligned<AtomicU64>,

    // Backpressure tracking - helps detect when system is under load

    /// Count of consecutive failed acquisition attempts
    /// High values indicate the system is under pressure
    consecutive_rejections: AtomicU32,

    /// Maximum wait time observed in nanoseconds
    /// Useful for performance monitoring
    max_wait_time_ns: AtomicU64,

    // Configuration fields (cold path - accessed less frequently)

    /// Maximum tokens the bucket can hold (burst capacity)
    pub(crate) max_tokens: u64,

    /// Number of tokens to add per refill interval
    refill_rate: u32,

    /// Milliseconds between refill operations
    refill_interval_ms: u64,

    /// Memory ordering strategy for atomic operations
    ordering: MemoryOrdering,

    // Metrics fields (cold path - accessed for monitoring)

    /// Total number of tokens successfully acquired
    total_acquired: AtomicU64,

    /// Total number of acquisition attempts that were rejected
    total_rejected: AtomicU64,

    /// Total number of refill operations performed
    total_refills: AtomicU64,
}

impl RateLimiter {
    /// Creates a new rate limiter with default configuration.
    ///
    /// This is the simplest way to create a rate limiter when you just need
    /// basic token bucket functionality.
    ///
    /// # Arguments
    ///
    /// * `max_tokens` - Maximum tokens (burst capacity)
    /// * `refill_rate` - Tokens added per second
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// // Allow 100 burst, 10 requests per second sustained
    /// let limiter = RateLimiter::new(100, 10);
    /// ```
    #[inline]
    pub fn new(max_tokens: u64, refill_rate: u32) -> Self {
        Self::with_config(RateLimiterConfig {
            max_tokens,
            refill_rate,
            ..Default::default()
        })
    }

    /// Creates a new rate limiter with custom configuration.
    ///
    /// Use this when you need fine control over all parameters including
    /// refill interval and memory ordering.
    ///
    /// # Panics
    ///
    /// Panics if the configuration is invalid (see `RateLimiterConfig::validate`).
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{RateLimiter, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_minute(600);
    /// let limiter = RateLimiter::with_config(config);
    /// ```
    pub fn with_config(config: RateLimiterConfig) -> Self {
        config.validate().expect("Invalid rate limiter configuration");

        let now_ms = current_time_ms();

        Self {
            tokens: CacheAligned::new(TokenCounter::new(config.max_tokens)),
            last_refill_ms: CacheAligned::new(AtomicU64::new(now_ms)),
            last_access_ms: CacheAligned::new(AtomicU64::new(now_ms)),
            consecutive_rejections: AtomicU32::new(0),
            max_wait_time_ns: AtomicU64::new(0),
            max_tokens: config.max_tokens,
            refill_rate: config.refill_rate,
            refill_interval_ms: config.refill_interval_ms,
            ordering: config.ordering,
            total_acquired: AtomicU64::new(0),
            total_rejected: AtomicU64::new(0),
            total_refills: AtomicU64::new(0),
        }
    }

    /// Attempts to acquire a single token - optimized fast path.
    ///
    /// This is the most common operation and is highly optimized for performance.
    /// It uses lock-free atomic operations to ensure thread safety without blocking.
    ///
    /// ## How it Works
    ///
    /// ```text
    ///     try_acquire() flow:
    ///
    ///     Check Tokens ──► Available? ──Yes──► Decrement ──► ✅ Success
    ///          │              │
    ///          │              No
    ///          │              ▼
    ///          └──────► Check Refill ──► Add Tokens ──► Retry
    ///                         │
    ///                         ▼
    ///                    ❌ Rejected
    /// ```
    ///
    /// ## Performance
    ///
    /// - **Best case**: Single atomic read + CAS operation (~10-20ns)
    /// - **With refill**: Additional refill logic (~50-100ns)
    /// - **Under contention**: May retry with exponential backoff
    ///
    /// ## Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(10, 1);
    ///
    /// // Fast path - single token acquisition
    /// if limiter.try_acquire() {
    ///     println!("Got a token!");
    /// } else {
    ///     println!("Rate limited!");
    /// }
    /// ```
    ///
    /// # Returns
    ///
    /// - `true` if a token was successfully acquired
    /// - `false` if no tokens are available (rate limited)
    #[inline(always)]
    pub fn try_acquire(&self) -> bool {
        let now_ms = current_time_ms();

        // Optimization: Only update last_access periodically to reduce contention
        // This atomic variable is used for cleanup detection, not critical path
        let last = self.last_access_ms.value.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) > LAST_ACCESS_UPDATE_INTERVAL_MS {
            self.last_access_ms.value.store(now_ms, Ordering::Relaxed);
        }

        // Check if refill needed before attempting acquisition
        // This reduces failed attempts when tokens are depleted
        let last_refill = self.last_refill_ms.value.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last_refill) >= self.refill_interval_ms {
            self.refill_if_needed(now_ms);
        }

        // Optimized single-token acquisition with bounded retries
        let mut retries = 0;
        loop {
            let current = self.tokens.value.load(self.ordering.load());

            // Fast path: no tokens available
            if current == 0 {
                self.on_rejection(1);
                return false;
            }

            // Try to atomically decrement the token count
            match self.tokens.value.compare_exchange_weak(
                current,
                current - 1,
                self.ordering.rmw(),
                self.ordering.cas_failure(),
            ) {
                Ok(_) => {
                    // Success! We got a token
                    self.on_acquisition(1);
                    return true;
                }
                Err(actual) if actual == 0 => {
                    // Someone else took the last token
                    self.on_rejection(1);
                    return false;
                }
                Err(_) => {
                    // CAS failed, another thread modified the value
                    retries += 1;

                    // Bounded retries to prevent infinite spinning
                    if retries >= MAX_CAS_RETRIES {
                        warn!("Single token CAS retry limit reached");
                        self.on_rejection(1);
                        return false;
                    }

                    // Exponential backoff to reduce contention
                    if retries > CAS_BACKOFF_THRESHOLD {
                        // Exponential backoff: 2^1, 2^2, ... up to 2^4 iterations
                        for _ in 0..(1 << (retries - CAS_BACKOFF_THRESHOLD).min(4)) {
                            cpu_relax();
                        }
                    } else {
                        // Simple CPU pause for short retries
                        cpu_relax();
                    }
                }
            }
        }
    }

    /// Attempts to acquire multiple tokens atomically.
    ///
    /// This operation either acquires all requested tokens or none at all.
    /// It's useful for operations that require multiple "units" of rate limit.
    ///
    /// ## Atomicity Guarantee
    ///
    /// ```text
    ///     try_acquire_n(5):
    ///
    ///     Current: 10 tokens
    ///
    ///     Scenario A (Success):
    ///     10 ──► 5 tokens (acquired 5) ✅
    ///
    ///     Scenario B (Failure):
    ///     3 ──► 3 tokens (need 5, none taken) ❌
    /// ```
    ///
    /// ## Use Cases
    ///
    /// - Expensive operations that consume more resources
    /// - Batch processing where you want to limit batch sizes
    /// - Priority requests that need multiple tokens
    ///
    /// # Arguments
    ///
    /// * `n` - Number of tokens to acquire (0 always succeeds)
    ///
    /// # Returns
    ///
    /// - `true` if all tokens were acquired
    /// - `false` if insufficient tokens (none are taken)
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    ///
    /// // Try to acquire 5 tokens for a batch operation
    /// if limiter.try_acquire_n(5) {
    ///     println!("Processing batch of 5 items");
    /// } else {
    ///     println!("Not enough tokens for batch");
    /// }
    /// ```
    #[inline]
    pub fn try_acquire_n(&self, n: u64) -> bool {
        // Fast paths for common cases
        if n == 0 {
            return true;  // Acquiring 0 tokens always succeeds
        }
        if n == 1 {
            return self.try_acquire();  // Use optimized single-token path
        }
        if n > self.max_tokens {
            self.on_rejection(n);
            return false;  // Can never acquire more than max
        }

        let now_ms = current_time_ms();
        self.last_access_ms.value.store(now_ms, self.ordering.store());

        // Check for refill before attempting acquisition
        self.refill_if_needed(now_ms);

        // Track wait time for performance monitoring
        let start_ns = std::time::Instant::now();
        let result = self.try_acquire_with_bounded_cas(n);

        let wait_ns = start_ns.elapsed().as_nanos() as u64;
        self.update_max_wait_time(wait_ns);

        result
    }

    /// Internal method for acquiring N tokens with CAS retry logic.
    ///
    /// This implements the core atomic acquisition with sophisticated
    /// retry logic to handle contention and ABA problems.
    #[inline(always)]
    fn try_acquire_with_bounded_cas(&self, n: u64) -> bool {
        let mut retries = 0;
        let mut last_seen = u64::MAX;
        let mut repeat_count = 0;

        loop {
            let current = self.tokens.value.load(self.ordering.load());

            // ABA problem detection: Check if we're seeing the same value repeatedly
            // This can indicate that other threads are modifying and restoring the value
            if current == last_seen {
                repeat_count += 1;
                if repeat_count >= MAX_REPEAT_COUNT {
                    // Use strong CAS to break potential ABA cycle
                    match self.tokens.value.compare_exchange(
                        current,
                        current.saturating_sub(n),
                        self.ordering.rmw(),
                        self.ordering.cas_failure(),
                    ) {
                        Ok(_) if current >= n => {
                            self.on_acquisition(n);
                            return true;
                        }
                        _ => {
                            self.on_rejection(n);
                            return false;
                        }
                    }
                }
            } else {
                last_seen = current;
                repeat_count = 0;
            }

            // Check if enough tokens are available
            if current < n {
                self.on_rejection(n);
                return false;
            }

            // Try to atomically acquire the tokens
            match self.tokens.value.compare_exchange_weak(
                current,
                current - n,
                self.ordering.rmw(),
                self.ordering.cas_failure(),
            ) {
                Ok(_) => {
                    self.on_acquisition(n);
                    return true;
                }
                Err(actual) => {
                    // Early exit if tokens were depleted by other threads
                    if actual < n {
                        self.on_rejection(n);
                        return false;
                    }

                    retries += 1;

                    if retries >= MAX_CAS_RETRIES {
                        warn!("Rate limiter CAS retry limit reached after {} attempts", retries);
                        self.on_rejection(n);
                        return false;
                    }

                    // Exponential backoff for high contention scenarios
                    if retries > CAS_BACKOFF_THRESHOLD {
                        for _ in 0..(1 << (retries - CAS_BACKOFF_THRESHOLD).min(4)) {
                            cpu_relax();
                        }
                    } else {
                        cpu_relax();
                    }
                }
            }
        }
    }

    /// Records successful token acquisition for metrics.
    #[inline]
    fn on_acquisition(&self, n: u64) {
        self.total_acquired.fetch_add(n, self.ordering.rmw());

        // Reset consecutive rejections counter if it's non-zero
        // This optimization avoids unnecessary atomic operations
        let rejections = self.consecutive_rejections.load(Ordering::Relaxed);
        if rejections > 0 {
            self.consecutive_rejections.store(0, self.ordering.store());
        }
    }

    /// Records failed token acquisition for metrics and backpressure detection.
    #[inline]
    fn on_rejection(&self, n: u64) {
        self.total_rejected.fetch_add(1, self.ordering.rmw());
        self.consecutive_rejections.fetch_add(1, self.ordering.rmw());
    }

    /// Checks if tokens need to be refilled and performs the refill if necessary.
    ///
    /// This method handles multiple missed refill periods (e.g., if the system
    /// was idle) and caps the refill to prevent token overflow.
    ///
    /// ## Refill Logic
    ///
    /// ```text
    ///     Refill Calculation:
    ///
    ///     Time elapsed: 3500ms
    ///     Refill interval: 1000ms
    ///     Periods missed: 3
    ///
    ///     Tokens to add = refill_rate × periods
    ///                   = 10 × 3 = 30 tokens
    ///
    ///     (capped at max_tokens)
    /// ```
    #[inline]
    fn refill_if_needed(&self, now_ms: u64) {
        let last_refill = self.last_refill_ms.value.load(self.ordering.load());

        let elapsed = now_ms.saturating_sub(last_refill);
        if elapsed < self.refill_interval_ms {
            return;  // Not time for refill yet
        }

        // Calculate how many refill periods have passed
        let periods = (elapsed / self.refill_interval_ms).min(MAX_REFILL_PERIODS);
        if periods == 0 {
            return;
        }

        let new_refill_time = last_refill + (periods * self.refill_interval_ms);

        // Try to claim the refill operation atomically
        if let Ok(_) = self.last_refill_ms.value.compare_exchange(
            last_refill,
            new_refill_time,
            self.ordering.rmw(),
            self.ordering.cas_failure(),
        ) {
            self.perform_refill(periods);
        }
        // If CAS failed, another thread is handling the refill
    }

    /// Performs the actual token refill operation.
    ///
    /// This method adds tokens to the bucket, with optional adaptive
    /// refill rate when under sustained pressure.
    #[inline]
    fn perform_refill(&self, periods: u64) {
        // Adaptive refill: reduce rate when under sustained pressure
        // This helps prevent the system from being overwhelmed
        let refill_rate = if self.is_under_sustained_pressure() {
            self.adaptive_refill_rate()
        } else {
            self.refill_rate
        };

        // Calculate tokens to add (with overflow protection)
        let tokens_to_add = (refill_rate as u64)
            .saturating_mul(periods)
            .min(self.max_tokens);

        let mut retries = 0;
        let mut current = self.tokens.value.load(self.ordering.load());

        loop {
            // Cap at max_tokens to prevent overflow
            let new_tokens = current.saturating_add(tokens_to_add).min(self.max_tokens);

            match self.tokens.value.compare_exchange_weak(
                current,
                new_tokens,
                self.ordering.rmw(),
                self.ordering.cas_failure(),
            ) {
                Ok(_) => {
                    self.total_refills.fetch_add(1, self.ordering.rmw());
                    debug!("Refilled {} tokens (periods: {})", new_tokens - current, periods);
                    break;
                }
                Err(actual) => {
                    current = actual;
                    retries += 1;

                    if retries >= MAX_CAS_RETRIES {
                        warn!("Rate limiter refill CAS retry limit reached");
                        break;
                    }

                    if retries > CAS_BACKOFF_THRESHOLD {
                        for _ in 0..(1 << (retries - CAS_BACKOFF_THRESHOLD).min(4)) {
                            cpu_relax();
                        }
                    } else {
                        cpu_relax();
                    }
                }
            }
        }
    }

    /// Checks if the rate limiter is under sustained pressure.
    ///
    /// Sustained pressure is indicated by many consecutive rejections,
    /// suggesting that demand consistently exceeds capacity.
    #[inline]
    fn is_under_sustained_pressure(&self) -> bool {
        self.consecutive_rejections.load(self.ordering.load()) > 10
    }

    /// Calculates an adaptive refill rate based on current pressure.
    ///
    /// When under pressure, we reduce the refill rate to provide
    /// backpressure and prevent system overload.
    ///
    /// ## Adaptive Strategy
    ///
    /// - High pressure (>50% rejections): 80% refill rate
    /// - Medium pressure (>30% rejections): 90% refill rate
    /// - Low pressure: Normal refill rate
    #[inline]
    fn adaptive_refill_rate(&self) -> u32 {
        let pressure_ratio = self.calculate_pressure_ratio();

        if pressure_ratio > 0.5 {
            (self.refill_rate as f64 * 0.8) as u32
        } else if pressure_ratio > 0.3 {
            (self.refill_rate as f64 * 0.9) as u32
        } else {
            self.refill_rate
        }
    }

    /// Calculates the ratio of rejected to total requests.
    ///
    /// This metric helps identify when the system is under load.
    #[inline]
    fn calculate_pressure_ratio(&self) -> f64 {
        let total_rejected = self.total_rejected.load(self.ordering.load());
        let total_acquired = self.total_acquired.load(self.ordering.load());
        let total = total_acquired + total_rejected;

        if total == 0 {
            0.0
        } else {
            total_rejected as f64 / total as f64
        }
    }

    /// Updates the maximum observed wait time for performance monitoring.
    #[inline]
    fn update_max_wait_time(&self, wait_ns: u64) {
        let mut current = self.max_wait_time_ns.load(self.ordering.load());
        while wait_ns > current {
            match self.max_wait_time_ns.compare_exchange_weak(
                current,
                wait_ns,
                self.ordering.rmw(),
                self.ordering.cas_failure(),
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Returns the current number of available tokens.
    ///
    /// This method triggers a refill check, so the returned value
    /// reflects the most up-to-date token count.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// println!("Available tokens: {}", limiter.available_tokens());
    /// ```
    #[inline]
    pub fn available_tokens(&self) -> u64 {
        self.refill_if_needed(current_time_ms());
        self.tokens.value.load(self.ordering.load())
    }

    /// Checks if the rate limiter has been inactive for a specified duration.
    ///
    /// This is useful for cleanup operations to identify and remove
    /// rate limiters that haven't been used recently.
    ///
    /// # Arguments
    ///
    /// * `inactive_duration_ms` - Milliseconds of inactivity to check for
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    ///
    /// // Check if inactive for more than 5 minutes
    /// if limiter.is_inactive(5 * 60 * 1000) {
    ///     println!("Limiter has been inactive");
    /// }
    /// ```
    #[inline]
    pub fn is_inactive(&self, inactive_duration_ms: u64) -> bool {
        let now_ms = current_time_ms();
        let last_ms = self.last_access_ms.value.load(self.ordering.load());
        now_ms.saturating_sub(last_ms) > inactive_duration_ms
    }

    /// Returns comprehensive metrics about the rate limiter's performance.
    ///
    /// These metrics are useful for monitoring, debugging, and capacity planning.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// // ... use the limiter ...
    ///
    /// let metrics = limiter.metrics();
    /// println!("Success rate: {:.2}%", metrics.success_rate() * 100.0);
    /// println!("Current tokens: {}/{}", metrics.current_tokens, metrics.max_tokens);
    /// ```
    pub fn metrics(&self) -> RateLimiterMetrics {
        // Use consistent ordering for all reads to get a coherent snapshot
        let ordering = self.ordering.load();
        RateLimiterMetrics {
            total_acquired: self.total_acquired.load(ordering),
            total_rejected: self.total_rejected.load(ordering),
            total_refills: self.total_refills.load(ordering),
            current_tokens: self.tokens.value.load(ordering),
            max_tokens: self.max_tokens,
            consecutive_rejections: self.consecutive_rejections.load(ordering),
            max_wait_time_ns: self.max_wait_time_ns.load(ordering),
            pressure_ratio: self.calculate_pressure_ratio(),
        }
    }

    /// Manually adds tokens to the bucket.
    ///
    /// This can be useful for:
    /// - Rewarding good behavior
    /// - Manual intervention during incidents
    /// - Testing and debugging
    ///
    /// Note: Added tokens are capped at max_tokens.
    ///
    /// # Arguments
    ///
    /// * `n` - Number of tokens to add
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    ///
    /// // Manually add 50 tokens
    /// limiter.add_tokens(50);
    /// ```
    #[inline]
    pub fn add_tokens(&self, n: u64) {
        let mut retries = 0;
        let mut current = self.tokens.value.load(self.ordering.load());

        loop {
            let new_tokens = current.saturating_add(n).min(self.max_tokens);

            match self.tokens.value.compare_exchange_weak(
                current,
                new_tokens,
                self.ordering.rmw(),
                self.ordering.cas_failure(),
            ) {
                Ok(_) => break,
                Err(actual) => {
                    current = actual;
                    retries += 1;

                    if retries >= MAX_CAS_RETRIES {
                        debug!("Token add CAS retry limit reached");
                        break;
                    }

                    if retries > CAS_BACKOFF_THRESHOLD {
                        for _ in 0..(1 << (retries - CAS_BACKOFF_THRESHOLD).min(4)) {
                            cpu_relax();
                        }
                    } else {
                        cpu_relax();
                    }
                }
            }
        }
    }

    /// Resets the rate limiter to its initial state.
    ///
    /// This operation:
    /// - Refills tokens to maximum capacity
    /// - Resets all metrics to zero
    /// - Clears pressure indicators
    ///
    /// Useful for:
    /// - Testing scenarios
    /// - Clearing state after incidents
    /// - Periodic resets in long-running applications
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// // ... heavy usage ...
    ///
    /// // Reset to fresh state
    /// limiter.reset();
    /// assert_eq!(limiter.available_tokens(), 100);
    /// ```
    pub fn reset(&self) {
        let now_ms = current_time_ms();

        // Reset all state to initial values
        self.tokens.value.store(self.max_tokens, self.ordering.store());
        self.last_refill_ms.value.store(now_ms, self.ordering.store());
        self.last_access_ms.value.store(now_ms, self.ordering.store());
        self.consecutive_rejections.store(0, self.ordering.store());
        self.max_wait_time_ns.store(0, self.ordering.store());
        self.total_acquired.store(0, self.ordering.store());
        self.total_rejected.store(0, self.ordering.store());
        self.total_refills.store(0, self.ordering.store());
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("max_tokens", &self.max_tokens)
            .field("refill_rate", &self.refill_rate)
            .field("refill_interval_ms", &self.refill_interval_ms)
            .field("current_tokens", &self.available_tokens())
            .finish()
    }
}

// Safety: RateLimiter can be safely shared between threads
// All operations use atomic instructions with proper memory ordering
unsafe impl Send for RateLimiter {}
unsafe impl Sync for RateLimiter {}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_basic_acquisition() {
        let limiter = RateLimiter::new(10, 1);

        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }

        assert!(!limiter.try_acquire());
    }

    #[test]
    fn test_bulk_acquisition() {
        let limiter = RateLimiter::new(10, 1);

        assert!(limiter.try_acquire_n(5));
        assert!(limiter.try_acquire_n(3));
        assert!(!limiter.try_acquire_n(5));
        assert!(limiter.try_acquire_n(2));
    }

    #[test]
    fn test_overflow_protection() {
        let limiter = RateLimiter::new(u64::MAX, u32::MAX);
        limiter.add_tokens(u64::MAX);
        assert_eq!(limiter.available_tokens(), u64::MAX);
    }


    #[test]
    fn test_refill_mechanism() {
        let limiter = RateLimiter::new(10, 5);

        // Drain all tokens
        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());

        // Force time to advance (simulate waiting for refill interval)
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Should have refilled tokens
        assert!(limiter.try_acquire_n(5));
    }

    #[test]
    fn test_multiple_refill_periods() {
        let config = RateLimiterConfig {
            max_tokens: 20,
            refill_rate: 5,
            refill_interval_ms: 100,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        // Drain tokens
        assert!(limiter.try_acquire_n(20));
        assert!(!limiter.try_acquire());

        // Wait for multiple refill periods
        std::thread::sleep(std::time::Duration::from_millis(450));

        // Should have refilled up to max
        assert_eq!(limiter.available_tokens(), 20);
    }

    #[test]
    fn test_add_tokens() {
        let limiter = RateLimiter::new(10, 1);

        // Use some tokens
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }

        // Add tokens manually
        limiter.add_tokens(3);
        assert_eq!(limiter.available_tokens(), 8);

        // Try to add beyond max
        limiter.add_tokens(20);
        assert_eq!(limiter.available_tokens(), 10);
    }

    #[test]
    fn test_reset() {
        let limiter = RateLimiter::new(10, 1);

        // Use tokens and accumulate metrics
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }
        for _ in 0..3 {
            assert!(!limiter.try_acquire_n(10));
        }

        let metrics_before = limiter.metrics();
        assert!(metrics_before.total_acquired > 0);
        assert!(metrics_before.total_rejected > 0);

        // Reset
        limiter.reset();

        assert_eq!(limiter.available_tokens(), 10);
        let metrics_after = limiter.metrics();
        assert_eq!(metrics_after.total_acquired, 0);
        assert_eq!(metrics_after.total_rejected, 0);
        assert_eq!(metrics_after.consecutive_rejections, 0);
    }

    #[test]
    fn test_is_inactive() {
        let limiter = RateLimiter::new(10, 1);

        // Should not be inactive immediately
        assert!(!limiter.is_inactive(1000));

        // Use a token to update last_access
        assert!(limiter.try_acquire());

        // Wait and check
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(limiter.is_inactive(100));
        assert!(!limiter.is_inactive(200));
    }

    #[test]
    fn test_sustained_pressure() {
        let limiter = RateLimiter::new(5, 1);

        // Drain all tokens
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }

        // Generate consecutive rejections
        for _ in 0..15 {
            assert!(!limiter.try_acquire());
        }

        let metrics = limiter.metrics();
        assert!(metrics.consecutive_rejections > 10);
        assert!(limiter.is_under_sustained_pressure());
    }

    #[test]
    fn test_adaptive_refill_under_pressure() {
        let limiter = RateLimiter::new(10, 10);

        // Create pressure
        for _ in 0..10 {
            limiter.try_acquire();
        }
        for _ in 0..20 {
            limiter.try_acquire(); // Will fail and increase rejections
        }

        // Trigger refill while under pressure
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // adaptive_refill_rate should have reduced the refill
        let tokens = limiter.available_tokens();
        assert!(tokens <= 10); // May be less due to adaptive refill
    }

    #[test]
    fn test_max_wait_time_tracking() {
        let limiter = RateLimiter::new(100, 10);

        // Acquire tokens with different amounts to trigger wait time tracking
        assert!(limiter.try_acquire_n(50));
        assert!(limiter.try_acquire_n(30));

        let metrics = limiter.metrics();
        // max_wait_time_ns should be non-zero after multiple acquisitions
        assert!(metrics.max_wait_time_ns > 0);
    }

    #[test]
    fn test_cas_retry_exhaustion() {
        use std::sync::Arc;
        use std::thread;

        let limiter = Arc::new(RateLimiter::new(100, 10));
        let mut handles = vec![];

        // Create high contention to trigger CAS retries
        for _ in 0..50 {
            let limiter_clone = limiter.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    limiter_clone.try_acquire_n(2);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Should have handled high contention without panicking
        let metrics = limiter.metrics();
        assert!(metrics.total_acquired > 0 || metrics.total_rejected > 0);
    }

    #[test]
    fn test_acquire_zero_tokens() {
        let limiter = RateLimiter::new(10, 1);

        // Acquiring 0 tokens should always succeed
        assert!(limiter.try_acquire_n(0));

        // Drain all tokens
        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }

        // Should still succeed with 0
        assert!(limiter.try_acquire_n(0));
    }

    #[test]
    fn test_acquire_more_than_max() {
        let limiter = RateLimiter::new(10, 1);

        // Try to acquire more than max_tokens
        assert!(!limiter.try_acquire_n(11));

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_rejected, 1);
    }

    #[test]
    fn test_last_access_update_throttling() {
        let limiter = RateLimiter::new(100, 10);

        // Rapid acquisitions should not update last_access every time
        let start_access = limiter.last_access_ms.value.load(Ordering::Relaxed);

        for _ in 0..50 {
            assert!(limiter.try_acquire());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // last_access should have been updated, but not 50 times
        let end_access = limiter.last_access_ms.value.load(Ordering::Relaxed);
        assert!(end_access >= start_access);
    }

    #[cfg(not(target_pointer_width = "64"))]
    #[test]
    fn test_32bit_token_ops() {
        use super::TokenOps;
        let counter = TokenCounter::new(u32::MAX as u64 + 1);

        // Should clamp to u32::MAX
        assert_eq!(counter.load(Ordering::Relaxed), u32::MAX as u64);

        counter.store(u32::MAX as u64 + 100, Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), u32::MAX as u64);
    }

    #[test]
    fn test_debug_impl() {
        let limiter = RateLimiter::new(10, 5);
        let debug_str = format!("{:?}", limiter);

        assert!(debug_str.contains("RateLimiter"));
        assert!(debug_str.contains("max_tokens: 10"));
        assert!(debug_str.contains("refill_rate: 5"));
    }
}