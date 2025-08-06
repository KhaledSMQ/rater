//! # Rater - High-Performance Rate Limiter for Rust
//!
//! A blazing-fast, lock-free rate limiting library that helps you control the flow of requests
//! in your application. Think of it as a traffic controller that ensures your system doesn't
//! get overwhelmed by too many requests at once.
//!
//! ## What is Rate Limiting?
//!
//! Rate limiting is like having a bouncer at a club - it controls how many requests can enter
//! your system within a specific time period. This prevents abuse, ensures fair usage, and
//! protects your services from being overwhelmed.
//!
//! ## The Token Bucket Algorithm
//!
//! This library uses the token bucket algorithm, which works like a vending machine:
//!
//! ```text
//!     Token Bucket Visualization:
//!
//!     Time 0:    [🪙🪙🪙🪙🪙] (5 tokens available)
//!     Request 1: [🪙🪙🪙🪙] ✅ (takes 1 token)
//!     Request 2: [🪙🪙🪙] ✅ (takes 1 token)
//!     Time +1s:  [🪙🪙🪙🪙🪙] (refilled to max)
//!     Request 3: [🪙🪙🪙🪙] ✅ (takes 1 token)
//! ```
//!
//! - **Tokens** = Permission to make a request
//! - **Bucket** = Container holding a limited number of tokens
//! - **Refill** = Tokens are added back at a steady rate
//!
//! ## Features
//!
//! - 🔒 **Lock-free Implementation** - Multiple threads can use it simultaneously without blocking
//! - ⚡ **Blazing Fast** - Optimized for your CPU architecture (uses PAUSE on Intel, YIELD on ARM)
//! - 🔄 **Smart Refilling** - Automatically adjusts when under heavy load
//! - 🌐 **Per-IP Limiting** - Different limits for different users/IPs
//! - 📊 **Real-time Metrics** - Monitor performance and detect issues
//! - 🛡️ **Thread-Safe** - Safe to share across threads without extra synchronization
//!
//! ## Quick Start
//!
//! ### Basic Rate Limiting
//!
//! ```rust
//! use rater::RateLimiter;
//!
//! // Create a rate limiter:
//! // - 100 tokens maximum capacity (burst limit)
//! // - Refills 10 tokens every second
//! let limiter = RateLimiter::new(100, 10);
//!
//! // Try to process a request
//! if limiter.try_acquire() {
//!     println!("✅ Request approved - processing...");
//!     // Your request handling code here
//! } else {
//!     println!("⛔ Rate limited - try again later");
//!     // Return 429 Too Many Requests
//! }
//! ```
//!
//! ### Advanced Usage with Builder Pattern
//!
//! ```rust
//! use rater::{RateLimiterBuilder, MemoryOrdering};
//!
//! let limiter = RateLimiterBuilder::new()
//!     .max_tokens(1000)           // Maximum burst size
//!     .refill_rate(100)           // Tokens per interval
//!     .refill_interval_ms(1000)   // Refill every second
//!     .memory_ordering(MemoryOrdering::AcquireRelease)  // Memory consistency
//!     .build();
//!
//! // Acquire multiple tokens at once (for expensive operations)
//! if limiter.try_acquire_n(5) {
//!     println!("Acquired 5 tokens for expensive operation");
//! }
//! ```
//!
//! ### Per-IP Rate Limiting
//!
//! ```rust
//! use rater::{IpRateLimiterManager, RateLimiterConfig};
//! use std::net::IpAddr;
//!
//! // Create a manager that handles rate limiting per IP address
//! let config = RateLimiterConfig::per_second(10);  // 10 requests per second per IP
//! let manager = IpRateLimiterManager::new(config);
//!
//! // In your request handler:
//! let client_ip: IpAddr = "192.168.1.100".parse().unwrap();
//!
//! if manager.try_acquire(client_ip) {
//!     // Process request
//! } else {
//!     // Rate limit this specific IP
//! }
//! ```
//!
//! ## Architecture Overview
//!
//! ```text
//!                    ┌─────────────────────────┐
//!                    │   Your Application      │
//!                    └──────────┬──────────────┘
//!                               │
//!                    ┌──────────▼──────────────┐
//!                    │    Rate Limiter API     │
//!                    ├──────────────────────────┤
//!                    │  • try_acquire()         │
//!                    │  • try_acquire_n()       │
//!                    │  • metrics()             │
//!                    └──────────┬───────────────┘
//!                               │
//!                ┌──────────────┴───────────────┐
//!                │                               │
//!     ┌──────────▼──────────┐       ┌───────────▼──────────┐
//!     │   Token Bucket      │       │   IP Manager         │
//!     ├─────────────────────┤       ├──────────────────────┤
//!     │ • Atomic tokens     │       │ • Per-IP limiters    │
//!     │ • Auto refill       │       │ • Auto cleanup       │
//!     │ • Lock-free CAS     │       │ • LRU eviction       │
//!     └─────────────────────┘       └──────────────────────┘
//! ```
//!
//! ## Performance Characteristics
//!
//! | Operation | Time Complexity | Space Complexity |
//! |-----------|----------------|------------------|
//! | try_acquire() | O(1)* | O(1) |
//! | try_acquire_n() | O(1)* | O(1) |
//! | refill | O(1)* | O(1) |
//! | metrics | O(1) | O(1) |
//!
//! *Amortized constant time with bounded retries under contention
//!
//! ## Common Use Cases
//!
//! 1. **API Rate Limiting** - Prevent API abuse and ensure fair usage
//! 2. **Database Protection** - Limit queries to prevent overload
//! 3. **Microservice Communication** - Control inter-service request rates
//! 4. **Background Job Processing** - Throttle job execution rates
//! 5. **Resource Access Control** - Limit access to expensive resources
//!
//! ## Thread Safety
//!
//! All types are thread-safe and can be shared across threads:
//! - `RateLimiter` - Safe to share via `Arc<RateLimiter>`
//! - `IpRateLimiterManager` - Safe to share via `Arc<IpRateLimiterManager>`
//!
//! ## Memory Ordering
//!
//! Choose the right memory ordering for your use case:
//! - `Relaxed` - Fastest, use when exact ordering doesn't matter
//! - `AcquireRelease` - Balanced (default), ensures proper synchronization
//! - `Sequential` - Strongest guarantees, use when strict ordering is critical
//!
//! ## Examples
//!
//! See the `examples/` directory for complete examples:
//! - `basic.rs` - Simple rate limiting
//! - `ip_limiting.rs` - Per-IP rate limiting with cleanup
//!
//! ## Safety
//!
//! This crate uses `unsafe` code only for:
//! - Platform-specific CPU pause instructions (for efficient spinning)
//! - Cache alignment optimizations (to prevent false sharing)
//!
//! All unsafe code is thoroughly tested and documented.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(
    missing_docs,
    rust_2018_idioms,
    unreachable_pub,
    missing_debug_implementations
)]
#![forbid(unsafe_op_in_unsafe_fn)]

