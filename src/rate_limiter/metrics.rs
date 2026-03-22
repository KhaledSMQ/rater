//! This module provides comprehensive performance monitoring and health analysis
//! for rate limiters. It helps you understand how your rate limiting is performing
//! and detect when your system is under stress.
//!
//! ## Metrics Overview
//!
//! ```text
//!     Metrics Dashboard:
//!     ┌─────────────────────────────────────┐
//!     │  Success Rate: 85%                 │
//!     │  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░  (85/100)     │
//!     │                                     │
//!     │  Token Usage: 70%                   │
//!     │  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░  (70/100)   │
//!     │                                     │
//!     │  Health: ✅ Healthy                 │
//!     │  Pressure: Low                      │
//!     │  Max Wait: 1.5ms                    │
//!     └─────────────────────────────────────┘
//! ```

use std::fmt;

/// Comprehensive metrics for rate limiter performance analysis.
///
/// This struct provides a snapshot of all important rate limiter metrics,
/// allowing you to monitor performance, detect issues, and make informed
/// decisions about capacity planning.
///
/// ## Key Metrics Explained
///
/// ### Success Metrics
/// - **total_acquired**: Successfully processed requests
/// - **total_rejected**: Requests that were rate limited
/// - **success_rate**: Percentage of successful requests
///
/// ### Capacity Metrics
/// - **current_tokens**: Available capacity right now
/// - **max_tokens**: Maximum possible capacity
/// - **utilization**: How much of the capacity is being used
///
/// ### Pressure Indicators
/// - **consecutive_rejections**: Recent rejection streak (high = pressure)
/// - **pressure_ratio**: Overall rejection ratio
/// - **max_wait_time_ns**: Longest wait observed
///
/// ## Example Usage
///
/// ```rust
/// use rater::RateLimiter;
///
/// let limiter = RateLimiter::new(100, 10);
/// // ... use the limiter ...
///
/// let metrics = limiter.metrics();
///
/// // Check health
/// if metrics.is_under_pressure() {
///     println!("⚠️ System under pressure!");
///     println!("Success rate: {:.1}%", metrics.success_rate() * 100.0);
/// }
///
/// // Display comprehensive report
/// println!("{}", metrics.summary());
/// ```
#[derive(Debug, Clone)]
pub struct RateLimiterMetrics {
    /// Total number of tokens successfully acquired.
    /// This represents the number of allowed requests.
    pub total_acquired: u64,

    /// Total number of token acquisition attempts that were rejected.
    /// This represents the number of rate-limited requests.
    pub total_rejected: u64,

    /// Total number of refill operations performed.
    /// High numbers indicate the limiter has been active for a while.
    pub total_refills: u64,

    /// Current number of available tokens in the bucket.
    /// This is the immediate capacity available.
    pub current_tokens: u64,

    /// Maximum capacity of the token bucket.
    /// This is the burst limit configured for the limiter.
    pub max_tokens: u64,

    /// Number of consecutive rejections without a successful acquisition.
    /// High values (>10) indicate sustained pressure.
    pub consecutive_rejections: u32,

    /// Maximum wait time observed in nanoseconds.
    /// Useful for identifying contention issues.
    pub max_wait_time_ns: u64,

    /// Ratio of rejected requests to total requests (0.0 to 1.0).
    /// Values above 0.3 indicate significant pressure.
    pub pressure_ratio: f64,
}

impl RateLimiterMetrics {
    /// Calculates the success rate of token acquisitions.
    ///
    /// # Returns
    ///
    /// A value between 0.0 and 1.0, where:
    /// - 1.0 = 100% success (no rejections)
    /// - 0.5 = 50% success (half rejected)
    /// - 0.0 = 0% success (all rejected)
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// let metrics = limiter.metrics();
    /// if metrics.success_rate() < 0.8 {
    ///     println!("Warning: High rejection rate!");
    /// }
    /// ```
    #[inline]
    pub fn success_rate(&self) -> f64 {
        let total = self.total_acquired + self.total_rejected;
        if total == 0 {
            1.0 // No requests yet, assume success
        } else {
            self.total_acquired as f64 / total as f64
        }
    }

