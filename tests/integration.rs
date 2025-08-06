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
        200, // cleanup interval
        100, // inactive duration
    ));

    // Phase 1: Add IPs
    for i in 0..50 {
        let ip: IpAddr = format!("192.168.1.{}", i).parse().unwrap();
        assert!(manager.try_acquire(ip));
    }
    assert_eq!(manager.active_ips(), 50);

    // Phase 2: Start cleanup thread
    let (handle, stop_tx) = manager.clone().start_stoppable_cleanup_thread();

    // Phase 3: Let some IPs become inactive
    thread::sleep(Duration::from_millis(550));

    // Phase 4: Keep some IPs active by continuously accessing them
    let active_ips = 10;
    for _ in 0..3 {
        // Access multiple times to ensure they stay active
        for i in 0..active_ips {
            let ip: IpAddr = format!("192.168.1.{}", i).parse().unwrap();
            manager.try_acquire(ip);
        }
        thread::sleep(Duration::from_millis(50)); // Small delay between accesses
    }

    // Phase 5: Wait for cleanup (shorter wait since we're actively keeping IPs alive)
    thread::sleep(Duration::from_millis(50));

    // Should have cleaned up inactive IPs but kept the active ones
    let remaining = manager.active_ips();
    println!("Remaining IPs after cleanup: {}", remaining);
    assert!(remaining < 50, "Should have cleaned up some IPs");
    assert!(
        remaining >= active_ips,
        "Should have kept at least {} active IPs, but only {} remain",
        active_ips,
        remaining
    );

    // Phase 6: Stop cleanup thread
    stop_tx.send(()).unwrap();
    handle.join().unwrap();

    // Verify stats
    let stats = manager.stats();
    assert_eq!(stats.total_created, 60);
    assert!(stats.total_cleaned > 0);
}