// Internal module
mod rate_limiter;

// Public re-exports
pub use rate_limiter::{
    cpu_relax, current_time_ms, current_time_ns, current_time_us, HealthStatus,
    IpRateLimiterManager, ManagerStats, MemoryOrdering, RateLimiter, RateLimiterConfig,
    RateLimiterMetrics,
};

/// A rate limiter wrapped in `Arc` for convenient thread-safe sharing.
///
/// # Example
/// ```rust
/// use rater::{RateLimiter, SharedRateLimiter};
/// use std::sync::Arc;
///
/// let limiter = RateLimiter::new(100, 10);
/// let shared: SharedRateLimiter = Arc::new(limiter);
///
/// // Now you can clone and share across threads
/// let limiter_clone = shared.clone();
/// std::thread::spawn(move || {
///     limiter_clone.try_acquire();
/// });
/// ```
pub type SharedRateLimiter = std::sync::Arc<RateLimiter>;

/// An IP rate limiter manager wrapped in `Arc` for convenient thread-safe sharing.
///
/// This is useful when you need to share the IP manager across multiple threads
/// or async tasks in a web server.
pub type SharedIpManager = std::sync::Arc<IpRateLimiterManager>;

/// Version information for the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Minimum supported Rust version.
///
/// This crate requires at least Rust 1.70.0 due to:
/// - Stable atomic operations
/// - Const generics
/// - Edition 2021 features
pub const MSRV: &str = "1.70.0";

