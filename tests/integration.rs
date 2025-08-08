use rater::{IpRateLimiterManager, MemoryOrdering, RateLimiter, RateLimiterConfig};
use std::net::IpAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
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

    // Wait for exactly one refill period
    thread::sleep(Duration::from_millis(105));

    // Should have refilled exactly refill_rate tokens
    let available = limiter.available_tokens();
    assert!(available >= 9 && available <= 10);
}

#[test]
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
