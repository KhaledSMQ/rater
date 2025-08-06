//! # Rate Limiter Module
//!
//! This module provides the internal implementation of the rate limiting functionality.
//! It's organized into several submodules, each responsible for a specific aspect
//! of the rate limiting system.
//!
//! ## Module Structure
//!
//! ```text
//!     rate_limiter/
//!     ├── mod.rs          (You are here - Module organization)
//!     ├── config.rs       (Configuration and settings)
//!     ├── core.rs         (Core token bucket implementation)
//!     ├── manager.rs      (IP-based rate limiter management)
//!     ├── metrics.rs      (Performance monitoring)
//!     └── utils.rs        (Platform-specific optimizations)
//! ```
//!
//! ## Architecture Flow
//!
//! ```text
//!     User Request
//!          │
//!          ▼
//!     ┌─────────┐
//!     │ Manager │ ◄── Per-IP rate limiting
//!     └────┬────┘
//!          │
//!          ▼
//!     ┌─────────┐
//!     │  Core   │ ◄── Token bucket algorithm
//!     └────┬────┘
//!          │
//!          ▼
//!     ┌─────────┐
//!     │ Config  │ ◄── Settings & validation
//!     └────┬────┘
//!          │
//!          ▼
//!     ┌─────────┐
//!     │  Utils  │ ◄── CPU optimizations
//!     └─────────┘
//! ```
//!
//! ## Component Responsibilities
//!
//! - **config**: Defines how the rate limiter behaves (tokens, refill rate, etc.)
//! - **core**: Implements the lock-free token bucket using atomic operations
//! - **manager**: Manages multiple rate limiters for different IPs
//! - **metrics**: Tracks performance and health statistics
//! - **utils**: Provides platform-specific optimizations and helpers

// Declare submodules (internal organization)
mod config;
mod core;
mod manager;
mod metrics;
mod utils;

// Re-export public types for external use
// These are the types that users of the library will interact with

/// Configuration types for customizing rate limiter behavior
pub use config::{MemoryOrdering, RateLimiterConfig, MAX_REFILL_PERIODS};

/// Core rate limiter implementing the token bucket algorithm
pub use core::RateLimiter;

/// IP-based rate limiting manager for per-client limits
pub use manager::{IpRateLimiterManager, ManagerStats};

/// Metrics and health monitoring for observability
pub use metrics::{HealthStatus, RateLimiterMetrics};

/// Utility functions for time and CPU optimizations
pub use utils::{cpu_relax, current_time_ms, current_time_ns, current_time_us};