    /// Calculates the rejection rate (inverse of success rate).
    ///
    /// # Returns
    ///
    /// A value between 0.0 and 1.0 representing the fraction of rejected requests.
    #[inline]
    pub fn rejection_rate(&self) -> f64 {
        1.0 - self.success_rate()
    }

    /// Determines if the rate limiter is under immediate pressure.
    ///
    /// Immediate pressure means:
    /// - Success rate below 50%, OR
    /// - No tokens currently available
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// let metrics = limiter.metrics();
    /// if metrics.is_under_pressure() {
    ///     // Consider backing off or queueing requests
    /// }
    /// ```
    #[inline]
    pub fn is_under_pressure(&self) -> bool {
        self.success_rate() < 0.5 || self.current_tokens == 0
    }

    /// Calculates the current utilization of the token bucket.
    ///
    /// Utilization shows how much of the capacity is being used:
    /// - 0.0 = Bucket is full (no usage)
    /// - 0.5 = Half capacity used
    /// - 1.0 = Bucket is empty (full usage)
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// let metrics = limiter.metrics();
    /// if metrics.utilization() > 0.9 {
    ///     println!("Running at high utilization!");
    /// }
    /// ```
    #[inline]
    pub fn utilization(&self) -> f64 {
        if self.max_tokens == 0 {
            0.0
        } else {
            1.0 - (self.current_tokens as f64 / self.max_tokens as f64)
        }
    }

    /// Returns the percentage of available tokens.
    ///
    /// This is the inverse of utilization, showing remaining capacity:
    /// - 100% = Bucket is full
    /// - 50% = Half capacity available
    /// - 0% = No tokens available
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// let metrics = limiter.metrics();
    /// println!("Available capacity: {:.1}%", metrics.availability_percentage());
    /// ```
    #[inline]
    pub fn availability_percentage(&self) -> f64 {
        if self.max_tokens == 0 {
            0.0
        } else {
            (self.current_tokens as f64 / self.max_tokens as f64) * 100.0
        }
    }

    /// Determines if the rate limiter is under sustained pressure.
    ///
    /// Sustained pressure indicates ongoing high demand that exceeds capacity.
    /// This is detected when:
    /// - More than 10 consecutive rejections, OR
    /// - Overall rejection ratio above 30%
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// let metrics = limiter.metrics();
    /// if metrics.is_under_sustained_pressure() {
    ///     // Consider scaling up capacity or implementing backpressure
    ///     println!("System needs intervention!");
    /// }
    /// ```
    #[inline]
    pub fn is_under_sustained_pressure(&self) -> bool {
        self.consecutive_rejections > 10 || self.pressure_ratio > 0.3
    }

    /// Returns the maximum wait time in microseconds.
    ///
    /// Converts nanoseconds to microseconds for easier reading.
    #[inline]
    pub fn max_wait_time_us(&self) -> f64 {
        self.max_wait_time_ns as f64 / 1000.0
    }

    /// Returns the maximum wait time in milliseconds.
    ///
    /// Converts nanoseconds to milliseconds for easier reading.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// let metrics = limiter.metrics();
    /// if metrics.max_wait_time_ms() > 10.0 {
    ///     println!("High contention detected: {:.2}ms max wait",
    ///              metrics.max_wait_time_ms());
    /// }
    /// ```
    #[inline]
    pub fn max_wait_time_ms(&self) -> f64 {
        self.max_wait_time_ns as f64 / 1_000_000.0
    }

    /// Returns the total number of requests (acquired + rejected).
    #[inline]
    pub fn total_requests(&self) -> u64 {
        self.total_acquired + self.total_rejected
    }