/// Prelude module for convenient imports.
///
/// Import everything you need with a single line:
/// ```rust
/// use rater::prelude::*;
/// ```
pub mod prelude {
    //! Common imports for typical rate limiting use cases.
    //!
    //! # Example
    //! ```rust
    //! use rater::prelude::*;
    //!
    //! let limiter = RateLimiter::new(100, 10);
    //! let config = RateLimiterConfig::per_second(50);
    //! let status = HealthStatus::Healthy;
    //! ```

    pub use crate::{
        HealthStatus, IpRateLimiterManager, ManagerStats, MemoryOrdering, RateLimiter,
        RateLimiterConfig, RateLimiterMetrics, SharedIpManager, SharedRateLimiter,
    };
}

/// Builder pattern for creating rate limiters with custom configuration.
///
/// The builder pattern provides a fluent API for constructing rate limiters
/// with validated configuration. This is the recommended way to create
/// rate limiters with non-default settings.
///
/// # Example
///
/// ```rust
/// use rater::{RateLimiterBuilder, MemoryOrdering};
///
/// // Build a rate limiter for 100 requests per minute
/// let limiter = RateLimiterBuilder::new()
///     .max_tokens(100)              // Burst capacity
///     .refill_rate(100)            // Tokens to add
///     .refill_interval_ms(60_000)  // Every minute
///     .memory_ordering(MemoryOrdering::Relaxed)  // Fast mode
///     .build();
///
/// // Or use try_build() for error handling
/// let result = RateLimiterBuilder::new()
///     .max_tokens(0)  // Invalid!
///     .try_build();
///
/// assert!(result.is_err());
/// ```
#[derive(Debug, Clone)]
pub struct RateLimiterBuilder {
    config: RateLimiterConfig,
}

impl RateLimiterBuilder {
    /// Creates a new builder with default configuration.
    ///
    /// Default configuration:
    /// - 50 maximum tokens
    /// - 10 tokens refill rate
    /// - 1000ms (1 second) refill interval
    /// - AcquireRelease memory ordering
    pub fn new() -> Self {
        Self {
            config: RateLimiterConfig::default(),
        }
    }

    /// Sets the maximum number of tokens the bucket can hold.
    ///
    /// This is also known as the "burst capacity" - the maximum number
    /// of requests that can be made in rapid succession.
    ///
    /// # Arguments
    ///
    /// * `tokens` - Maximum token capacity (must be > 0)
    pub fn max_tokens(mut self, tokens: u64) -> Self {
        self.config.max_tokens = tokens;
        self
    }

    /// Sets the refill rate (tokens added per interval).
    ///
    /// This determines the sustained rate limit. For example, with a
    /// refill_rate of 10 and refill_interval_ms of 1000, you get
    /// 10 requests per second sustained rate.
    ///
    /// # Arguments
    ///
    /// * `rate` - Number of tokens to add each interval
    pub fn refill_rate(mut self, rate: u32) -> Self {
        self.config.refill_rate = rate;
        self
    }

    /// Sets the refill interval in milliseconds.
    ///
    /// How often tokens are added back to the bucket.
    /// Common values:
    /// - 1000 ms = per second
    /// - 60000 ms = per minute
    /// - 100 ms = 10 times per second
    ///
    /// # Arguments
    ///
    /// * `ms` - Interval between refills in milliseconds (must be > 0)
    pub fn refill_interval_ms(mut self, ms: u64) -> Self {
        self.config.refill_interval_ms = ms;
        self
    }

