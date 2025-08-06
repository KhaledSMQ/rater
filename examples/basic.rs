//! Basic usage example for the rater crate.

use core::time::Duration;
use rater::{MemoryOrdering, RateLimiter, RateLimiterConfig};
use std::thread;

fn main() {
    println!("=== Basic Rate Limiter Example ===\n");

    // Example 1: Simple rate limiter
    simple_example();

    println!("{}", "\n".to_owned() + "=".repeat(50).as_str() + "\n");

    // Example 2: Custom configuration
    custom_config_example();

    println!("{}", "\n".to_owned() + "=".repeat(50).as_str() + "\n");

    // Example 3: Bulk operations
    bulk_operations_example();

    println!("{}", "\n".to_owned() + "=".repeat(50).as_str() + "\n");

    // Example 4: Monitoring metrics
    metrics_example();

    println!("{}", "\n".to_owned() + "=".repeat(50).as_str() + "\n");

    // Example 5: Refill demonstration
    refill_example();
}

fn simple_example() {
    println!("1. Simple Rate Limiter:");

    // Create a rate limiter with 10 tokens, refilling 2 tokens per second
    let limiter = RateLimiter::new(10, 2);

    println!("   Created limiter with 10 tokens, refilling 2 tokens/second");

    // Try to acquire tokens
    let mut successful = 0;
    let mut failed = 0;

    for i in 1..=15 {
        if limiter.try_acquire() {
            successful += 1;
            println!("   Request {} - ✅ Allowed", i);
        } else {
            failed += 1;
            println!("   Request {} - ❌ Rate limited", i);
        }
    }

    println!(
        "   Results: {} successful, {} rate limited",
        successful, failed
    );
}

fn custom_config_example() {
    println!("2. Custom Configuration:");

    // Create a configuration for 100 requests per second
    let config = RateLimiterConfig::per_second(100)
        .with_burst_multiplier(2) // Allow 2x burst capacity
        .with_ordering(MemoryOrdering::Relaxed); // Use relaxed ordering for better performance

    let limiter = RateLimiter::with_config(config.clone());

    println!("   Configuration:");
    println!("   - Max tokens: {}", config.max_tokens);
    println!(
        "   - Refill rate: {} tokens/second",
        config.effective_rate_per_second()
    );
    println!("   - Memory ordering: {:?}", config.ordering);

    // Test burst capacity
    let mut burst_count = 0;
    while limiter.try_acquire() {
        burst_count += 1;
    }

    println!(
        "   Burst capacity test: {} requests processed immediately",
        burst_count
    );
}

fn bulk_operations_example() {
    println!("3. Bulk Token Operations:");

    let limiter = RateLimiter::new(50, 10);

    println!("   Initial tokens: {}", limiter.available_tokens());

    // Try to acquire 10 tokens at once
    if limiter.try_acquire_n(10) {
        println!("   ✅ Acquired 10 tokens in bulk");
    }

    println!("   Remaining tokens: {}", limiter.available_tokens());

    // Try to acquire 50 tokens (should fail)
    if !limiter.try_acquire_n(50) {
        println!(
            "   ❌ Cannot acquire 50 tokens (only {} available)",
            limiter.available_tokens()
        );
    }

    // Add bonus tokens manually
    limiter.add_tokens(20);
    println!("   Added 20 bonus tokens");
    println!("   Current tokens: {}", limiter.available_tokens());
}

fn metrics_example() {
    println!("4. Monitoring and Metrics:");

    let limiter = RateLimiter::new(20, 5);

    // Generate some traffic
    for _ in 0..25 {
        limiter.try_acquire();
    }

    // Get metrics
    let metrics = limiter.metrics();

    println!("   Performance Metrics:");
    println!("   - Total requests: {}", metrics.total_requests());
    println!("   - Success rate: {:.2}%", metrics.success_rate() * 100.0);
    println!(
        "   - Rejection rate: {:.2}%",
        metrics.rejection_rate() * 100.0
    );
    println!(
        "   - Current utilization: {:.2}%",
        metrics.utilization() * 100.0
    );
    println!(
        "   - Available tokens: {}/{}",
        metrics.current_tokens, metrics.max_tokens
    );
    println!(
        "   - Consecutive rejections: {}",
        metrics.consecutive_rejections
    );

    // Check health status
    let health = metrics.health_status();
    println!("   - Health status: {:?}", health);
    println!("   - Suggested action: {}", health.suggested_action());
}

fn refill_example() {
    println!("5. Token Refill Demonstration:");

    let config = RateLimiterConfig {
        max_tokens: 5,
        refill_rate: 5,
        refill_interval_ms: 1000, // Refill every second
        ordering: MemoryOrdering::AcquireRelease,
    };

    let limiter = RateLimiter::with_config(config);

    println!("   Configuration: 5 tokens max, refill 5 tokens/second");

    // Exhaust all tokens
    for i in 1..=5 {
        if limiter.try_acquire() {
            println!("   Token {} acquired", i);
        }
    }

    println!(
        "   All tokens exhausted, available: {}",
        limiter.available_tokens()
    );

    // Try to acquire without waiting (should fail)
    if !limiter.try_acquire() {
        println!("   ❌ No tokens available immediately");
    }

    // Wait for refill
    println!("   Waiting 1 second for refill...");
    thread::sleep(Duration::from_secs(1));

    println!(
        "   After refill, available tokens: {}",
        limiter.available_tokens()
    );

    // Should be able to acquire again
    if limiter.try_acquire() {
        println!("   ✅ Token acquired after refill!");
    }

    // Demonstrate reset
    println!("\n   Resetting the limiter...");
    limiter.reset();

    let metrics = limiter.metrics();
    println!("   After reset:");
    println!(
        "   - Available tokens: {}/{}",
        metrics.current_tokens, metrics.max_tokens
    );
    println!("   - Total acquired: {}", metrics.total_acquired);
    println!("   - Total rejected: {}", metrics.total_rejected);
}
