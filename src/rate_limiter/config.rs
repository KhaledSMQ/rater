//! # Rate Limiter Configuration
//!
//! This module provides configuration structures and enums for customizing rate limiter behavior.
//! Think of this as the "settings panel" for your rate limiter.
//!
//! ## Key Concepts
//!
//! ### Token Bucket Parameters
//!
//! ```text
//!     Token Bucket Configuration:
//!
//!     ┌──────────────────────────────┐
//!     │   Max Tokens (Capacity)      │ ← Burst limit
//!     │   ┌─────────────────────┐    │
//!     │   │ 🪙 🪙 🪙 🪙 🪙     │    │
//!     │   │ 🪙 🪙 🪙 🪙 🪙     │    │ ← Current tokens
//!     │   └─────────────────────┘    │
//!     │                              │
//!     │   Refill Rate: 10 tokens     │ ← Tokens added
//!     │   Refill Interval: 1000ms    │ ← How often
//!     └──────────────────────────────┘
//! ```
//!
//! ### Memory Ordering
//!
//! Memory ordering controls how atomic operations synchronize between threads:
//!
//! ```text
//!     Relaxed ──────► Fast but minimal guarantees
//!        │
//!     AcquireRelease ► Balanced (recommended)
//!        │
//!     Sequential ───► Slow but strongest guarantees
//! ```

use std::sync::atomic::Ordering;

/// Maximum number of refill periods to process at once.
///
/// This prevents issues when the system has been idle for a long time.
/// For example, if your app was paused for an hour, we don't want to
/// suddenly add an hour's worth of tokens - we cap it at 100 periods.
pub const MAX_REFILL_PERIODS: u64 = 100;

/// Memory ordering strategy for atomic operations.
///
/// This controls the synchronization guarantees between threads.
/// Choose based on your performance vs correctness requirements.
///
/// ## Quick Guide
///
/// - Use `Relaxed` when you need maximum speed and don't care about exact ordering
/// - Use `AcquireRelease` (default) for most use cases - good balance
/// - Use `Sequential` when you need strict ordering guarantees (e.g., for debugging)
///
/// ## Example
///
/// ```rust
/// use rater::{RateLimiterConfig, MemoryOrdering};
///
/// // For high-performance scenarios where exact counts don't matter
/// let fast_config = RateLimiterConfig::new(1000, 100, 1000)
///     .with_ordering(MemoryOrdering::Relaxed);
///
/// // For normal use (default)
/// let balanced_config = RateLimiterConfig::new(1000, 100, 1000)
///     .with_ordering(MemoryOrdering::AcquireRelease);
///
/// // For scenarios requiring strict ordering
/// let strict_config = RateLimiterConfig::new(1000, 100, 1000)
///     .with_ordering(MemoryOrdering::Sequential);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOrdering {
    /// Relaxed ordering - fastest but provides minimal guarantees.
    ///
    /// Use when:
    /// - You need maximum performance
    /// - Exact token counts aren't critical
    /// - You're OK with some operations appearing out of order
    Relaxed,

    /// Acquire-Release ordering - balanced performance and correctness.
    ///
    /// Use when:
    /// - You want good performance with reasonable guarantees (default)
    /// - You need synchronization between threads
    /// - This is the recommended setting for most applications
    AcquireRelease,

    /// Sequential consistency - strongest guarantees but slower.
    ///
    /// Use when:
    /// - You need strict ordering guarantees
    /// - You're debugging synchronization issues
    /// - Correctness is more important than performance
    Sequential,
}

impl MemoryOrdering {
    /// Returns the appropriate `Ordering` for load (read) operations.
    ///
    /// Used when reading the current token count.
    #[inline(always)]
    pub(crate) fn load(&self) -> Ordering {
        match self {
            Self::Relaxed => Ordering::Relaxed,
            Self::AcquireRelease => Ordering::Acquire,
            Self::Sequential => Ordering::SeqCst,
        }
    }

    /// Returns the appropriate `Ordering` for store (write) operations.
    ///
    /// Used when setting token counts directly.
    #[inline(always)]
    pub(crate) fn store(&self) -> Ordering {
        match self {
            Self::Relaxed => Ordering::Relaxed,
            Self::AcquireRelease => Ordering::Release,
            Self::Sequential => Ordering::SeqCst,
        }
    }

