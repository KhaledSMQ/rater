# Rater

[![Crates.io](https://img.shields.io/crates/v/rater.svg)](https://crates.io/crates/rater)
[![Documentation](https://docs.rs/rater/badge.svg)](https://docs.rs/rater)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Build Status](https://img.shields.io/github/actions/workflow/status/khaledsmq/rater/ci.yml?branch=main)](https://github.com/khaledsmq/rater/actions)
[![MSRV](https://img.shields.io/badge/MSRV-1.70.0-blue)](https://github.com/khaledsmq/rater)

A high-performance, lock-free rate limiting library for Rust. It uses the token bucket algorithm under the hood and comes with built-in per-IP rate limiting, so you can drop it into a web server and start throttling requests in minutes.

## Features

- **Lock-free & thread-safe** -- atomic CAS operations instead of mutexes, so threads never block each other
- **Fast** -- cache-aligned structures, platform-specific hints (PAUSE on x86, YIELD on ARM), ~29 ns per `try_acquire` call
- **Adaptive refill** -- automatically adjusts refill rate when your system is under sustained pressure
- **Per-IP rate limiting** -- built-in `IpRateLimiterManager` with automatic cleanup of idle clients
- **Real-time metrics** -- success rates, pressure detection, health status -- all available at a glance
- **Lightweight** -- just three dependencies: [dashmap](https://crates.io/crates/dashmap), [ahash](https://crates.io/crates/ahash), and [tracing](https://crates.io/crates/tracing)
- **Safe Rust** -- `unsafe` is used only for platform-specific CPU pause instructions

## Installation

Add `rater` to your `Cargo.toml`:

```toml
[dependencies]
rater = "0.1.2"
```

If you also want serde serialization support:

```toml
[dependencies]
rater = { version = "0.1.2", features = ["full"] }
```

## Quick Start

### Basic Rate Limiting

```rust
use rater::RateLimiter;

fn main() {
    // 100 token capacity, refills 10 tokens every second
    let limiter = RateLimiter::new(100, 10);

    if limiter.try_acquire() {
        println!("Request allowed!");
    } else {
        println!("Rate limited -- try again later");
    }

    // Acquire multiple tokens at once (handy for batch/expensive operations)
    if limiter.try_acquire_n(5) {
        println!("Batch request allowed!");
    }

    println!("Available tokens: {}", limiter.available_tokens());
}
```

### Per-IP Rate Limiting

Perfect for web servers -- each client IP gets its own token bucket:

```rust
use rater::{IpRateLimiterManager, RateLimiterConfig};
use std::net::IpAddr;
use std::sync::Arc;

fn main() {
    let config = RateLimiterConfig::per_second(10); // 10 req/s per IP
    let manager = Arc::new(IpRateLimiterManager::new(config));

    // Start a background cleanup thread (removes idle IPs automatically)
    let (handle, stop_tx) = manager.clone().start_stoppable_cleanup_thread();

    let client_ip: IpAddr = "192.168.1.100".parse().unwrap();

    if manager.try_acquire(client_ip) {
        println!("Request from {} allowed", client_ip);
    } else {
        println!("Request from {} rate limited", client_ip);
    }

    // Check how things are going
    let stats = manager.stats();
    println!("{}", stats.summary());

    // When you're done, shut down the cleanup thread gracefully
    let _ = stop_tx.send(());
    let _ = handle.join();
}
```

### Builder Pattern

For full control over every knob:

```rust
use rater::{RateLimiterBuilder, MemoryOrdering};

fn main() {
    let limiter = RateLimiterBuilder::new()
        .max_tokens(1000)
        .refill_rate(100)
        .refill_interval_ms(1000)
        .memory_ordering(MemoryOrdering::AcquireRelease)
        .build();

    if limiter.try_acquire() {
        println!("Request processed!");
    }
}
```

## Advanced Configuration

### Rate Limiting Strategies

```rust
use rater::{RateLimiterConfig, MemoryOrdering};

// 100 requests per second (with 2x burst capacity by default)
let config = RateLimiterConfig::per_second(100);

// 1,000 requests per minute
let config = RateLimiterConfig::per_minute(1000);

// Full manual control: 500 max tokens, refill 50 tokens every 1000 ms
let config = RateLimiterConfig::new(500, 50, 1000)
    .with_burst_multiplier(3)                   // allow bursts up to 3x the refill rate
    .with_ordering(MemoryOrdering::Sequential);  // strongest memory ordering
```

### Monitoring and Metrics

```rust
use rater::RateLimiter;

let limiter = RateLimiter::new(100, 10);

for _ in 0..50 {
    limiter.try_acquire();
}

let metrics = limiter.metrics();
println!("Success rate: {:.2}%", metrics.success_rate() * 100.0);
println!("Current tokens: {}/{}", metrics.current_tokens, metrics.max_tokens);
println!("Health status: {:?}", metrics.health_status());
println!("{}", metrics.summary());
```

### Custom Cleanup Settings

Control how aggressively idle IP entries are pruned:

```rust
use rater::{IpRateLimiterManager, RateLimiterConfig};
use std::sync::Arc;

let config = RateLimiterConfig::per_second(10);
let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
    config,
    60_000,    // run cleanup every 60 seconds
    300_000,   // evict IPs inactive for 5 minutes
));

let (handle, stop_tx) = manager.clone().start_stoppable_cleanup_thread();

// ... use the manager ...

// Shut down gracefully
stop_tx.send(()).unwrap();
handle.join().unwrap();
```

## Performance

Rater is built for high-concurrency scenarios. Here are some highlights from the benchmark suite (Apple M-series, `cargo bench`):

| Operation | Latency | Throughput |
|---|---|---|
| `try_acquire()` (single thread) | ~29 ns | ~35 M ops/s |
| `try_acquire_n(50)` (single thread) | ~30 ns | ~1.67 G tokens/s |
| `get_limiter()` (IP lookup) | ~10 ns | — |
| `try_acquire()` per IP | ~38 ns | — |
| Concurrent acquire (2 threads) | ~90 µs / 2k ops | ~22 M ops/s |
| Concurrent acquire (8 threads) | ~1.2 ms / 8k ops | ~6.4 M ops/s |
| IP manager (8 threads) | ~122 µs / 8k ops | — |
| Metrics snapshot | ~2.3 ns | — |

Design choices that make this fast:

- **Lock-free CAS loops** with bounded retries (no infinite spinning)
- **Cache-aligned structs** to prevent false sharing across cores
- **Platform-specific spin hints** (x86 PAUSE / ARM YIELD)
- **Adaptive refill** that backs off under sustained pressure

Run the benchmarks yourself:

```bash
cargo bench
```

## Architecture

### Core Components

1. **`RateLimiter`** -- the lock-free token bucket
   - Atomic token counter (u64 on 64-bit, u32 on 32-bit)
   - Time-based auto-refill with adaptive backoff
   - Backpressure detection

2. **`IpRateLimiterManager`** -- per-IP rate limiting
   - Sharded concurrent hash map ([DashMap](https://crates.io/crates/dashmap)) for IP tracking
   - Automatic + emergency cleanup when approaching capacity (10k IPs max)
   - LRU-like eviction of idle entries

3. **Metrics** -- real-time health monitoring
   - Success / rejection rates and counts
   - Pressure ratio and consecutive-rejection streaks
   - Health status enum (`Healthy`, `Warning`, `Critical`)

### Memory Ordering

Pick the trade-off that fits your use case:

| Ordering | When to use |
|---|---|
| `Relaxed` | Maximum speed, approximate counts are fine |
| `AcquireRelease` | **(default)** Good balance of speed and correctness |
| `Sequential` | Strict ordering required (debugging, auditing) |

## Use Cases

- **API gateways** -- throttle requests per client IP
- **Microservices** -- backpressure and circuit-breaker patterns
- **Game servers** -- prevent spam and abuse
- **IoT gateways** -- control device message rates
- **Background workers** -- throttle calls to external APIs

## Examples

The [`examples/`](examples/) directory has runnable demos:

- [`basic.rs`](examples/basic.rs) -- simple rate limiting
- [`ip_limiting.rs`](examples/ip_limiting.rs) -- per-IP rate limiting with monitoring

```bash
cargo run --example basic
cargo run --example ip_limiting
```

## Testing

```bash
cargo test                # run the full test suite
cargo test --all-features # include serde tests
cargo bench               # run benchmarks
```

## Contributing

Contributions are welcome! Feel free to open an issue or submit a PR.

1. Fork the repo
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

Licensed under either of:

- [MIT license](http://opensource.org/licenses/MIT)
- [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)

at your option. See [LICENSE](LICENSE) for details.

---

Made with care by [Khaled](https://github.com/khaledsmq)
