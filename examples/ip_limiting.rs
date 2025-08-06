use core::net::{IpAddr, Ipv4Addr};
use rater::{IpRateLimiterManager, RateLimiterConfig};

fn main() {
    let config = RateLimiterConfig::per_second(10);
    let manager = IpRateLimiterManager::new(config);

    // Simulate requests from different IPs
    let ips = vec![
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3)),
    ];

    for ip in &ips {
        for i in 1..=12 {
            if manager.try_acquire(*ip) {
                println!("IP {} - Request {} allowed", ip, i);
            } else {
                println!("IP {} - Request {} BLOCKED", ip, i);
            }
        }
        println!();
    }

    let stats = manager.stats();
    println!("{}", stats.summary());
}