    /// Returns the appropriate `Ordering` for read-modify-write operations.
    ///
    /// Used for operations that both read and update values atomically.
    #[inline(always)]
    pub(crate) fn rmw(&self) -> Ordering {
        match self {
            Self::Relaxed => Ordering::Relaxed,
            Self::AcquireRelease => Ordering::AcqRel,
            Self::Sequential => Ordering::SeqCst,
        }
    }

    /// Returns the appropriate `Ordering` for compare-and-swap failure cases.
    ///
    /// Used when a CAS operation fails and we need to read the current value.
    #[inline(always)]
    pub(crate) fn cas_failure(&self) -> Ordering {
        match self {
            Self::Relaxed => Ordering::Relaxed,
            Self::AcquireRelease => Ordering::Acquire,
            Self::Sequential => Ordering::SeqCst,
        }
    }
}

impl Default for MemoryOrdering {
    /// Returns the default memory ordering (AcquireRelease).
    ///
    /// This provides a good balance between performance and correctness.
    fn default() -> Self {
        Self::AcquireRelease
    }
}

/// Configuration for rate limiter instances.
///
/// This struct defines all the parameters that control how a rate limiter behaves.
/// You can create configurations manually or use the convenient factory methods.
///
/// ## Token Bucket Model
///
/// ```text
///     Configuration Example:
///     ┌────────────────────────────────────┐
///     │ max_tokens: 100                    │
///     │ refill_rate: 10                    │
///     │ refill_interval_ms: 1000           │
///     │                                    │
///     │ Result: 10 requests/second         │
///     │         100 burst capacity         │
///     └────────────────────────────────────┘
/// ```
///
/// ## Examples
///
/// ```rust
/// use rater::RateLimiterConfig;
///
/// // Simple per-second rate limiting
/// let config = RateLimiterConfig::per_second(50);  // 50 req/sec
///
/// // Per-minute rate limiting
/// let config = RateLimiterConfig::per_minute(1000);  // 1000 req/min
///
/// // Custom configuration
/// let config = RateLimiterConfig::new(
///     100,    // max tokens (burst)
///     10,     // refill rate
///     100     // refill every 100ms
/// );
///
/// // With burst multiplier
/// let config = RateLimiterConfig::per_second(10)
///     .with_burst_multiplier(5);  // Allow bursts up to 50 requests
/// ```
#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    /// Maximum number of tokens the bucket can hold.
    ///
    /// This is your "burst capacity" - the maximum number of requests
    /// that can be made in rapid succession. Set this higher than your
    /// refill_rate to allow for traffic bursts.
    pub max_tokens: u64,

    /// Number of tokens to add during each refill interval.
    ///
    /// This determines your sustained rate. With refill_rate=10 and
    /// refill_interval_ms=1000, you get 10 requests per second sustained.
    pub refill_rate: u32,

    /// Interval between token refills in milliseconds.
    ///
    /// How often new tokens are added to the bucket.
    /// Common values:
    /// - 1000 ms (1 second) for per-second limiting
    /// - 60000 ms (1 minute) for per-minute limiting
    /// - 100 ms for fine-grained control
    pub refill_interval_ms: u64,

    /// Memory ordering strategy for atomic operations.
    ///
    /// Controls the synchronization guarantees. Default is AcquireRelease
    /// which provides good balance. Only change if you have specific needs.
    pub ordering: MemoryOrdering,
}

impl Default for RateLimiterConfig {
    /// Creates a default configuration.
    ///
    /// Default values:
    /// - 50 maximum tokens (burst capacity)
    /// - 10 tokens refill rate
    /// - 1000ms (1 second) refill interval
    /// - AcquireRelease memory ordering
    ///
    /// This gives you 10 requests/second with burst up to 50.
    fn default() -> Self {
        Self {
            max_tokens: 50,
            refill_rate: 10,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::AcquireRelease,
        }
    }
}

impl RateLimiterConfig {
    /// Creates a new configuration with specified parameters.
    ///
    /// # Arguments
    ///
    /// * `max_tokens` - Maximum tokens (burst capacity)
    /// * `refill_rate` - Tokens added per interval
    /// * `refill_interval_ms` - Milliseconds between refills
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiterConfig;
    ///
    /// // 100 requests burst, 20 requests/second sustained
    /// let config = RateLimiterConfig::new(100, 20, 1000);
    /// ```
    pub fn new(max_tokens: u64, refill_rate: u32, refill_interval_ms: u64) -> Self {
        Self {
            max_tokens,
            refill_rate,
            refill_interval_ms,
            ordering: MemoryOrdering::default(),
        }
    }

