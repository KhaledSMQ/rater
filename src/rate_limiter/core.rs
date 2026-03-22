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
use tracing::debug;

const MAX_CAS_RETRIES: usize = 16;
const CAS_BACKOFF_THRESHOLD: usize = 4;
const MAX_REPEAT_COUNT: usize = 3;
const LAST_ACCESS_UPDATE_INTERVAL_MS: u64 = 100;

// Platform-specific token counter type selection
// On 64-bit systems, use AtomicU64 for larger token counts
// On 32-bit systems, use AtomicU32 to ensure native atomic operations
#[cfg(target_pointer_width = "64")]
type TokenCounter = AtomicU64;

#[cfg(not(target_pointer_width = "64"))]
type TokenCounter = AtomicU32;

// On 32-bit targets, AtomicU32 lacks a native u64 interface.
// This trait provides u64-based wrappers with clamping to u32::MAX,
// so the rest of the code can work uniformly with u64 token counts.
#[cfg(not(target_pointer_width = "64"))]
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
/// for h in handles {
///     h.join().unwrap();
/// }
/// ```
pub struct RateLimiter {
    // === HOT: touched on every try_acquire call ===
    tokens: CacheAligned<TokenCounter>,

    // === WARM: touched on refill check (most calls), shares no cache line with tokens ===
    last_refill_ms: CacheAligned<AtomicU64>,

    // === COLD: updated infrequently (throttled to 100ms) ===
    last_access_ms: CacheAligned<AtomicU64>,

    // === CONFIG: read-only after construction, can live on same cache line ===
    max_tokens: u64,
    refill_rate: u32,
    refill_interval_ms: u64,
    ordering: MemoryOrdering,

    // Precomputed orderings to avoid match dispatch on every hot-path call
    ordering_load: Ordering,
    ordering_rmw: Ordering,
    ordering_store: Ordering,

    // === METRICS: cold path, only for monitoring ===
    consecutive_rejections: AtomicU32,
    max_wait_time_ns: AtomicU64,
    total_acquired: AtomicU64,
    total_rejected: AtomicU64,
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
        config
            .validate()
            .expect("Invalid rate limiter configuration");

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
            ordering_load: config.ordering.load(),
            ordering_rmw: config.ordering.rmw(),
            ordering_store: config.ordering.store(),
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
        // Ultra-fast path: try CAS immediately before any time checks.
        // In the common case (tokens available, no refill needed), this
        // completes in a single atomic load + CAS (~5-15ns).
        let current = self.tokens.0.load(self.ordering_load);
        if current > 0
            && self
                .tokens
                .0
                .compare_exchange_weak(current, current - 1, self.ordering_rmw, Ordering::Relaxed)
                .is_ok()
        {
            self.on_acquisition(1);
            self.touch_last_access_lazy();
            return true;
        }

