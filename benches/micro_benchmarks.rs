//! # Micro Benchmarks
//!
//! Fine-grained benchmarks for specific rate limiter operations.
//!
//! Run with: `cargo bench --bench micro_benchmarks`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rater::{
    cpu_relax, current_time_ms, current_time_ns, current_time_us, MemoryOrdering, RateLimiter,
    RateLimiterConfig,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

/// Benchmark atomic operations with different orderings
fn bench_atomic_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("atomic_ops");

    let orderings = [
        ("Relaxed", Ordering::Relaxed),
        ("Acquire", Ordering::Acquire),
        // ("Release", Ordering::Release),
        // ("AcqRel", Ordering::AcqRel),
        // ("SeqCst", Ordering::SeqCst),
    ];

    // Benchmark load operations
    for (name, ordering) in &orderings {
        group.bench_function(format!("load_{}", name), |b| {
            let atomic = AtomicU64::new(42);
            b.iter(|| black_box(atomic.load(*ordering)));
        });
    }

    // Benchmark store operations
    for (name, ordering) in &orderings {
        if *ordering == Ordering::Acquire || *ordering == Ordering::AcqRel {
            continue; // Invalid for store
        }
        group.bench_function(format!("store_{}", name), |b| {
            let atomic = AtomicU64::new(0);
            let mut val = 0u64;
            b.iter(|| {
                atomic.store(val, *ordering);
                val = val.wrapping_add(1);
            });
        });
    }

    // Benchmark compare_exchange operations
    for (name, ordering) in &orderings {
        if *ordering == Ordering::Acquire || *ordering == Ordering::Release {
            continue; // Need specific combinations
        }
        group.bench_function(format!("cas_{}", name), |b| {
            let atomic = AtomicU64::new(0);
            b.iter(|| {
                let current = atomic.load(Ordering::Relaxed);
                black_box(atomic.compare_exchange_weak(
                    current,
                    current + 1,
                    *ordering,
                    Ordering::Relaxed,
                ))
            });
        });
    }

    group.finish();
}

/// Benchmark time functions
fn bench_time_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("time_functions");

    group.bench_function("current_time_ms", |b| {
        b.iter(|| black_box(current_time_ms()));
    });

    group.bench_function("current_time_us", |b| {
        b.iter(|| black_box(current_time_us()));
    });

    group.bench_function("current_time_ns", |b| {
        b.iter(|| black_box(current_time_ns()));
    });

    group.bench_function("std_instant_now", |b| {
        b.iter(|| black_box(std::time::Instant::now()));
    });

    group.finish();
}

/// Benchmark CPU relaxation primitives
fn bench_cpu_primitives(c: &mut Criterion) {
    let mut group = c.benchmark_group("cpu_primitives");

    group.bench_function("cpu_relax", |b| {
        b.iter(|| {
            cpu_relax();
        });
    });

    group.bench_function("spin_loop_hint", |b| {
        b.iter(|| {
            std::hint::spin_loop();
        });
    });

    group.bench_function("yield_now", |b| {
        b.iter(|| {
            thread::yield_now();
        });
    });

    // Benchmark different backoff strategies
    group.bench_function("exponential_backoff_4", |b| {
        let mut count = 0u32;
        b.iter(|| {
            for _ in 0..(1 << (count.min(4))) {
                cpu_relax();
            }
            count = (count + 1) % 8;
        });
    });

    group.finish();
}

/// Benchmark available_tokens with different states
fn bench_available_tokens(c: &mut Criterion) {
    let mut group = c.benchmark_group("available_tokens");

    // Full bucket
    group.bench_function("full_bucket", |b| {
        let limiter = RateLimiter::new(1000, 100);
        b.iter(|| black_box(limiter.available_tokens()));
    });

    // Empty bucket
    group.bench_function("empty_bucket", |b| {
        let limiter = RateLimiter::new(100, 10);
        for _ in 0..100 {
            limiter.try_acquire();
        }
        b.iter(|| black_box(limiter.available_tokens()));
    });

    // Half full bucket
    group.bench_function("half_full_bucket", |b| {
        let limiter = RateLimiter::new(100, 10);
        for _ in 0..50 {
            limiter.try_acquire();
        }
        b.iter(|| black_box(limiter.available_tokens()));
    });

    group.finish();
}

/// Benchmark is_inactive checks
fn bench_is_inactive(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_inactive");

    group.bench_function("recently_active", |b| {
        let limiter = RateLimiter::new(100, 10);
        limiter.try_acquire(); // Make it active

        b.iter(|| black_box(limiter.is_inactive(1000)));
    });

    group.bench_function("long_inactive", |b| {
        let limiter = RateLimiter::new(100, 10);
        // Don't use it, so it's been inactive since creation

        b.iter(|| black_box(limiter.is_inactive(0)));
    });

    group.finish();
}