    /// Creates a configuration for per-second rate limiting.
    ///
    /// Convenience method that sets up limiting by requests per second.
    /// The burst capacity is automatically set to 2x the rate.
    ///
    /// # Arguments
    ///
    /// * `requests_per_second` - Maximum sustained requests per second
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiterConfig;
    ///
    /// // Allow 100 requests per second with burst up to 200
    /// let config = RateLimiterConfig::per_second(100);
    /// ```
    pub fn per_second(requests_per_second: u32) -> Self {
        Self {
            max_tokens: (requests_per_second * 2) as u64,  // 2x burst capacity
            refill_rate: requests_per_second,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::default(),
        }
    }

    /// Creates a configuration for per-minute rate limiting.
    ///
    /// Convenience method that sets up limiting by requests per minute.
    /// Useful for APIs with minute-based quotas.
    ///
    /// # Arguments
    ///
    /// * `requests_per_minute` - Maximum requests per minute
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiterConfig;
    ///
    /// // Allow 1000 requests per minute
    /// let config = RateLimiterConfig::per_minute(1000);
    /// ```
    pub fn per_minute(requests_per_minute: u32) -> Self {
        Self {
            max_tokens: requests_per_minute as u64,
            refill_rate: requests_per_minute,
            refill_interval_ms: 60_000,
            ordering: MemoryOrdering::default(),
        }
    }

    /// Sets the memory ordering strategy.
    ///
    /// Builder method to customize memory ordering if needed.
    /// Most applications should use the default (AcquireRelease).
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{RateLimiterConfig, MemoryOrdering};
    ///
    /// let config = RateLimiterConfig::per_second(100)
    ///     .with_ordering(MemoryOrdering::Relaxed);  // Maximum speed
    /// ```
    pub fn with_ordering(mut self, ordering: MemoryOrdering) -> Self {
        self.ordering = ordering;
        self
    }

    /// Sets the burst capacity as a multiple of the refill rate.
    ///
    /// This is useful when you want to allow bursts but express them
    /// as a multiplier rather than an absolute number.
    ///
    /// # Arguments
    ///
    /// * `multiplier` - How many times the refill rate for burst capacity
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiterConfig;
    ///
    /// // 10 req/sec sustained, burst up to 50 requests
    /// let config = RateLimiterConfig::per_second(10)
    ///     .with_burst_multiplier(5);
    /// ```
    pub fn with_burst_multiplier(mut self, multiplier: u32) -> Self {
        self.max_tokens = (self.refill_rate * multiplier) as u64;
        self
    }