    /// Determines the health status of the rate limiter.
    ///
    /// Health status provides a quick assessment of the limiter's state:
    /// - **Healthy**: Operating normally
    /// - **Degraded**: Under some pressure but functional
    /// - **Critical**: Severe pressure, intervention needed
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{HealthStatus, RateLimiter};
    ///
    /// let limiter = RateLimiter::new(100, 10);
    /// let metrics = limiter.metrics();
    /// match metrics.health_status() {
    ///     HealthStatus::Healthy => println!("✅ All good"),
    ///     HealthStatus::Degraded => println!("⚠️ Monitor closely"),
    ///     HealthStatus::Critical => println!("🔴 Take action!"),
    /// }
    /// ```
    pub fn health_status(&self) -> HealthStatus {
        if self.is_under_sustained_pressure() {
            HealthStatus::Critical
        } else if self.is_under_pressure() {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        }
    }

    /// Generates a human-readable summary of the metrics.
    ///
    /// This provides a comprehensive report suitable for logging or display.
    ///
    /// # Example Output
    ///
    /// ```text
    /// RateLimiter Metrics:
    /// ├─ Performance:
    /// │  ├─ Success Rate: 85.50%
    /// │  ├─ Rejection Rate: 14.50%
    /// │  └─ Max Wait Time: 1.234ms
    /// ├─ Capacity:
    /// │  ├─ Available Tokens: 75/100
    /// │  ├─ Utilization: 25.00%
    /// │  └─ Availability: 75.00%
    /// └─ Health:
    ///    ├─ Status: Healthy
    ///    └─ Under Pressure: false
    /// ```
    pub fn summary(&self) -> String {
        format!(
            "RateLimiter Metrics:\n\
             ├─ Performance:\n\
             │  ├─ Success Rate: {:.2}%\n\
             │  ├─ Rejection Rate: {:.2}%\n\
             │  └─ Max Wait Time: {:.3}ms\n\
             ├─ Capacity:\n\
             │  ├─ Available Tokens: {}/{}\n\
             │  ├─ Utilization: {:.2}%\n\
             │  └─ Availability: {:.2}%\n\
             ├─ Counters:\n\
             │  ├─ Total Acquired: {}\n\
             │  ├─ Total Rejected: {}\n\
             │  ├─ Total Refills: {}\n\
             │  └─ Consecutive Rejections: {}\n\
             └─ Health:\n\
                ├─ Status: {:?}\n\
                ├─ Under Pressure: {}\n\
                └─ Under Sustained Pressure: {}",
            self.success_rate() * 100.0,
            self.rejection_rate() * 100.0,
            self.max_wait_time_ms(),
            self.current_tokens,
            self.max_tokens,
            self.utilization() * 100.0,
            self.availability_percentage(),
            self.total_acquired,
            self.total_rejected,
            self.total_refills,
            self.consecutive_rejections,
            self.health_status(),
            self.is_under_pressure(),
            self.is_under_sustained_pressure()
        )
    }
}

impl fmt::Display for RateLimiterMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.summary())
    }
}

