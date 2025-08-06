//! # Rate Limiter Benchmarks
//!
//! Comprehensive performance benchmarks for the rate limiter library.
//!
//! Run with: `cargo bench`

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use rater::{IpRateLimiterManager, MemoryOrdering, RateLimiter, RateLimiterConfig};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Benchmark single-threaded token acquisition
fn bench_single_acquire(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_acquire");

    for tokens in [100, 1000, 10000] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(tokens),
            &tokens,
            |b, &tokens| {
                let limiter = RateLimiter::new(tokens, 100);
                b.iter(|| std::hint::black_box(limiter.try_acquire()));
            },
        );
    }

    group.finish();
}

/// Benchmark bulk token acquisition
fn bench_bulk_acquire(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_acquire");

    for n in [1, 5, 10, 20, 50] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let limiter = RateLimiter::new(10000, 1000);
            b.iter(|| std::hint::black_box(limiter.try_acquire_n(n)));
        });
    }

    group.finish();
}

/// Benchmark different memory orderings
fn bench_memory_orderings(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_orderings");

    let orderings = [
        ("Relaxed", MemoryOrdering::Relaxed),
        ("AcquireRelease", MemoryOrdering::AcquireRelease),
        ("Sequential", MemoryOrdering::Sequential),
    ];

    for (name, ordering) in orderings {
        group.bench_function(name, |b| {
            let config = RateLimiterConfig::new(1000, 100, 1000).with_ordering(ordering);
            let limiter = RateLimiter::with_config(config);

            b.iter(|| std::hint::black_box(limiter.try_acquire()));
        });
    }

    group.finish();
}