/// Benchmark metrics calculation
fn bench_metrics_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("metrics_calc");

    // Create different scenarios
    let scenarios = [
        ("no_activity", 0, 0),
        ("all_success", 1000, 0),
        ("all_rejected", 0, 1000),
        ("mixed_50_50", 500, 500),
        ("high_success", 900, 100),
        ("high_rejection", 100, 900),
    ];

    for (name, acquired, rejected) in scenarios {
        group.bench_function(name, |b| {
            let limiter = RateLimiter::new(1000, 100);

            // Generate the scenario
            for _ in 0..acquired {
                limiter.try_acquire();
            }
            // Exhaust tokens first
            while limiter.try_acquire() {}
            for _ in 0..rejected {
                limiter.try_acquire(); // Will be rejected
            }

            b.iter(|| black_box(limiter.metrics()));
        });
    }

    group.finish();
}

/// Benchmark CAS retry scenarios
fn bench_cas_retries(c: &mut Criterion) {
    let mut group = c.benchmark_group("cas_retries");

    // Low contention (2 threads)
    group.bench_function("low_contention", |b| {
        let limiter = Arc::new(RateLimiter::new(10000, 1000));

        b.iter_custom(|iters| {
            let start = std::time::Instant::now();

            for _ in 0..iters {
                let limiter_clone = limiter.clone();
                let handles: Vec<_> = (0..2)
                    .map(|_| {
                        let limiter = limiter_clone.clone();
                        thread::spawn(move || {
                            for _ in 0..10 {
                                limiter.try_acquire();
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    handle.join().unwrap();
                }
            }

            start.elapsed()
        });
    });

    // Medium contention (8 threads)
    group.bench_function("medium_contention", |b| {
        let limiter = Arc::new(RateLimiter::new(10000, 1000));

        b.iter_custom(|iters| {
            let start = std::time::Instant::now();

            for _ in 0..iters {
                let limiter_clone = limiter.clone();
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        let limiter = limiter_clone.clone();
                        thread::spawn(move || {
                            for _ in 0..10 {
                                limiter.try_acquire();
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    handle.join().unwrap();
                }
            }

            start.elapsed()
        });
    });

    // High contention (32 threads)
    group.bench_function("high_contention", |b| {
        let limiter = Arc::new(RateLimiter::new(10000, 1000));

        b.iter_custom(|iters| {
            let start = std::time::Instant::now();

            for _ in 0..iters {
                let limiter_clone = limiter.clone();
                let handles: Vec<_> = (0..32)
                    .map(|_| {
                        let limiter = limiter_clone.clone();
                        thread::spawn(move || {
                            for _ in 0..10 {
                                limiter.try_acquire();
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    handle.join().unwrap();
                }
            }

            start.elapsed()
        });
    });

    group.finish();
}

/// Benchmark refill period calculations
fn bench_refill_calculations(c: &mut Criterion) {
    let mut group = c.benchmark_group("refill_calc");

    // Different refill intervals
    let intervals = [1, 10, 100, 1000, 10000];

    for interval_ms in intervals {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}ms", interval_ms)),
            &interval_ms,
            |b, &interval_ms| {
                let config = RateLimiterConfig::new(1000, 100, interval_ms);
                let limiter = RateLimiter::with_config(config);

                // Drain tokens to force refill checks
                for _ in 0..1000 {
                    limiter.try_acquire();
                }

                b.iter(|| {
                    // This will check if refill is needed
                    black_box(limiter.try_acquire())
                });
            },
        );
    }

    group.finish();
}

/// Benchmark configuration validation
fn bench_config_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("config_validation");

    group.bench_function("valid_config", |b| {
        b.iter(|| {
            let config = RateLimiterConfig::new(100, 10, 1000);
            black_box(config.validate())
        });
    });

    group.bench_function("invalid_config", |b| {
        b.iter(|| {
            let config = RateLimiterConfig::new(0, 10, 1000);
            black_box(config.validate())
        });
    });

    group.bench_function("effective_rate_calculation", |b| {
        let config = RateLimiterConfig::new(100, 10, 1000);
        b.iter(|| black_box(config.effective_rate_per_second()));
    });

    group.finish();
}

/// Benchmark builder pattern
fn bench_builder_pattern(c: &mut Criterion) {
    let mut group = c.benchmark_group("builder");

    group.bench_function("builder_create", |b| {
        use rater::RateLimiterBuilder;

        b.iter(|| {
            let limiter = RateLimiterBuilder::new()
                .max_tokens(100)
                .refill_rate(10)
                .refill_interval_ms(1000)
                .memory_ordering(MemoryOrdering::AcquireRelease)
                .build();
            black_box(limiter)
        });
    });

    group.finish();
}

criterion_group!(
    micro_benches,
    bench_atomic_operations,
    bench_time_functions,
    bench_cpu_primitives,
    bench_available_tokens,
    bench_is_inactive,
    bench_metrics_calculation,
    bench_cas_retries,
    bench_refill_calculations,
    bench_config_validation,
    bench_builder_pattern,
);

criterion_main!(micro_benches);
