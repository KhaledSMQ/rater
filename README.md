# Rater 🚀

[![Crates.io](https://img.shields.io/crates/v/rater.svg)](https://crates.io/crates/rater)
[![Documentation](https://docs.rs/rater/badge.svg)](https://docs.rs/rater)
[![License](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Build Status](https://img.shields.io/github/actions/workflow/status/khaledsmq/rater/ci.yml?branch=main)](https://github.com/khaledsmq/rater/actions)
[![MSRV](https://img.shields.io/badge/MSRV-1.70.0-blue)](https://github.com/khaledsmq/rater)

A blazingly fast, lock-free, thread-safe rate limiting library for Rust implementing the token bucket algorithm with optional per-IP rate limiting support.

## ✨ Features

- 🔒 **Lock-free Implementation** - Uses atomic operations for thread-safe token management without mutex overhead
- ⚡ **High Performance** - Cache-aligned structures and platform-specific optimizations (PAUSE on x86, YIELD on ARM)
- 🔄 **Adaptive Refill** - Automatically adjusts refill rate under sustained pressure
- 🌐 **Per-IP Rate Limiting** - Built-in IP-based rate limiter management with automatic cleanup
- 📊 **Comprehensive Metrics** - Real-time performance monitoring and health status tracking
- 🎯 **Zero Dependencies** - Minimal dependencies (only dashmap for IP management)
- 🦀 **100% Safe Rust** - Unsafe code only for platform-specific CPU instructions

## 📦 Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
rater = "0.1.0"
```

For all features including serialization support:

```toml
[dependencies]
rater = { version = "0.1.0", features = ["full"] }
```

## 🚀 Quick Start

### Basic Rate Limiting

```rust
use rater::RateLimiter;

fn main() {
    // Create a rate limiter with 100 tokens, refilling 10 tokens/second
    let limiter = RateLimiter::new(100, 10);

    // Try to acquire a single token
    if limiter.try_acquire() {
        println!("Request allowed!");
    } else {
        println!("Rate limited!");
    }

    // Try to acquire multiple tokens at once
    if limiter.try_acquire_n(5) {
        println!("Batch request allowed!");
    }

    // Check available tokens
    println!("Available tokens: {}", limiter.available_tokens());
}
```

### Per-IP Rate Limiting

```rust
use rater::IpRateLimiterManager;
use std::net::IpAddr;
use std::sync::Arc;

fn main() {
    // Create a manager for per-IP rate limiting
    let config = rater::RateLimiterConfig::per_second(10); // 10 requests per second
    let manager = Arc::new(IpRateLimiterManager::new(config));

    // Start automatic cleanup thread
    let manager_clone = manager.clone();
    manager_clone.start_cleanup_thread();

    // Handle incoming requests
    let client_ip: IpAddr = "192.168.1.100".parse().unwrap();
    
    if manager.try_acquire(client_ip) {
        println!("Request from {} allowed", client_ip);
    } else {
        println!("Request from {} rate limited", client_ip);
    }

    // Get statistics
    let stats = manager.stats();
    println!("{}", stats.summary());
}
```

### Using the Builder Pattern

```rust
use rater::{RateLimiterBuilder, MemoryOrdering};

fn main() {
    let limiter = RateLimiterBuilder::new()
        .max_tokens(1000)
        .refill_rate(100)
        .refill_interval_ms(1000)
        .memory_ordering(MemoryOrdering::AcquireRelease)
        .build();

    // Use the configured limiter
    if limiter.try_acquire() {
        println!("Request processed!");
    }
}
```

## 🔧 Advanced Configuration

### Custom Rate Limiting Strategies

```rust
use rater::RateLimiterConfig;

// Per-second rate limiting
let config = RateLimiterConfig::per_second(100);

// Per-minute rate limiting
let config = RateLimiterConfig::per_minute(1000);

// Custom configuration with burst capacity
let config = RateLimiterConfig::new(500, 50, 1000)  // 500 max tokens, 50 refill rate, 1000ms interval
    .with_burst_multiplier(3)  // Allow bursts up to 3x the normal rate
    .with_ordering(MemoryOrdering::Sequential);  // Strongest memory ordering
```

### Monitoring and Metrics

```rust
use rater::RateLimiter;

let limiter = RateLimiter::new(100, 10);

// Perform some operations
for _ in 0..50 {
    limiter.try_acquire();
}

// Get comprehensive metrics
let metrics = limiter.metrics();
println!("Success rate: {:.2}%", metrics.success_rate() * 100.0);
println!("Current tokens: {}/{}", metrics.current_tokens, metrics.max_tokens);
println!("Health status: {:?}", metrics.health_status());

// Get a detailed summary
println!("{}", metrics.summary());
```

### IP Manager with Custom Cleanup Settings

```rust
use rater::{IpRateLimiterManager, RateLimiterConfig};
use std::sync::Arc;

let config = RateLimiterConfig::per_second(10);
let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
    config,
    60_000,    // Cleanup every 60 seconds
    300_000,   // Remove limiters inactive for 5 minutes
));

// Start a stoppable cleanup thread
let (handle, stop_tx) = manager.clone().start_stoppable_cleanup_thread();

// ... use the manager ...

// Stop the cleanup thread when done
stop_tx.send(()).unwrap();
handle.join().unwrap();
```

## 🏎️ Performance

Rater is designed for extreme performance in high-concurrency scenarios:

- **Lock-free algorithm** using atomic CAS operations
- **Cache-aligned structures** to prevent false sharing between CPU cores
- **Platform-specific optimizations**:
    - x86_64: Uses PAUSE instruction for efficient spin-waiting
    - ARM64: Uses YIELD instruction
- **Bounded CAS retries** to prevent infinite spinning under extreme contention
- **Adaptive refill** automatically reduces refill rate under sustained pressure

### Benchmark

```
TODO
```

*Benchmarks run on AMD Ryzen 9 5900X, 32GB RAM*

## 🏗️ Architecture

### Core Components

1. **RateLimiter**: Lock-free token bucket implementation
    - Atomic token counter with platform-specific sizing (u64 on 64-bit, u32 on 32-bit)
    - Adaptive refill mechanism
    - Backpressure detection

2. **IpRateLimiterManager**: Per-IP rate limiting
    - Concurrent hash map for IP tracking
    - Automatic cleanup of inactive limiters
    - Emergency cleanup under memory pressure

3. **Metrics & Monitoring**: Real-time performance tracking
    - Success/rejection rates
    - Pressure detection
    - Health status monitoring

### Memory Ordering Options

Choose the appropriate memory ordering for your use case:

```rust
use rater::MemoryOrdering;

// Best performance, minimal guarantees
MemoryOrdering::Relaxed

// Balanced performance and correctness (default)
MemoryOrdering::AcquireRelease  

// Strongest guarantees, lowest performance
MemoryOrdering::Sequential
```

## 📊 Use Cases

- **Web Services**: Rate limit API endpoints per client IP
- **Microservices**: Implement circuit breakers and backpressure
- **Game Servers**: Prevent spam and DoS attacks
- **IoT Gateways**: Control device message rates
- **Proxy Servers**: Implement fair resource allocation
- **Background Jobs**: Throttle external API calls

## 🔍 Examples

Check out the [examples](examples/) directory for more detailed usage:

- [basic.rs](examples/basic.rs) - Simple rate limiting example
- [ip_limiting.rs](examples/ip_limiting.rs) - Per-IP rate limiting with monitoring

## 🧪 Testing

Run the test suite:

```bash
# Run all tests
cargo test

# Run with all features
cargo test --all-features

# Run benchmarks
cargo bench
```

## 📈 Benchmarking

The library includes comprehensive benchmarks:

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench --bench rate_limiter
```

## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 📝 License

MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## 🙏 Acknowledgments

- Inspired by various rate limiting implementations in the Rust ecosystem
- Thanks to the Rust community for excellent tooling and documentation

---

Made with ❤️ by [Khaled](https://github.com/khaledsmq)