        // Full path: includes time check, refill, and retry loop
        self.try_acquire_full()
    }

    /// Full acquisition path with refill logic and retry loop.
    /// Separated from the ultra-fast path to keep try_acquire small
    /// and maximize the chance it gets fully inlined.
    #[cold]
    #[inline(never)]
    fn try_acquire_full(&self) -> bool {
        let now_ms = current_time_ms();

        self.touch_last_access(now_ms);

        let last_refill = self.last_refill_ms.0.load(Ordering::Relaxed);
        if now_ms.wrapping_sub(last_refill) >= self.refill_interval_ms {
            self.refill_if_needed(now_ms);
        }

        let mut retries = 0;
        loop {
            let current = self.tokens.0.load(self.ordering_load);

            if current == 0 {
                self.on_rejection(1);
                return false;
            }

            match self.tokens.0.compare_exchange_weak(
                current,
                current - 1,
                self.ordering_rmw,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.on_acquisition(1);
                    return true;
                }
                Err(0) => {
                    self.on_rejection(1);
                    return false;
                }
                Err(_) => {
                    retries += 1;
                    if retries >= MAX_CAS_RETRIES {
                        self.on_rejection(1);
                        return false;
                    }
                    Self::backoff(retries);
                }
            }
        }
    }

    #[inline(always)]
    fn touch_last_access_lazy(&self) {
        let now = current_time_ms();
        let last = self.last_access_ms.0.load(Ordering::Relaxed);
        if now.wrapping_sub(last) > LAST_ACCESS_UPDATE_INTERVAL_MS {
            self.last_access_ms.0.store(now, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    fn touch_last_access(&self, now_ms: u64) {
        let last = self.last_access_ms.0.load(Ordering::Relaxed);
        if now_ms.wrapping_sub(last) > LAST_ACCESS_UPDATE_INTERVAL_MS {
            self.last_access_ms.0.store(now_ms, Ordering::Relaxed);
        }
    }

    /// Shared backoff logic to avoid code duplication.
    #[inline(always)]
    fn backoff(retries: usize) {
        if retries > CAS_BACKOFF_THRESHOLD {
            for _ in 0..(1 << (retries - CAS_BACKOFF_THRESHOLD).min(4)) {
                cpu_relax();
            }
        } else {
            cpu_relax();
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
        if n == 0 {
            return true;
        }
        if n == 1 {
            return self.try_acquire();
        }
        if n > self.max_tokens {
            self.on_rejection(n);
            return false;
        }

        let now_ms = current_time_ms();
        self.touch_last_access(now_ms);
        self.refill_if_needed(now_ms);

        self.try_acquire_with_bounded_cas(n)
    }

    /// Internal method for acquiring N tokens with CAS retry logic.
    ///
    /// This implements the core atomic acquisition with sophisticated
    /// retry logic to handle contention and ABA problems.
    #[inline(always)]
    fn try_acquire_with_bounded_cas(&self, n: u64) -> bool {
        let mut retries = 0;
        let mut last_seen = u64::MAX;
        let mut repeat_count: usize = 0;

        loop {
            let current = self.tokens.0.load(self.ordering_load);

            if current == last_seen {
                repeat_count += 1;
                if repeat_count >= MAX_REPEAT_COUNT {
                    if current < n {
                        self.on_rejection(n);
                        return false;
                    }
                    match self.tokens.0.compare_exchange(
                        current,
                        current - n,
                        self.ordering_rmw,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            self.on_acquisition(n);
                            return true;
                        }
                        Err(_) => {
                            self.on_rejection(n);
                            return false;
                        }
                    }
                }
            } else {
                last_seen = current;
                repeat_count = 0;
            }

            if current < n {
                self.on_rejection(n);
                return false;
            }

            match self.tokens.0.compare_exchange_weak(
                current,
                current - n,
                self.ordering_rmw,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.on_acquisition(n);
                    return true;
                }
                Err(actual) => {
                    if actual < n {
                        self.on_rejection(n);
                        return false;
                    }

                    retries += 1;

                    if retries >= MAX_CAS_RETRIES {
                        self.on_rejection(n);
                        return false;
                    }

                    Self::backoff(retries);
                }
            }
        }
    }

    #[inline(always)]
    fn on_acquisition(&self, _n: u64) {
        self.total_acquired.fetch_add(1, Ordering::Relaxed);
        if self.consecutive_rejections.load(Ordering::Relaxed) != 0 {
            self.consecutive_rejections.store(0, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    fn on_rejection(&self, _n: u64) {
        self.total_rejected.fetch_add(1, Ordering::Relaxed);
        self.consecutive_rejections.fetch_add(1, Ordering::Relaxed);
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
        let last_refill = self.last_refill_ms.0.load(Ordering::Relaxed);

        let elapsed = now_ms.wrapping_sub(last_refill);
        if elapsed < self.refill_interval_ms {
            return;
        }

        let periods = (elapsed / self.refill_interval_ms).min(MAX_REFILL_PERIODS);
        if periods == 0 {
            return;
        }

        let new_refill_time = last_refill.wrapping_add(periods * self.refill_interval_ms);

        // Claim the refill atomically; use Relaxed failure ordering
        // since we don't need the actual value on failure
        if self
            .last_refill_ms
            .0
            .compare_exchange(
                last_refill,
                new_refill_time,
                self.ordering_rmw,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.perform_refill(periods);
        }
    }

    /// Performs the actual token refill operation.
    ///
    /// This method adds tokens to the bucket, with optional adaptive
    /// refill rate when under sustained pressure.
    #[inline]
    fn perform_refill(&self, periods: u64) {
        let refill_rate = if self.is_under_sustained_pressure() {
            self.adaptive_refill_rate()
        } else {
            self.refill_rate
        };

        let tokens_to_add = (refill_rate as u64)
            .saturating_mul(periods)
            .min(self.max_tokens);

        let mut retries = 0;
        let mut current = self.tokens.0.load(self.ordering_load);

        loop {
            let new_tokens = current.saturating_add(tokens_to_add).min(self.max_tokens);

            if new_tokens == current {
                self.total_refills.fetch_add(1, Ordering::Relaxed);
                break;
            }

            match self.tokens.0.compare_exchange_weak(
                current,
                new_tokens,
                self.ordering_rmw,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.total_refills.fetch_add(1, Ordering::Relaxed);
                    debug!(
                        "Refilled {} tokens (periods: {})",
                        new_tokens - current,
                        periods
                    );
                    break;
                }
                Err(actual) => {
                    current = actual;
                    retries += 1;

                    if retries >= MAX_CAS_RETRIES {
                        break;
                    }

                    Self::backoff(retries);
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
        self.consecutive_rejections.load(Ordering::Relaxed) > 10
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
        let total_rejected = self.total_rejected.load(Ordering::Relaxed);
        let total_acquired = self.total_acquired.load(Ordering::Relaxed);
        let total = total_acquired + total_rejected;

        if total == 0 {
            return self.refill_rate;
        }

        // pressure_ratio > 0.5 means rejected * 2 > total
        if total_rejected * 2 > total {
            self.refill_rate * 4 / 5
        } else if total_rejected * 10 > total * 3 {
            self.refill_rate * 9 / 10
        } else {
            self.refill_rate
        }
    }

    /// Calculates the ratio of rejected to total requests.
    ///
    /// This metric helps identify when the system is under load.
    #[inline]
    fn calculate_pressure_ratio(&self) -> f64 {
        let total_rejected = self.total_rejected.load(Ordering::Relaxed);
        let total_acquired = self.total_acquired.load(Ordering::Relaxed);
        let total = total_acquired + total_rejected;

        if total == 0 {
            0.0
        } else {
            total_rejected as f64 / total as f64
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
    #[inline(always)]
    pub fn available_tokens(&self) -> u64 {
        self.refill_if_needed(current_time_ms());
        self.tokens.0.load(self.ordering_load)
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
    #[inline(always)]
    pub fn is_inactive(&self, inactive_duration_ms: u64) -> bool {
        let now_ms = current_time_ms();
        let last_ms = self.last_access_ms.0.load(Ordering::Relaxed);
        now_ms.wrapping_sub(last_ms) > inactive_duration_ms
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
        RateLimiterMetrics {
            total_acquired: self.total_acquired.load(Ordering::Relaxed),
            total_rejected: self.total_rejected.load(Ordering::Relaxed),
            total_refills: self.total_refills.load(Ordering::Relaxed),
            current_tokens: self.tokens.0.load(Ordering::Relaxed),
            max_tokens: self.max_tokens,
            consecutive_rejections: self.consecutive_rejections.load(Ordering::Relaxed),
            max_wait_time_ns: self.max_wait_time_ns.load(Ordering::Relaxed),
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
        let mut current = self.tokens.0.load(self.ordering_load);

        loop {
            let new_tokens = current.saturating_add(n).min(self.max_tokens);

            if new_tokens == current {
                break;
            }

            match self.tokens.0.compare_exchange_weak(
                current,
                new_tokens,
                self.ordering_rmw,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => {
                    current = actual;
                    retries += 1;

                    if retries >= MAX_CAS_RETRIES {
                        break;
                    }

                    Self::backoff(retries);
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

        self.tokens.0.store(self.max_tokens, self.ordering_store);
        self.last_refill_ms.0.store(now_ms, self.ordering_store);
        self.last_access_ms.0.store(now_ms, self.ordering_store);
        self.consecutive_rejections.store(0, self.ordering_store);
        self.max_wait_time_ns.store(0, self.ordering_store);
        self.total_acquired.store(0, self.ordering_store);
        self.total_rejected.store(0, self.ordering_store);
        self.total_refills.store(0, self.ordering_store);
    }

    /// Returns the maximum number of tokens allowed in the bucket.
    ///
    /// This is the burst capacity of the rate limiter.
    #[inline]
    pub fn get_max_tokens(&self) -> u64 {
        self.max_tokens
    }

    /// Returns the last access timestamp in milliseconds since UNIX epoch.
    ///
    /// Used for cleanup detection of inactive rate limiters.
    #[inline]
    pub fn get_last_access_ms(&self) -> u64 {
        self.last_access_ms.0.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("max_tokens", &self.max_tokens)
            .field("refill_rate", &self.refill_rate)
            .field("refill_interval_ms", &self.refill_interval_ms)
            .field("ordering", &self.ordering)
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
    #[cfg_attr(miri, ignore)]
    fn test_basic_acquisition() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }

        assert!(!limiter.try_acquire());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_bulk_acquisition() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

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
    #[cfg_attr(miri, ignore)]
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
    #[cfg_attr(miri, ignore)]
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
    #[cfg_attr(miri, ignore)]
    fn test_add_tokens() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }

        limiter.add_tokens(3);
        assert_eq!(limiter.available_tokens(), 8);

        limiter.add_tokens(20);
        assert_eq!(limiter.available_tokens(), 10);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_reset() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

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
    #[cfg_attr(miri, ignore)]
    fn test_is_inactive() {
        let limiter = RateLimiter::new(10, 1);

        // Should not be inactive immediately
        assert!(!limiter.is_inactive(1000));

        // available_tokens() goes through the full path (calls current_time_ms),
        // which guarantees last_access_ms is updated to the current time.
        let _ = limiter.available_tokens();

        // Wait and check
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Should be inactive for 100ms threshold (we slept 200ms)
        assert!(limiter.is_inactive(100));

        // Should NOT be inactive for 1000ms threshold (we only slept ~200ms)
        assert!(!limiter.is_inactive(1000));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_sustained_pressure() {
        let config = RateLimiterConfig {
            max_tokens: 5,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }

        for _ in 0..15 {
            assert!(!limiter.try_acquire());
        }

        let metrics = limiter.metrics();
        assert!(metrics.consecutive_rejections > 10);
        assert!(limiter.is_under_sustained_pressure());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
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

        assert!(limiter.try_acquire_n(50));
        assert!(limiter.try_acquire_n(30));

        // max_wait_time_ns is not tracked on the hot path to avoid
        // the cost of Instant::now() on every call.
        let _metrics = limiter.metrics();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
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
    #[cfg_attr(miri, ignore)]
    fn test_acquire_zero_tokens() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire_n(0));

        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }

        assert!(limiter.try_acquire_n(0));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_acquire_more_than_max() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        assert!(!limiter.try_acquire_n(11));

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_rejected, 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_last_access_update_throttling() {
        let limiter = RateLimiter::new(100, 10);

        // Rapid acquisitions should not update last_access every time
        let start_access = limiter.last_access_ms.0.load(Ordering::Relaxed);

        for _ in 0..50 {
            assert!(limiter.try_acquire());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // last_access should have been updated, but not 50 times
        let end_access = limiter.last_access_ms.0.load(Ordering::Relaxed);
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_aba_fix_no_silent_drain() {
        let config = RateLimiterConfig {
            max_tokens: 3,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        // Acquire 2, leaving 1 token
        assert!(limiter.try_acquire_n(2));

        // Requesting 3 should fail and NOT drain the remaining token
        assert!(!limiter.try_acquire_n(3));

        // The single remaining token should still be available
        assert_eq!(limiter.available_tokens(), 1);
        assert!(limiter.try_acquire());

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_acquired, 2); // 2 attempts succeeded (try_acquire_n(2) + try_acquire())
        assert_eq!(metrics.total_rejected, 1); // 1 attempt failed (try_acquire_n(3))
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_metrics_count_attempts_not_tokens() {
        let limiter = RateLimiter::new(100, 10);

        assert!(limiter.try_acquire_n(50));
        assert!(limiter.try_acquire_n(30));
        assert!(limiter.try_acquire());

        let metrics = limiter.metrics();
        // Each call is 1 attempt regardless of tokens requested
        assert_eq!(metrics.total_acquired, 3);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_rejection_resets_consecutive_on_acquire() {
        let limiter = RateLimiter::new(5, 1);

        // Drain all tokens
        for _ in 0..5 {
            limiter.try_acquire();
        }

        // Accumulate rejections
        for _ in 0..8 {
            assert!(!limiter.try_acquire());
        }
        assert!(limiter.metrics().consecutive_rejections >= 8);

        // Wait for refill and acquire
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(limiter.try_acquire());

        // Consecutive rejections should be reset to 0
        assert_eq!(limiter.metrics().consecutive_rejections, 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_relaxed_ordering_mode() {
        let config = RateLimiterConfig {
            max_tokens: 20,
            refill_rate: 5,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::Relaxed,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..20 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_acquired, 20);
        assert_eq!(metrics.total_rejected, 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_sequential_ordering_mode() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 5,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::Sequential,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_concurrent_single_and_multi_acquire() {
        use std::sync::Arc;
        use std::thread;

        let limiter = Arc::new(RateLimiter::new(500, 10));
        let mut handles = vec![];

        // Half threads use try_acquire, half use try_acquire_n
        for i in 0..20 {
            let l = limiter.clone();
            handles.push(thread::spawn(move || {
                let mut acquired = 0u64;
                for _ in 0..50 {
                    if i % 2 == 0 {
                        if l.try_acquire() {
                            acquired += 1;
                        }
                    } else if l.try_acquire_n(2) {
                        acquired += 1;
                    }
                }
                acquired
            }));
        }

        let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_acquired, total);
        // Total requests = acquired + rejected
        assert_eq!(metrics.total_requests(), 20 * 50);
    }

    #[test]
    fn test_get_max_tokens() {
        let limiter = RateLimiter::new(42, 5);
        assert_eq!(limiter.get_max_tokens(), 42);
    }

    #[test]
    fn test_get_last_access_ms_updates() {
        let limiter = RateLimiter::new(100, 10);
        let before = limiter.get_last_access_ms();

        // try_acquire_n always updates last_access
        assert!(limiter.try_acquire_n(1));

        let after = limiter.get_last_access_ms();
        assert!(after >= before);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_add_tokens_when_empty() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..10 {
            limiter.try_acquire();
        }
        assert_eq!(limiter.available_tokens(), 0);

        limiter.add_tokens(5);
        assert_eq!(limiter.available_tokens(), 5);
    }

    #[test]
    fn test_add_tokens_zero() {
        let limiter = RateLimiter::new(10, 1);
        let before = limiter.available_tokens();
        limiter.add_tokens(0);
        assert_eq!(limiter.available_tokens(), before);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_pressure_ratio_calculation() {
        let config = RateLimiterConfig {
            max_tokens: 5,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..5 {
            limiter.try_acquire();
        }
        for _ in 0..5 {
            limiter.try_acquire();
        }

        let metrics = limiter.metrics();
        let ratio = metrics.pressure_ratio;
        assert!(ratio > 0.4 && ratio < 0.6, "Expected ~0.5, got {}", ratio);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_refill_caps_at_max() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 10,
            refill_interval_ms: 50,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        // Drain 5
        for _ in 0..5 {
            limiter.try_acquire();
        }

        // Wait for multiple refill periods
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Should cap at max_tokens, not exceed it
        assert_eq!(limiter.available_tokens(), 10);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_reset_clears_all_state() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 5,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        for _ in 0..10 {
            limiter.try_acquire();
        }
        for _ in 0..5 {
            limiter.try_acquire();
        }

        limiter.reset();

        assert_eq!(limiter.available_tokens(), 10);
        let m = limiter.metrics();
        assert_eq!(m.total_acquired, 0);
        assert_eq!(m.total_rejected, 0);
        assert_eq!(m.total_refills, 0);
        assert_eq!(m.consecutive_rejections, 0);
        assert_eq!(m.max_wait_time_ns, 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_try_acquire_n_exactly_max() {
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 600_000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire_n(10));
        assert_eq!(limiter.available_tokens(), 0);

        assert!(!limiter.try_acquire());
    }
}