/// Health status indicator for the rate limiter.
///
/// Provides a simple three-level assessment of rate limiter health,
/// making it easy to trigger alerts or take action based on status.
///
/// ## Status Levels
///
/// ```text
///     Healthy ──────► Normal operation, plenty of capacity
///        │
///     Degraded ─────► Some pressure, monitor closely
///        │
///     Critical ─────► Severe pressure, immediate action needed
/// ```
///
/// ## Example Usage
///
/// ```rust
/// use tracing::{error, warn};
/// use rater::{RateLimiter, HealthStatus};
///
/// let limiter = RateLimiter::new(100, 10);
/// // ... heavy usage ...
///
/// let metrics = limiter.metrics();
/// let health = metrics.health_status();
///
/// // Take action based on health
/// match health {
///     HealthStatus::Healthy => {
///         // Normal operation
///     }
///     HealthStatus::Degraded => {
///         // Log warning, consider scaling
///         warn!("Rate limiter degraded: {}", health.suggested_action());
///     }
///     HealthStatus::Critical => {
///         // Alert on-call, scale immediately
///         error!("Rate limiter critical: {}", health.suggested_action());
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// Operating normally with good success rates.
    ///
    /// Indicates:
    /// - Success rate above 50%
    /// - Tokens available
    /// - Low rejection count
    Healthy,

    /// Under some pressure but still functional.
    ///
    /// Indicates:
    /// - Success rate below 50% OR
    /// - No tokens currently available
    /// - System can recover if load decreases
    Degraded,

    /// Under severe pressure, intervention recommended.
    ///
    /// Indicates:
    /// - Sustained high rejection rate (>30%) OR
    /// - Many consecutive rejections (>10)
    /// - System needs scaling or load reduction
    Critical,
}

impl HealthStatus {
    /// Returns true if the status indicates any problems.
    ///
    /// Useful for simple health checks.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let rater = RateLimiter::new(100, 10);
    /// let health  = rater.metrics().health_status();
    /// if health.is_unhealthy() {
    ///     // Take corrective action
    ///     true;
    /// }
    /// ```
    pub fn is_unhealthy(&self) -> bool {
        !matches!(self, Self::Healthy)
    }