    /// Validates the configuration for correctness.
    ///
    /// Checks that all parameters are valid and consistent.
    /// This is automatically called when creating a rate limiter.
    ///
    /// # Errors
    ///
    /// Returns an error message if:
    /// - `max_tokens` is 0
    /// - `refill_interval_ms` is 0
    /// - `refill_rate` exceeds `max_tokens`
    /// - Values would overflow on 32-bit systems
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiterConfig;
    ///
    /// let config = RateLimiterConfig::new(0, 10, 1000);  // Invalid!
    /// assert!(config.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_tokens == 0 {
            return Err("max_tokens must be greater than 0");
        }

        // Platform-specific validation for 32-bit systems
        #[cfg(not(target_pointer_width = "64"))]
        {
            if self.max_tokens > u32::MAX as u64 {
                return Err("max_tokens exceeds platform limit (u32::MAX on 32-bit systems)");
            }

            // Check for potential overflow in refill calculations
            let max_refill = (self.refill_rate as u64).saturating_mul(MAX_REFILL_PERIODS);
            if max_refill > u32::MAX as u64 {
                return Err("refill_rate * MAX_REFILL_PERIODS would overflow on 32-bit systems");
            }
        }

        if self.refill_interval_ms == 0 {
            return Err("refill_interval_ms must be greater than 0");
        }

        // Validate that refill rate makes sense
        if self.refill_rate as u64 > self.max_tokens {
            return Err("refill_rate should not exceed max_tokens");
        }

        Ok(())
    }

    /// Returns the effective rate limit per second.
    ///
    /// Calculates the actual requests per second based on the
    /// refill rate and interval. Useful for displaying the
    /// configured rate to users.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiterConfig;
    ///
    /// let config = RateLimiterConfig::new(100, 50, 500);  // 50 tokens per 500ms
    /// assert_eq!(config.effective_rate_per_second(), 100.0);  // 100 req/sec
    /// ```
    pub fn effective_rate_per_second(&self) -> f64 {
        if self.refill_interval_ms == 0 {
            0.0
        } else {
            (self.refill_rate as f64 * 1000.0) / self.refill_interval_ms as f64
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_ordering() {
        let ordering = MemoryOrdering::AcquireRelease;
        assert_eq!(ordering.load(), Ordering::Acquire);
        assert_eq!(ordering.store(), Ordering::Release);
        assert_eq!(ordering.rmw(), Ordering::AcqRel);
        assert_eq!(ordering.cas_failure(), Ordering::Acquire);
    }

    #[test]
    fn test_config_validation() {
        let valid = RateLimiterConfig::default();
        assert!(valid.validate().is_ok());

        let invalid = RateLimiterConfig {
            max_tokens: 0,
            ..Default::default()
        };
        assert!(invalid.validate().is_err());

        let invalid_refill = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 20,
            ..Default::default()
        };
        assert!(invalid_refill.validate().is_err());
    }

    #[test]
    fn test_config_builders() {
        let config = RateLimiterConfig::per_second(100);
        assert_eq!(config.max_tokens, 200);
        assert_eq!(config.refill_rate, 100);
        assert_eq!(config.effective_rate_per_second(), 100.0);
    }

    #[cfg(not(target_pointer_width = "64"))]
    #[test]
    fn test_32bit_validation() {
        let config = RateLimiterConfig {
            max_tokens: (u32::MAX as u64) + 1,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
    #[test]
    fn test_memory_ordering_all_variants() {
        let relaxed = MemoryOrdering::Relaxed;
        assert_eq!(relaxed.load(), Ordering::Relaxed);
        assert_eq!(relaxed.store(), Ordering::Relaxed);
        assert_eq!(relaxed.rmw(), Ordering::Relaxed);
        assert_eq!(relaxed.cas_failure(), Ordering::Relaxed);

        let sequential = MemoryOrdering::Sequential;
        assert_eq!(sequential.load(), Ordering::SeqCst);
        assert_eq!(sequential.store(), Ordering::SeqCst);
        assert_eq!(sequential.rmw(), Ordering::SeqCst);
        assert_eq!(sequential.cas_failure(), Ordering::SeqCst);
    }

    #[test]
    fn test_config_with_burst_multiplier() {
        let config = RateLimiterConfig::per_second(10)
            .with_burst_multiplier(5);

        assert_eq!(config.max_tokens, 50);
        assert_eq!(config.refill_rate, 10);
    }

    #[test]
    fn test_config_per_minute() {
        let config = RateLimiterConfig::per_minute(120);

        assert_eq!(config.max_tokens, 120);
        assert_eq!(config.refill_rate, 120);
        assert_eq!(config.refill_interval_ms, 60_000);
        assert_eq!(config.effective_rate_per_second(), 2.0);
    }

    #[test]
    fn test_config_with_ordering() {
        let config = RateLimiterConfig::default()
            .with_ordering(MemoryOrdering::Sequential);

        assert_eq!(config.ordering, MemoryOrdering::Sequential);
    }

    #[test]
    fn test_config_validation_edge_cases() {
        // Zero refill interval
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 5,
            refill_interval_ms: 0,
            ordering: MemoryOrdering::default(),
        };
        assert!(config.validate().is_err());
        assert_eq!(config.effective_rate_per_second(), 0.0);
    }

    #[test]
    fn test_default_memory_ordering() {
        assert_eq!(MemoryOrdering::default(), MemoryOrdering::AcquireRelease);
    }

    #[cfg(not(target_pointer_width = "64"))]
    #[test]
    fn test_32bit_overflow_validation() {
        // Test overflow in refill calculations
        let config = RateLimiterConfig {
            max_tokens: 1000,
            refill_rate: u32::MAX / 10,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::default(),
        };
        assert!(config.validate().is_err());
    }
}