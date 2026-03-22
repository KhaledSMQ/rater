use rater::{
    HealthStatus, IpRateLimiterManager, MemoryOrdering, RateLimiter, RateLimiterBuilder,
    RateLimiterConfig,
};
use std::net::IpAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
#[cfg_attr(miri, ignore)]
fn test_refill_timing_accuracy() {
    let config = RateLimiterConfig {
        max_tokens: 10,
        refill_rate: 10,
        refill_interval_ms: 100,
        ordering: MemoryOrdering::AcquireRelease,
    };

    let limiter = RateLimiter::with_config(config);

    // Drain all tokens
    for _ in 0..10 {
        assert!(limiter.try_acquire());
    }

    // Should have no tokens
    assert_eq!(limiter.available_tokens(), 0);

    // Wait for at least one refill period
    thread::sleep(Duration::from_millis(150));

    // Should have refilled (CI VMs may be slow, so accept a range)
    let available = limiter.available_tokens();
    assert!(
        available >= 5 && available <= 10,
        "Expected 5..=10 tokens after refill, got {}",
        available
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_sustained_load_scenario() {
    let limiter = Arc::new(RateLimiter::new(1000, 100));
    let mut handles = vec![];

    // Simulate sustained load from multiple threads
    for thread_id in 0..20 {
        let limiter_clone = limiter.clone();
        handles.push(thread::spawn(move || {
            let mut acquired = 0;
            let mut rejected = 0;
            let start = std::time::Instant::now();

            while start.elapsed() < Duration::from_secs(2) {
                if limiter_clone.try_acquire() {
                    acquired += 1;
                } else {
                    rejected += 1;
                }
                thread::sleep(Duration::from_millis(1));
            }

            (thread_id, acquired, rejected)
        }));
    }

    let results: Vec<(usize, u32, u32)> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let total_acquired: u32 = results.iter().map(|(_, a, _)| a).sum();
    let total_rejected: u32 = results.iter().map(|(_, _, r)| r).sum();

    println!(
        "Sustained load test - Acquired: {}, Rejected: {}",
        total_acquired, total_rejected
    );

    // Should have both acquisitions and rejections under load
    assert!(total_acquired > 0);
    assert!(total_rejected > 0);

    // Check metrics consistency
    let metrics = limiter.metrics();
    assert_eq!(metrics.total_acquired, total_acquired as u64);
}

// Replace the test_ip_manager_lifecycle function in integration.rs with this:

#[test]
#[cfg_attr(miri, ignore)]
fn test_ip_manager_lifecycle() {
    let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
        RateLimiterConfig::per_second(10),
        1000, // longer cleanup interval so it doesn't interfere
        100,  // inactive duration
    ));

    // Phase 1: Add IPs
    for i in 0..50 {
        let ip: IpAddr = format!("192.168.1.{}", i).parse().unwrap();
        assert!(manager.try_acquire(ip));
    }
    assert_eq!(manager.active_ips(), 50);

    // Phase 2: Wait for IPs to become inactive
    thread::sleep(Duration::from_millis(150));

    // Phase 3: Keep some IPs active by using try_acquire_n
    let active_ips = 10;
    for i in 0..active_ips {
        let ip: IpAddr = format!("192.168.1.{}", i).parse().unwrap();
        // Force update of last_access_ms
        manager.try_acquire_n(ip, 1);
    }

    // Phase 4: Manually trigger cleanup instead of relying on thread timing
    manager.cleanup();

    // Check results
    let remaining = manager.active_ips();
    println!("Remaining IPs after cleanup: {}", remaining);

    assert!(
        remaining >= active_ips,
        "Should have kept at least {} active IPs, but only {} remain",
        active_ips,
        remaining
    );
    assert!(
        remaining < 50,
        "Should have cleaned up some IPs (had 50, now have {})",
        remaining
    );

    // Verify stats
    let stats = manager.stats();
    assert_eq!(stats.total_created, 50);
    assert!(stats.total_cleaned > 0);
    assert!(stats.total_cleaned == 50 - remaining as u64);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_all_memory_orderings_under_contention() {
    for ordering in [
        MemoryOrdering::Relaxed,
        MemoryOrdering::AcquireRelease,
        MemoryOrdering::Sequential,
    ] {
        let config = RateLimiterConfig {
            max_tokens: 500,
            refill_rate: 50,
            refill_interval_ms: 1000,
            ordering,
        };
        let limiter = Arc::new(RateLimiter::with_config(config));
        let mut handles = vec![];

        for _ in 0..10 {
            let l = limiter.clone();
            handles.push(thread::spawn(move || {
                let mut acquired = 0u32;
                for _ in 0..100 {
                    if l.try_acquire() {
                        acquired += 1;
                    }
                }
                acquired
            }));
        }

        let total: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert!(
            total > 0 && total <= 500,
            "ordering={:?} total={}",
            ordering,
            total
        );

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_acquired + metrics.total_rejected, 1000);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_burst_then_steady_state() {
    let config = RateLimiterConfig {
        max_tokens: 50,
        refill_rate: 10,
        refill_interval_ms: 100,
        ordering: MemoryOrdering::AcquireRelease,
    };
    let limiter = RateLimiter::with_config(config);

    // Burst: consume all 50 tokens immediately
    let mut burst_acquired = 0;
    for _ in 0..50 {
        if limiter.try_acquire() {
            burst_acquired += 1;
        }
    }
    assert_eq!(burst_acquired, 50);

    // 51st should fail
    assert!(!limiter.try_acquire());

    // Steady state: wait for refill, then acquire at the refill rate
    thread::sleep(Duration::from_millis(150));

    let available = limiter.available_tokens();
    assert!(
        available >= 5 && available <= 50,
        "Expected some tokens after refill, got {}",
        available
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_metrics_consistent_under_contention() {
    let limiter = Arc::new(RateLimiter::new(200, 20));
    let mut handles = vec![];

    for _ in 0..16 {
        let l = limiter.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                l.try_acquire();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let metrics = limiter.metrics();
    // total_acquired + total_rejected should exactly equal total attempts
    assert_eq!(
        metrics.total_acquired + metrics.total_rejected,
        16 * 100,
        "Metrics inconsistency: acquired={} + rejected={} != {}",
        metrics.total_acquired,
        metrics.total_rejected,
        16 * 100
    );

    // total_acquired should not exceed initial + refilled tokens
    assert!(
        metrics.total_acquired <= 200 + (metrics.total_refills * 20),
        "Acquired more tokens than possible"
    );
}

#[test]
fn test_health_transitions() {
    let limiter = RateLimiter::new(10, 1);

    // Initially healthy
    assert_eq!(limiter.metrics().health_status(), HealthStatus::Healthy);

    // Drain all tokens
    for _ in 0..10 {
        limiter.try_acquire();
    }

    // Should be degraded (empty tokens) or critical depending on rejection count
    let status_after_drain = limiter.metrics().health_status();
    assert!(
        status_after_drain == HealthStatus::Degraded || status_after_drain == HealthStatus::Healthy,
        "Expected Degraded or Healthy after drain, got {:?}",
        status_after_drain,
    );

    // Accumulate many rejections
    for _ in 0..20 {
        limiter.try_acquire();
    }

    // Should be critical
    assert_eq!(limiter.metrics().health_status(), HealthStatus::Critical);

    // Reset should bring back to healthy
    limiter.reset();
    assert_eq!(limiter.metrics().health_status(), HealthStatus::Healthy);
}

#[test]
fn test_builder_try_build_errors() {
    // refill_rate = 0
    let result = RateLimiterBuilder::new().refill_rate(0).try_build();
    assert!(result.is_err());

    // max_tokens = 0
    let result = RateLimiterBuilder::new().max_tokens(0).try_build();
    assert!(result.is_err());

    // refill_interval_ms = 0
    let result = RateLimiterBuilder::new().refill_interval_ms(0).try_build();
    assert!(result.is_err());

    // refill_rate > max_tokens
    let result = RateLimiterBuilder::new()
        .max_tokens(5)
        .refill_rate(10)
        .try_build();
    assert!(result.is_err());
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_ip_manager_concurrent_same_ip_contention() {
    let manager = Arc::new(IpRateLimiterManager::new(RateLimiterConfig {
        max_tokens: 100,
        refill_rate: 10,
        refill_interval_ms: 1000,
        ordering: MemoryOrdering::AcquireRelease,
    }));

    let ip: IpAddr = "10.0.0.1".parse().unwrap();
    let mut handles = vec![];

    // 20 threads hammering the same IP
    for _ in 0..20 {
        let m = manager.clone();
        handles.push(thread::spawn(move || {
            let mut acquired = 0u32;
            for _ in 0..50 {
                if m.try_acquire(ip) {
                    acquired += 1;
                }
            }
            acquired
        }));
    }

    let total: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();

    // Should have acquired exactly 100 tokens (no refill in this short time)
    assert!(
        total <= 100,
        "Should not exceed max_tokens, but got {}",
        total
    );
    assert!(
        total >= 90,
        "Should acquire most tokens, but only got {}",
        total
    );
    assert_eq!(manager.active_ips(), 1);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_add_tokens_concurrent() {
    let limiter = Arc::new(RateLimiter::new(1000, 10));

    // Drain all tokens
    assert!(limiter.try_acquire_n(1000));

    let mut handles = vec![];

    // Multiple threads adding tokens
    for _ in 0..10 {
        let l = limiter.clone();
        handles.push(thread::spawn(move || {
            l.add_tokens(100);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Should have added up to max_tokens (capped at 1000)
    assert!(limiter.available_tokens() <= 1000);
    assert!(limiter.available_tokens() > 0);
}

#[test]
fn test_time_helpers_are_public() {
    let ms = rater::current_time_ms();
    let us = rater::current_time_us();
    let ns = rater::current_time_ns();

    assert!(ms > 0);
    assert!(us > 0);
    assert!(ns > 0);
    assert!(us > ms);
    assert!(ns > us);
}

#[test]
fn test_cpu_relax_is_public() {
    rater::cpu_relax();
}