    /// Sets the memory ordering strategy for atomic operations.
    ///
    /// - `Relaxed`: Fastest but weakest guarantees
    /// - `AcquireRelease`: Balanced performance and correctness (default)
    /// - `Sequential`: Strongest guarantees but slower
    ///
    /// Unless you have specific requirements, use the default.
    pub fn memory_ordering(mut self, ordering: MemoryOrdering) -> Self {
        self.config.ordering = ordering;
        self
    }

    /// Builds the rate limiter with the configured settings.
    ///
    /// # Panics
    ///
    /// Panics if the configuration is invalid:
    /// - `max_tokens` is 0
    /// - `refill_interval_ms` is 0
    /// - `refill_rate` exceeds `max_tokens`
    ///
    /// Use `try_build()` if you want to handle errors.
    pub fn build(self) -> RateLimiter {
        RateLimiter::with_config(self.config)
    }

    /// Attempts to build the rate limiter, returning an error if invalid.
    ///
    /// This is the safe version that returns a `Result` instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns an error message if configuration is invalid.
    pub fn try_build(self) -> Result<RateLimiter, &'static str> {
        self.config.validate()?;
        Ok(RateLimiter::with_config(self.config))
    }
}

impl Default for RateLimiterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_basic_functionality() {
        let limiter = RateLimiter::new(10, 1);

        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }

        assert!(!limiter.try_acquire());

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_acquired, 10);
        assert_eq!(metrics.total_rejected, 1);
    }

    #[test]
    fn test_builder() {
        let limiter = RateLimiterBuilder::new()
            .max_tokens(50)
            .refill_rate(5)
            .refill_interval_ms(1000)
            .build();

        assert_eq!(limiter.available_tokens(), 50);
    }

    #[test]
    fn test_builder_validation() {
        let result = RateLimiterBuilder::new().max_tokens(0).try_build();

        assert!(result.is_err());
    }

    #[test]
    fn test_thread_safety() {
        let limiter = Arc::new(RateLimiter::new(1000, 100));
        let mut handles = vec![];

        for _ in 0..10 {
            let limiter_clone = limiter.clone();
            let handle = thread::spawn(move || {
                let mut acquired = 0;
                for _ in 0..200 {
                    if limiter_clone.try_acquire() {
                        acquired += 1;
                    }
                }
                acquired
            });
            handles.push(handle);
        }

        let total: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();

        // Should acquire exactly 1000 tokens (or very close due to refills)
        assert!(total <= 1000);
        assert!(total >= 900); // Allow some margin for timing
    }
    #[test]
    fn test_prelude_imports() {
        // Test that prelude exports work
        use crate::prelude::*;

        let _limiter = RateLimiter::new(10, 1);
        let _config = RateLimiterConfig::default();
        let _ordering = MemoryOrdering::AcquireRelease;
        let _status = HealthStatus::Healthy;
    }

    #[test]
    fn test_shared_types() {
        let limiter = RateLimiter::new(10, 1);
        let _shared: SharedRateLimiter = std::sync::Arc::new(limiter);

        let manager = IpRateLimiterManager::new(RateLimiterConfig::default());
        let _shared_manager: SharedIpManager = std::sync::Arc::new(manager);
    }

    #[test]
    fn test_constants() {
        assert!(!VERSION.is_empty());
        assert_eq!(MSRV, "1.70.0");
    }

    #[test]
    fn test_builder_default() {
        let builder = RateLimiterBuilder::default();
        let limiter = builder.build();
        assert!(limiter.available_tokens() > 0);
    }

    #[test]
    fn test_builder_chain() {
        let limiter = RateLimiterBuilder::new()
            .max_tokens(100)
            .refill_rate(10)
            .refill_interval_ms(500)
            .memory_ordering(MemoryOrdering::Sequential)
            .build();

        assert_eq!(limiter.available_tokens(), 100);
    }
}