/// Benchmark concurrent token acquisition
fn bench_concurrent_acquire(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_acquire");

    for num_threads in [2, 4, 8, 16] {
        group.throughput(Throughput::Elements(num_threads as u64 * 1000));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_threads", num_threads)),
            &num_threads,
            |b, &num_threads| {
                let limiter = Arc::new(RateLimiter::new(1_000_000, 10000));

                b.iter_custom(|iters| {
                    let mut total_duration = Duration::ZERO;

                    for _ in 0..iters {
                        limiter.reset(); // Reset between iterations
                        let limiter_clone = limiter.clone();

                        let start = std::time::Instant::now();

                        let handles: Vec<_> = (0..num_threads)
                            .map(|_| {
                                let limiter = limiter_clone.clone();
                                thread::spawn(move || {
                                    for _ in 0..1000 {
                                        limiter.try_acquire();
                                    }
                                })
                            })
                            .collect();

                        for handle in handles {
                            handle.join().unwrap();
                        }

                        total_duration += start.elapsed();
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

/// Benchmark high contention scenarios
fn bench_high_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("high_contention");

    group.bench_function("high_contention_32_threads", |b| {
        let limiter = Arc::new(RateLimiter::new(10000, 1000));

        b.iter_custom(|iters| {
            let mut total_duration = Duration::ZERO;

            for _ in 0..iters {
                limiter.reset();
                let limiter_clone = limiter.clone();

                let start = std::time::Instant::now();

                let handles: Vec<_> = (0..32)
                    .map(|_| {
                        let limiter = limiter_clone.clone();
                        thread::spawn(move || {
                            for _ in 0..100 {
                                limiter.try_acquire_n(5);
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    handle.join().unwrap();
                }

                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

/// Benchmark refill operations
fn bench_refill(c: &mut Criterion) {
    let mut group = c.benchmark_group("refill");

    group.bench_function("refill_check", |b| {
        let config = RateLimiterConfig::new(100, 10, 1);
        let limiter = RateLimiter::with_config(config);

        // Drain all tokens
        for _ in 0..100 {
            limiter.try_acquire();
        }

        b.iter(|| {
            // This will trigger refill check and potentially refill
            std::hint::black_box(limiter.available_tokens())
        });
    });

    group.bench_function("manual_add_tokens", |b| {
        let limiter = RateLimiter::new(100, 10);

        b.iter(|| {
            limiter.add_tokens(10);
        });
    });

    group.finish();
}

/// Benchmark metrics collection
fn bench_metrics(c: &mut Criterion) {
    let mut group = c.benchmark_group("metrics");

    group.bench_function("get_metrics", |b| {
        let limiter = RateLimiter::new(1000, 100);

        // Generate some activity
        for _ in 0..500 {
            limiter.try_acquire();
        }

        b.iter(|| std::hint::black_box(limiter.metrics()));
    });

    group.bench_function("calculate_pressure", |b| {
        let limiter = RateLimiter::new(100, 10);

        // Create some pressure
        for _ in 0..100 {
            limiter.try_acquire();
        }
        for _ in 0..50 {
            limiter.try_acquire(); // These will be rejected
        }

        b.iter(|| {
            let metrics = limiter.metrics();
            std::hint::black_box(metrics.is_under_pressure())
        });
    });

    group.finish();
}

/// Benchmark IP rate limiter manager
fn bench_ip_manager(c: &mut Criterion) {
    let mut group = c.benchmark_group("ip_manager");

    group.bench_function("get_limiter", |b| {
        let config = RateLimiterConfig::new(100, 10, 1000);
        let manager = IpRateLimiterManager::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        b.iter(|| std::hint::black_box(manager.get_limiter(ip)));
    });

    group.bench_function("try_acquire_ip", |b| {
        let config = RateLimiterConfig::new(1000, 100, 1000);
        let manager = IpRateLimiterManager::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        b.iter(|| std::hint::black_box(manager.try_acquire(ip)));
    });

    group.bench_function("multiple_ips", |b| {
        let config = RateLimiterConfig::new(100, 10, 1000);
        let manager = IpRateLimiterManager::new(config);
        let mut counter = 0u8;

        b.iter(|| {
            counter = counter.wrapping_add(1);
            let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, counter));
            std::hint::black_box(manager.try_acquire(ip))
        });
    });

    group.finish();
}

/// Benchmark concurrent IP manager access
fn bench_ip_manager_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("ip_manager_concurrent");

    for num_threads in [4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_threads", num_threads)),
            &num_threads,
            |b, &num_threads| {
                let config = RateLimiterConfig::new(1000, 100, 1000);
                let manager = Arc::new(IpRateLimiterManager::new(config));

                b.iter_custom(|iters| {
                    let mut total_duration = Duration::ZERO;

                    for _ in 0..iters {
                        let manager_clone = manager.clone();

                        let start = std::time::Instant::now();

                        let handles: Vec<_> = (0..num_threads)
                            .map(|thread_id| {
                                let manager = manager_clone.clone();
                                thread::spawn(move || {
                                    let ip = IpAddr::V4(Ipv4Addr::new(
                                        10,
                                        0,
                                        0,
                                        (thread_id % 256) as u8,
                                    ));
                                    for _ in 0..100 {
                                        manager.try_acquire(ip);
                                    }
                                })
                            })
                            .collect();

                        for handle in handles {
                            handle.join().unwrap();
                        }

                        total_duration += start.elapsed();
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

/// Benchmark IP manager cleanup operations
fn bench_ip_manager_cleanup(c: &mut Criterion) {
    let mut group = c.benchmark_group("ip_manager_cleanup");

    group.bench_function("cleanup_100_ips", |b| {
        let config = RateLimiterConfig::new(100, 10, 1000);
        let manager = IpRateLimiterManager::with_cleanup_settings(
            config, 1000, 1, // Very short inactive duration for benchmark
        );

        b.iter_batched(
            || {
                // Setup: Add 100 IPs
                for i in 0..100 {
                    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, i));
                    manager.get_limiter(ip);
                }
                // Wait to make them inactive
                thread::sleep(Duration::from_millis(5));
            },
            |_| {
                manager.cleanup();
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

/// Benchmark worst-case scenarios
fn bench_worst_case(c: &mut Criterion) {
    let mut group = c.benchmark_group("worst_case");

    // Benchmark when all tokens are exhausted
    group.bench_function("exhausted_limiter", |b| {
        let limiter = RateLimiter::new(100, 1);

        // Exhaust all tokens
        for _ in 0..100 {
            limiter.try_acquire();
        }

        b.iter(|| std::hint::black_box(limiter.try_acquire()));
    });

    // Benchmark with maximum retries
    group.bench_function("max_cas_retries", |b| {
        let limiter = Arc::new(RateLimiter::new(1, 1));

        b.iter_custom(|iters| {
            let mut total_duration = Duration::ZERO;

            for _ in 0..iters {
                limiter.add_tokens(1);
                let limiter_clone = limiter.clone();

                let start = std::time::Instant::now();

                // Create high contention for a single token
                let handles: Vec<_> = (0..32)
                    .map(|_| {
                        let limiter = limiter_clone.clone();
                        thread::spawn(move || limiter.try_acquire())
                    })
                    .collect();

                for handle in handles {
                    handle.join().unwrap();
                }

                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

/// Benchmark reset operations
fn bench_reset(c: &mut Criterion) {
    let mut group = c.benchmark_group("reset");

    group.bench_function("reset_limiter", |b| {
        let limiter = RateLimiter::new(1000, 100);

        // Use some tokens and generate metrics
        for _ in 0..500 {
            limiter.try_acquire();
        }

        b.iter(|| {
            limiter.reset();
        });
    });

    group.finish();
}

/// Benchmark different token amounts
fn bench_token_amounts(c: &mut Criterion) {
    let mut group = c.benchmark_group("token_amounts");

    for max_tokens in [10u64, 100, 1000, 10000, 100000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(max_tokens),
            &max_tokens,
            |b, &max_tokens| {
                let limiter = RateLimiter::new(max_tokens, (max_tokens / 10) as u32);

                b.iter(|| {
                    // Acquire 1% of max tokens
                    let to_acquire = (max_tokens / 100).max(1);
                    std::hint::black_box(limiter.try_acquire_n(to_acquire))
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_single_acquire,
    bench_bulk_acquire,
    bench_memory_orderings,
    bench_concurrent_acquire,
    bench_high_contention,
    bench_refill,
    bench_metrics,
    bench_ip_manager,
    bench_ip_manager_concurrent,
    bench_ip_manager_cleanup,
    bench_worst_case,
    bench_reset,
    bench_token_amounts,
);

criterion_main!(benches);