    /// Returns a suggested action based on the health status.
    ///
    /// Provides actionable guidance for operators.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::RateLimiter;
    ///
    /// let rater = RateLimiter::new(100, 10);
    /// let health  = rater.metrics().health_status();
    /// println!("Recommendation: {}", health.suggested_action());
    /// ```
    pub fn suggested_action(&self) -> &'static str {
        match self {
            Self::Healthy => "No action needed",
            Self::Degraded => "Monitor closely, consider increasing capacity",
            Self::Critical => "Immediate action required: scale up or reduce load",
        }
    }
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => write!(f, "✅ Healthy"),
            Self::Degraded => write!(f, "⚠️ Degraded"),
            Self::Critical => write!(f, "🔴 Critical"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_calculations() {
        let metrics = RateLimiterMetrics {
            total_acquired: 80,
            total_rejected: 20,
            total_refills: 10,
            current_tokens: 25,
            max_tokens: 100,
            consecutive_rejections: 5,
            max_wait_time_ns: 1_000_000,
            pressure_ratio: 0.2,
        };

        assert_eq!(metrics.success_rate(), 0.8);
        assert_eq!(metrics.utilization(), 0.75);
        assert!(!metrics.is_under_pressure());
        assert_eq!(metrics.health_status(), HealthStatus::Healthy);
    }

    #[test]
    fn test_health_status() {
        let metrics = RateLimiterMetrics {
            total_acquired: 40,
            total_rejected: 60,
            total_refills: 10,
            current_tokens: 0,
            max_tokens: 100,
            consecutive_rejections: 15,
            max_wait_time_ns: 0,
            pressure_ratio: 0.6,
        };

        assert!(metrics.is_under_pressure());
        assert!(metrics.is_under_sustained_pressure());
        assert_eq!(metrics.health_status(), HealthStatus::Critical);
    }

    #[test]
    fn test_edge_cases() {
        // Test with zero totals
        let metrics = RateLimiterMetrics {
            total_acquired: 0,
            total_rejected: 0,
            total_refills: 0,
            current_tokens: 50,
            max_tokens: 100,
            consecutive_rejections: 0,
            max_wait_time_ns: 0,
            pressure_ratio: 0.0,
        };

        assert_eq!(metrics.success_rate(), 1.0);
        assert_eq!(metrics.utilization(), 0.5);
        assert!(!metrics.is_under_pressure());

        // Test with max_tokens = 0
        let metrics = RateLimiterMetrics {
            total_acquired: 0,
            total_rejected: 0,
            total_refills: 0,
            current_tokens: 0,
            max_tokens: 0,
            consecutive_rejections: 0,
            max_wait_time_ns: 0,
            pressure_ratio: 0.0,
        };

        assert_eq!(metrics.utilization(), 0.0);
        assert_eq!(metrics.availability_percentage(), 0.0);
    }
    #[test]
    fn test_health_status_methods() {
        assert!(!HealthStatus::Healthy.is_unhealthy());
        assert!(HealthStatus::Degraded.is_unhealthy());
        assert!(HealthStatus::Critical.is_unhealthy());

        assert_eq!(HealthStatus::Healthy.suggested_action(), "No action needed");
        assert!(HealthStatus::Degraded
            .suggested_action()
            .contains("Monitor"));
        assert!(HealthStatus::Critical
            .suggested_action()
            .contains("Immediate"));
    }

    #[test]
    fn test_health_status_display() {
        let healthy = format!("{}", HealthStatus::Healthy);
        assert!(healthy.contains("Healthy"));

        let degraded = format!("{}", HealthStatus::Degraded);
        assert!(degraded.contains("Degraded"));

        let critical = format!("{}", HealthStatus::Critical);
        assert!(critical.contains("Critical"));
    }

    #[test]
    fn test_metrics_display() {
        let metrics = RateLimiterMetrics {
            total_acquired: 100,
            total_rejected: 20,
            total_refills: 5,
            current_tokens: 30,
            max_tokens: 100,
            consecutive_rejections: 0,
            max_wait_time_ns: 1_500_000,
            pressure_ratio: 0.1,
        };

        let display = format!("{}", metrics);
        assert!(display.contains("RateLimiter Metrics"));
        assert!(display.contains("Success Rate"));

        let summary = metrics.summary();
        assert!(summary.contains("Performance"));
        assert!(summary.contains("Capacity"));
        assert!(summary.contains("Health"));
    }

    #[test]
    fn test_metrics_time_conversions() {
        let metrics = RateLimiterMetrics {
            total_acquired: 0,
            total_rejected: 0,
            total_refills: 0,
            current_tokens: 0,
            max_tokens: 100,
            consecutive_rejections: 0,
            max_wait_time_ns: 1_500_000_000, // 1.5 seconds
            pressure_ratio: 0.0,
        };

        assert_eq!(metrics.max_wait_time_us(), 1_500_000.0);
        assert_eq!(metrics.max_wait_time_ms(), 1_500.0);
    }

    #[test]
    fn test_total_requests() {
        let metrics = RateLimiterMetrics {
            total_acquired: 75,
            total_rejected: 25,
            total_refills: 0,
            current_tokens: 0,
            max_tokens: 100,
            consecutive_rejections: 0,
            max_wait_time_ns: 0,
            pressure_ratio: 0.0,
        };

        assert_eq!(metrics.total_requests(), 100);
    }

    #[test]
    fn test_degraded_status() {
        // Under pressure but not sustained: success < 50% but low consecutive rejections
        let metrics = RateLimiterMetrics {
            total_acquired: 30,
            total_rejected: 70,
            total_refills: 0,
            current_tokens: 5,
            max_tokens: 100,
            consecutive_rejections: 3,
            max_wait_time_ns: 0,
            pressure_ratio: 0.1,
        };

        assert!(metrics.is_under_pressure());
        assert!(!metrics.is_under_sustained_pressure());
        assert_eq!(metrics.health_status(), HealthStatus::Degraded);
    }

    #[test]
    fn test_degraded_by_empty_tokens() {
        // Tokens empty but success rate is still high
        let metrics = RateLimiterMetrics {
            total_acquired: 90,
            total_rejected: 10,
            total_refills: 5,
            current_tokens: 0,
            max_tokens: 100,
            consecutive_rejections: 2,
            max_wait_time_ns: 0,
            pressure_ratio: 0.1,
        };

        assert!(metrics.is_under_pressure());
        assert_eq!(metrics.health_status(), HealthStatus::Degraded);
    }

    #[test]
    fn test_rejection_rate() {
        let metrics = RateLimiterMetrics {
            total_acquired: 60,
            total_rejected: 40,
            total_refills: 0,
            current_tokens: 50,
            max_tokens: 100,
            consecutive_rejections: 0,
            max_wait_time_ns: 0,
            pressure_ratio: 0.0,
        };

        assert!((metrics.rejection_rate() - 0.4).abs() < 0.001);
        assert!((metrics.success_rate() - 0.6).abs() < 0.001);
    }

    #[test]
    fn test_sustained_pressure_by_consecutive_rejections() {
        let metrics = RateLimiterMetrics {
            total_acquired: 90,
            total_rejected: 10,
            total_refills: 0,
            current_tokens: 0,
            max_tokens: 100,
            consecutive_rejections: 15,
            max_wait_time_ns: 0,
            pressure_ratio: 0.05,
        };

        assert!(metrics.is_under_sustained_pressure());
        assert_eq!(metrics.health_status(), HealthStatus::Critical);
    }

    #[test]
    fn test_sustained_pressure_by_pressure_ratio() {
        let metrics = RateLimiterMetrics {
            total_acquired: 60,
            total_rejected: 40,
            total_refills: 0,
            current_tokens: 50,
            max_tokens: 100,
            consecutive_rejections: 0,
            max_wait_time_ns: 0,
            pressure_ratio: 0.4,
        };

        assert!(metrics.is_under_sustained_pressure());
    }

    #[test]
    fn test_utilization_boundaries() {
        // Full bucket = 0% utilization
        let full = RateLimiterMetrics {
            total_acquired: 0, total_rejected: 0, total_refills: 0,
            current_tokens: 100, max_tokens: 100,
            consecutive_rejections: 0, max_wait_time_ns: 0, pressure_ratio: 0.0,
        };
        assert_eq!(full.utilization(), 0.0);
        assert_eq!(full.availability_percentage(), 100.0);

        // Empty bucket = 100% utilization
        let empty = RateLimiterMetrics {
            total_acquired: 0, total_rejected: 0, total_refills: 0,
            current_tokens: 0, max_tokens: 100,
            consecutive_rejections: 0, max_wait_time_ns: 0, pressure_ratio: 0.0,
        };
        assert_eq!(empty.utilization(), 1.0);
        assert_eq!(empty.availability_percentage(), 0.0);
    }

    #[test]
    fn test_metrics_clone() {
        let metrics = RateLimiterMetrics {
            total_acquired: 42, total_rejected: 13, total_refills: 7,
            current_tokens: 30, max_tokens: 100,
            consecutive_rejections: 2, max_wait_time_ns: 500, pressure_ratio: 0.1,
        };
        let cloned = metrics.clone();

        assert_eq!(cloned.total_acquired, 42);
        assert_eq!(cloned.total_rejected, 13);
        assert_eq!(cloned.current_tokens, 30);
    }

    #[test]
    fn test_health_status_clone_eq() {
        let h1 = HealthStatus::Healthy;
        let h2 = h1;
        assert_eq!(h1, h2);

        let h3 = HealthStatus::Critical;
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_summary_contains_all_fields() {
        let metrics = RateLimiterMetrics {
            total_acquired: 50, total_rejected: 10, total_refills: 3,
            current_tokens: 40, max_tokens: 100,
            consecutive_rejections: 1, max_wait_time_ns: 2_000_000, pressure_ratio: 0.1,
        };

        let summary = metrics.summary();
        assert!(summary.contains("50"));  // total_acquired
        assert!(summary.contains("10"));  // total_rejected
        assert!(summary.contains("3"));   // total_refills
        assert!(summary.contains("40/100")); // tokens
        assert!(summary.contains("Consecutive Rejections: 1"));
    }
}
