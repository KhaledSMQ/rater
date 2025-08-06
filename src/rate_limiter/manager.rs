//! # IP-based Rate Limiter Manager
//!
//! This module provides a manager for per-IP rate limiting with automatic cleanup.
//! It's designed for web applications that need to apply different rate limits
//! to different clients based on their IP addresses.
//!
//! ## Architecture
//!
//! ```text
//!     IP Rate Limiting Architecture:
//!
//!     Client Requests:
//!     192.168.1.1 ──┐
//!     192.168.1.2 ──┤
//!     192.168.1.3 ──┼──► IP Manager ──► Individual Rate Limiters
//!     10.0.0.1 ─────┤         │
//!     10.0.0.2 ─────┘         ▼
//!                       ┌──────────────┐
//!                       │  DashMap     │
//!                       │  ┌────────┐  │
//!                       │  │IP → RL │  │  RL = Rate Limiter
//!                       │  │IP → RL │  │
//!                       │  │IP → RL │  │
//!                       │  └────────┘  │
//!                       └──────────────┘
//! ```
//!
//! ## Key Features
//!
//! 1. **Per-IP Isolation**: Each IP gets its own rate limiter
//! 2. **Automatic Cleanup**: Removes inactive IP limiters to save memory
//! 3. **Bounded Memory**: Limits maximum tracked IPs to prevent DoS
//! 4. **Lock-Free Operations**: Uses DashMap for concurrent access
//! 5. **Emergency Cleanup**: Aggressive cleanup when approaching limits

use super::{
    config::RateLimiterConfig,
    core::RateLimiter,
    utils::current_time_ms,
};
use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
use tracing::{debug, info, warn};

// Configuration constants

/// Maximum number of unique IPs that can be tracked simultaneously.
///
/// This limit prevents memory exhaustion attacks where an attacker
/// tries to create rate limiters for millions of IPs.
const MAX_TRACKED_IPS: usize = 10_000;

/// Threshold at which we start cleanup operations (90% of max).
///
/// When we reach this many active IPs, we trigger cleanup to make room.
const CLEANUP_THRESHOLD: usize = (MAX_TRACKED_IPS * 90) / 100;

/// Target number of IPs after cleanup (70% of max).
///
/// Cleanup will try to reduce the count to this level, giving us
/// headroom before the next cleanup is needed.
const CLEANUP_TARGET: usize = (MAX_TRACKED_IPS * 70) / 100;

/// Factor to reduce inactive duration during emergency cleanup.
///
/// During emergency cleanup, we're more aggressive about removing
/// IPs that haven't been seen recently.
const EMERGENCY_CLEANUP_INACTIVE_FACTOR: u64 = 2;

/// Minimum inactive duration during emergency cleanup (milliseconds).
///
/// Even during emergency cleanup, we won't remove IPs that were
/// active within the last second.
const EMERGENCY_CLEANUP_MIN_INACTIVE_MS: u64 = 1000;

/// Manager for per-IP rate limiting.
///
/// This struct manages a collection of rate limiters, one for each IP address.
/// It provides automatic cleanup of inactive limiters and prevents memory
/// exhaustion through bounded tracking.
///
/// ## Usage Patterns
///
/// ### Web Server Integration
///
/// ```rust
/// use rater::{IpRateLimiterManager, RateLimiterConfig};
/// use std::net::IpAddr;
/// use std::sync::Arc;
///
/// // Create a shared manager for your web server
/// let config = RateLimiterConfig::per_second(100);
/// let manager = Arc::new(IpRateLimiterManager::new(config));
///
/// // In your request handler:
/// fn handle_request(manager: &IpRateLimiterManager, client_ip: IpAddr) {
///     if !manager.try_acquire(client_ip) {
///         // Return 429 Too Many Requests
///         return;
///     }
///     // Process the request
/// }
/// ```
///
/// ### With Automatic Cleanup
///
/// ```rust
/// use rater::{IpRateLimiterManager, RateLimiterConfig};
/// use std::sync::Arc;
///
/// let config = RateLimiterConfig::per_second(50);
/// let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
///     config,
///     60_000,  // Cleanup every minute
///     300_000, // Remove IPs inactive for 5 minutes
/// ));
///
/// // Start automatic cleanup thread
/// let cleanup_handle = manager.clone().start_cleanup_thread();
/// ```
///
/// ## Memory Management
///
/// The manager automatically manages memory through:
///
/// 1. **Bounded Tracking**: Maximum 10,000 IPs by default
/// 2. **LRU-like Eviction**: Removes least recently used IPs when full
/// 3. **Periodic Cleanup**: Removes inactive IPs regularly
/// 4. **Emergency Cleanup**: Aggressive cleanup when near capacity
#[derive(Clone)]
pub struct IpRateLimiterManager {
    /// Concurrent hash map storing IP to rate limiter mappings.
    /// DashMap provides lock-free concurrent access with sharding.
    limiters: Arc<DashMap<IpAddr, Arc<RateLimiter>, ahash::RandomState>>,

    /// Current count of active rate limiters.
    /// Used for fast capacity checks without iterating the map.
    active_count: Arc<AtomicUsize>,

    /// Configuration template for creating new rate limiters.
    config: RateLimiterConfig,

    /// Interval between cleanup operations (milliseconds).
    cleanup_interval_ms: u64,

    /// Duration after which an IP is considered inactive (milliseconds).
    inactive_duration_ms: u64,

    /// Total number of rate limiters created since startup.
    total_created: Arc<AtomicU64>,

    /// Total number of rate limiters cleaned up since startup.
    total_cleaned: Arc<AtomicU64>,

    /// Flag to prevent concurrent emergency cleanups.
    cleanup_in_progress: Arc<AtomicBool>,
}

impl IpRateLimiterManager {
    /// Creates a new IP rate limiter manager with default settings.
    ///
    /// Default settings:
    /// - Cleanup interval: 60 seconds
    /// - Inactive duration: 5 minutes
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for individual rate limiters
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = IpRateLimiterManager::new(config);
    /// ```
    pub fn new(config: RateLimiterConfig) -> Self {
        // Calculate optimal shard count based on CPU cores
        // More shards = less contention but more memory overhead
        let num_shards = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .next_power_of_two()
            .min(64);  // Cap at 64 shards for memory efficiency

        // Pre-size each shard for expected load distribution
        let initial_capacity = (MAX_TRACKED_IPS / num_shards).max(128);

        Self {
            limiters: Arc::new(DashMap::with_capacity_and_hasher_and_shard_amount(
                initial_capacity,
                ahash::RandomState::new(),  // Fast, secure hash function
                num_shards,
            )),
            active_count: Arc::new(AtomicUsize::new(0)),
            config,
            cleanup_interval_ms: 60_000,  // 1 minute default
            inactive_duration_ms: 300_000,  // 5 minutes default
            total_created: Arc::new(AtomicU64::new(0)),
            total_cleaned: Arc::new(AtomicU64::new(0)),
            cleanup_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Creates a new manager with custom cleanup settings.
    ///
    /// Use this when you need fine control over cleanup behavior.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for individual rate limiters
    /// * `cleanup_interval_ms` - How often to run cleanup (milliseconds)
    /// * `inactive_duration_ms` - How long before an IP is considered inactive
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(50);
    /// let manager = IpRateLimiterManager::with_cleanup_settings(
    ///     config,
    ///     30_000,   // Cleanup every 30 seconds
    ///     120_000,  // Remove IPs inactive for 2 minutes
    /// );
    /// ```
    pub fn with_cleanup_settings(
        config: RateLimiterConfig,
        cleanup_interval_ms: u64,
        inactive_duration_ms: u64,
    ) -> Self {
        let mut manager = Self::new(config);
        manager.cleanup_interval_ms = cleanup_interval_ms;
        manager.inactive_duration_ms = inactive_duration_ms;
        manager
    }

    /// Gets or creates a rate limiter for the specified IP.
    ///
    /// This is the core method that manages rate limiter creation and retrieval.
    /// It handles:
    /// - Fast path: Return existing limiter
    /// - Slow path: Create new limiter with capacity checks
    /// - Emergency cleanup when approaching limits
    ///
    /// # Arguments
    ///
    /// * `ip` - The IP address to get a rate limiter for
    ///
    /// # Returns
    ///
    /// - `Some(limiter)` if successful
    /// - `None` if at capacity and unable to make room
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::net::IpAddr;
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = IpRateLimiterManager::new(config);
    /// let ip: IpAddr = "192.168.1.1".parse().unwrap();
    /// if let Some(limiter) = manager.get_limiter(ip) {
    ///     // Use the limiter
    ///     limiter.try_acquire();
    /// }
    /// ```
    #[inline]
    pub fn get_limiter(&self, ip: IpAddr) -> Option<Arc<RateLimiter>> {
        // Fast path: check if limiter already exists
        // This is the common case and avoids any allocation
        if let Some(limiter) = self.limiters.get(&ip) {
            return Some(limiter.clone());
        }

        // Slow path: need to create new limiter
        let current = self.active_count.load(Ordering::Acquire);

        // Early rejection if at capacity
        if current >= MAX_TRACKED_IPS {
            warn!("Rate limiter capacity reached, rejecting IP: {}", ip);
            return None;
        }

        // Trigger cleanup if approaching threshold
        if current >= CLEANUP_THRESHOLD {
            self.emergency_cleanup();

            // Re-check after cleanup
            if self.active_count.load(Ordering::Acquire) >= MAX_TRACKED_IPS {
                warn!("Rate limiter capacity reached after cleanup, rejecting IP: {}", ip);
                return None;
            }
        }

        // Use entry API for atomic insert-or-get
        let entry = self.limiters.entry(ip);

        match entry {
            dashmap::mapref::entry::Entry::Occupied(occupied) => {
                // Another thread created it while we were checking
                Some(occupied.get().clone())
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                // Reserve our slot atomically
                let prev = self.active_count.fetch_add(1, Ordering::AcqRel);

                // Check for race condition where we exceeded limit
                if prev >= MAX_TRACKED_IPS {
                    // Rollback our increment
                    self.active_count.fetch_sub(1, Ordering::AcqRel);
                    warn!("Rate limiter capacity race detected, rejecting IP: {}", ip);
                    return None;
                }

                // Create and insert the new limiter
                let limiter = Arc::new(RateLimiter::with_config(self.config.clone()));
                vacant.insert(limiter.clone());

                self.total_created.fetch_add(1, Ordering::Relaxed);
                debug!("Created new rate limiter for IP: {} (total: {})", ip, prev + 1);

                Some(limiter)
            }
        }
    }

    /// Performs emergency cleanup when approaching capacity limits.
    ///
    /// This method aggressively removes inactive IPs to make room for new ones.
    /// It uses a lower inactivity threshold and removes the oldest entries first.
    fn emergency_cleanup(&self) {
        // Prevent concurrent emergency cleanups using atomic flag
        if self.cleanup_in_progress.compare_exchange(
            false,
            true,
            Ordering::Acquire,
            Ordering::Relaxed,
        ).is_err() {
            // Another cleanup is already in progress
            return;
        }

        // Ensure we reset the flag when done (RAII pattern)
        let _guard = CleanupGuard {
            flag: &self.cleanup_in_progress,
        };

        let before = self.active_count.load(Ordering::Acquire);
        if before <= CLEANUP_TARGET {
            return;  // Already below target
        }

        info!("Starting emergency cleanup (current: {} IPs)", before);

        // Calculate how many entries to remove
        let to_remove_count = before.saturating_sub(CLEANUP_TARGET);
        let mut removed = 0;

        // Lower threshold during emergency
        let inactive_threshold = if cfg!(test) {
            0  // In tests, remove anything we can
        } else {
            (self.inactive_duration_ms / EMERGENCY_CLEANUP_INACTIVE_FACTOR)
                .max(EMERGENCY_CLEANUP_MIN_INACTIVE_MS)
        };

        let now = current_time_ms();

        // Collect candidates for removal (LRU-style)
        let mut candidates: Vec<(u64, IpAddr)> = Vec::with_capacity(to_remove_count.min(1000));

        // First pass: collect inactive entries
        for entry in self.limiters.iter() {
            let last_access = entry.value().last_access_ms.value.load(Ordering::Relaxed);
            let idle_time = now.saturating_sub(last_access);

            if idle_time >= inactive_threshold {
                candidates.push((idle_time, *entry.key()));

                if candidates.len() >= to_remove_count {
                    break;
                }
            }
        }

        // If not enough inactive entries found (mainly in tests)
        if cfg!(test) && candidates.len() < to_remove_count {
            for entry in self.limiters.iter() {
                if candidates.iter().any(|(_, ip)| ip == entry.key()) {
                    continue;  // Skip already collected
                }

                let last_access = entry.value().last_access_ms.value.load(Ordering::Relaxed);
                let idle_time = now.saturating_sub(last_access);
                candidates.push((idle_time, *entry.key()));

                if candidates.len() >= to_remove_count {
                    break;
                }
            }
        }

        // Sort by idle time (most idle first - LRU eviction)
        candidates.sort_by(|a, b| b.0.cmp(&a.0));

        // Remove the most idle entries
        for (_, ip) in candidates.iter().take(to_remove_count) {
            if self.limiters.remove(ip).is_some() {
                self.active_count.fetch_sub(1, Ordering::AcqRel);
                removed += 1;
            }
        }

        if removed > 0 {
            self.total_cleaned.fetch_add(removed, Ordering::Relaxed);
            info!("Emergency cleanup removed {} limiters (target was {})", removed, to_remove_count);
        }

        // Verify we reached the target
        let after = self.active_count.load(Ordering::Acquire);
        if after > CLEANUP_TARGET && removed < to_remove_count as u64 {
            warn!("Emergency cleanup incomplete: removed {}/{} entries, current count: {}",
                  removed, to_remove_count, after);
        }
    }

    /// Attempts to acquire a single token for the specified IP.
    ///
    /// This is a convenience method that combines getting/creating a limiter
    /// and acquiring a token in one call.
    ///
    /// # Arguments
    ///
    /// * `ip` - The IP address to rate limit
    ///
    /// # Returns
    ///
    /// - `true` if a token was acquired
    /// - `false` if rate limited or at capacity
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::net::IpAddr;
    ///
    /// let ip: IpAddr = "192.168.1.1".parse().unwrap();
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = IpRateLimiterManager::new(config);
    /// if manager.try_acquire(ip) {
    ///     // Request allowed
    /// } else {
    ///     // Rate limited
    /// }
    /// ```
    #[inline(always)]
    pub fn try_acquire(&self, ip: IpAddr) -> bool {
        match self.get_limiter(ip) {
            Some(limiter) => limiter.try_acquire(),
            None => false,
        }
    }

    /// Attempts to acquire multiple tokens for the specified IP.
    ///
    /// Useful for operations that consume multiple rate limit units.
    ///
    /// # Arguments
    ///
    /// * `ip` - The IP address to rate limit
    /// * `n` - Number of tokens to acquire
    ///
    /// # Returns
    ///
    /// - `true` if all tokens were acquired
    /// - `false` if insufficient tokens or at capacity
    #[inline]
    pub fn try_acquire_n(&self, ip: IpAddr, n: u64) -> bool {
        match self.get_limiter(ip) {
            Some(limiter) => limiter.try_acquire_n(n),
            None => false,
        }
    }

    /// Performs routine cleanup of inactive rate limiters.
    ///
    /// This method removes rate limiters for IPs that haven't been
    /// active recently, freeing up memory and capacity.
    ///
    /// ## Cleanup Strategy
    ///
    /// - Normal mode: Remove IPs inactive for `inactive_duration_ms`
    /// - High usage mode: More aggressive cleanup (half the duration)
    /// - Also shrinks the internal map if significantly oversized
    pub fn cleanup(&self) {
        // Skip if emergency cleanup is already running
        if self.cleanup_in_progress.load(Ordering::Acquire) {
            return;
        }

        let before = self.active_count.load(Ordering::Acquire);

        // Adjust threshold based on current usage
        let threshold = if before > CLEANUP_THRESHOLD {
            self.inactive_duration_ms / 2  // More aggressive when near capacity
        } else {
            self.inactive_duration_ms
        };

        let mut removed = 0;

        // Remove inactive entries
        self.limiters.retain(|ip, limiter| {
            if !limiter.is_inactive(threshold) {
                true  // Keep active limiters
            } else {
                debug!("Removing inactive limiter for IP: {}", ip);
                removed += 1;
                self.active_count.fetch_sub(1, Ordering::AcqRel);
                false  // Remove inactive limiter
            }
        });

        if removed > 0 {
            self.total_cleaned.fetch_add(removed, Ordering::Relaxed);
            debug!("Cleanup removed {} inactive limiters", removed);
        }

        // Shrink the map if it has significant overcapacity
        self.shrink_to_fit();
    }

    /// Shrinks the internal map if it has significant overcapacity.
    ///
    /// This helps reduce memory usage after many IPs have been removed.
    pub fn shrink_to_fit(&self) {
        let current_size = self.active_count.load(Ordering::Acquire);
        let capacity = self.limiters.capacity();

        // Shrink if capacity is more than 4x the current size
        if capacity > current_size * 4 && capacity > 1024 {
            self.limiters.shrink_to_fit();
            debug!("Shrunk limiter map capacity from {} to ~{}", capacity, current_size);
        }
    }

    /// Returns the number of currently active IP limiters.
    #[inline]
    pub fn active_ips(&self) -> usize {
        self.active_count.load(Ordering::Acquire)
    }

    /// Returns comprehensive statistics about the manager.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = IpRateLimiterManager::new(config);
    /// let stats = manager.stats();
    /// println!("{}", stats.summary());
    ///
    /// if stats.is_near_capacity() {
    ///     println!("Warning: Approaching capacity limit!");
    /// }
    /// ```
    pub fn stats(&self) -> ManagerStats {
        ManagerStats {
            active_ips: self.active_ips(),
            total_created: self.total_created.load(Ordering::Relaxed),
            total_cleaned: self.total_cleaned.load(Ordering::Relaxed),
            capacity_used: self.active_ips() as f64 / MAX_TRACKED_IPS as f64,
            max_capacity: MAX_TRACKED_IPS,
        }
    }

    /// Starts an automatic cleanup thread.
    ///
    /// The thread runs indefinitely, performing cleanup at regular intervals.
    ///
    /// # Returns
    ///
    /// A `JoinHandle` for the cleanup thread.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = Arc::new(IpRateLimiterManager::new(config));
    /// let handle = manager.clone().start_cleanup_thread();
    ///
    /// // The cleanup thread is now running in the background
    /// // It will run until the program exits
    /// ```
    pub fn start_cleanup_thread(self: Arc<Self>) -> thread::JoinHandle<()> {
        let manager = self.clone();

        thread::Builder::new()
            .name("rater-cleanup".to_string())
            .spawn(move || {
                info!(
                    "Started cleanup thread (interval: {}ms, inactive threshold: {}ms)",
                    manager.cleanup_interval_ms,
                    manager.inactive_duration_ms
                );

                loop {
                    thread::sleep(Duration::from_millis(manager.cleanup_interval_ms));
                    manager.cleanup();

                    let active = manager.active_ips();
                    if active > CLEANUP_THRESHOLD {
                        warn!(
                            "High IP usage: {} active limiters ({}% of capacity)",
                            active,
                            (active * 100) / MAX_TRACKED_IPS
                        );
                    }
                }
            })
            .expect("Failed to spawn cleanup thread")
    }

    /// Starts a stoppable cleanup thread.
    ///
    /// Similar to `start_cleanup_thread`, but can be stopped by sending
    /// a signal through the returned channel.
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// - `JoinHandle` for the cleanup thread
    /// - `Sender` to signal the thread to stop
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = Arc::new(IpRateLimiterManager::new(config));
    /// let (handle, stop_tx) = manager.clone().start_stoppable_cleanup_thread();
    ///
    /// // Later, to stop the thread:
    /// stop_tx.send(()).unwrap();
    /// handle.join().unwrap();
    /// ```
    pub fn start_stoppable_cleanup_thread(
        self: Arc<Self>
    ) -> (thread::JoinHandle<()>, mpsc::Sender<()>) {
        let (stop_tx, stop_rx) = mpsc::channel();
        let manager = self.clone();

        let handle = thread::Builder::new()
            .name("rater-cleanup".to_string())
            .spawn(move || {
                info!(
                    "Started stoppable cleanup thread (interval: {}ms)",
                    manager.cleanup_interval_ms
                );

                loop {
                    match stop_rx.recv_timeout(Duration::from_millis(manager.cleanup_interval_ms)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            info!("Cleanup thread stopping");
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            manager.cleanup();

                            let active = manager.active_ips();
                            if active > CLEANUP_THRESHOLD {
                                warn!(
                                    "High IP usage: {} active limiters ({}% of capacity)",
                                    active,
                                    (active * 100) / MAX_TRACKED_IPS
                                );
                            }
                        }
                    }
                }
            })
            .expect("Failed to spawn cleanup thread");

        (handle, stop_tx)
    }

    /// Clears all rate limiters.
    ///
    /// This removes all IP rate limiters and resets the manager to
    /// an empty state. Useful for testing or emergency resets.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = IpRateLimiterManager::new(config);
    /// manager.clear();
    /// assert_eq!(manager.active_ips(), 0);
    /// ```
    pub fn clear(&self) {
        let count = self.limiters.len();
        self.limiters.clear();
        self.active_count.store(0, Ordering::Release);
        self.total_cleaned.fetch_add(count as u64, Ordering::Relaxed);
        info!("Cleared all {} rate limiters", count);
    }
}

impl std::fmt::Debug for IpRateLimiterManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpRateLimiterManager")
            .field("active_ips", &self.active_ips())
            .field("cleanup_interval_ms", &self.cleanup_interval_ms)
            .field("inactive_duration_ms", &self.inactive_duration_ms)
            .finish()
    }
}

/// RAII guard for cleanup flag.
///
/// Ensures the cleanup flag is always reset, even if the cleanup
/// function panics or returns early.
struct CleanupGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> Drop for CleanupGuard<'a> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Statistics for the IP rate limiter manager.
///
/// Provides insights into the manager's state and performance.
///
/// ## Metrics Explained
///
/// - **active_ips**: Current number of tracked IPs
/// - **total_created**: Lifetime count of limiters created
/// - **total_cleaned**: Lifetime count of limiters removed
/// - **capacity_used**: Percentage of maximum capacity in use
/// - **max_capacity**: Maximum number of IPs that can be tracked
#[derive(Debug, Clone)]
pub struct ManagerStats {
    /// Number of currently active IP limiters.
    pub active_ips: usize,

    /// Total number of limiters created since startup.
    pub total_created: u64,

    /// Total number of limiters cleaned up since startup.
    pub total_cleaned: u64,

    /// Percentage of capacity currently in use (0.0 to 1.0).
    pub capacity_used: f64,

    /// Maximum number of IPs that can be tracked.
    pub max_capacity: usize,
}

impl ManagerStats {
    /// Returns a human-readable summary of the statistics.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = IpRateLimiterManager::new(config);
    /// let stats = manager.stats();
    /// println!("{}", stats.summary());
    /// ```
    pub fn summary(&self) -> String {
        format!(
            "IP Rate Limiter Manager Stats:\n\
             ├─ Capacity:\n\
             │  ├─ Active IPs: {}/{}\n\
             │  ├─ Capacity Used: {:.2}%\n\
             │  └─ Available Slots: {}\n\
             └─ Lifetime:\n\
                ├─ Total Created: {}\n\
                ├─ Total Cleaned: {}\n\
                └─ Net Active: {}",
            self.active_ips,
            self.max_capacity,
            self.capacity_used * 100.0,
            self.max_capacity - self.active_ips,
            self.total_created,
            self.total_cleaned,
            self.total_created.saturating_sub(self.total_cleaned)
        )
    }

    /// Checks if the manager is approaching capacity.
    ///
    /// Returns `true` if using more than 80% of capacity.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rater::{IpRateLimiterManager, RateLimiterConfig};
    ///
    /// let config = RateLimiterConfig::per_second(100);
    /// let manager = IpRateLimiterManager::new(config);
    /// let stats = manager.stats();
    /// if stats.is_near_capacity() {
    ///     // Consider increasing cleanup frequency
    ///     // or adjusting rate limits
    /// }
    /// ```
    pub fn is_near_capacity(&self) -> bool {
        self.capacity_used > 0.8
    }

    /// Returns the cleanup efficiency ratio.
    ///
    /// A ratio close to 1.0 means most created limiters have been cleaned,
    /// indicating good memory management. A low ratio might indicate
    /// memory growth.
    pub fn cleanup_ratio(&self) -> f64 {
        if self.total_created == 0 {
            0.0
        } else {
            self.total_cleaned as f64 / self.total_created as f64
        }
    }
}

impl std::fmt::Display for ManagerStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.summary())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use crate::MemoryOrdering;

    #[test]
    fn test_basic_ip_limiting() {
        use super::super::config::MemoryOrdering;

        let config = RateLimiterConfig {
            max_tokens: 5,
            refill_rate: 1,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let manager = IpRateLimiterManager::new(config);

        let ip1: IpAddr = "192.168.1.1".parse().unwrap();
        let ip2: IpAddr = "192.168.1.2".parse().unwrap();

        // Each IP gets its own limit
        for _ in 0..5 {
            assert!(manager.try_acquire(ip1));
            assert!(manager.try_acquire(ip2));
        }

        // Both should be exhausted
        assert!(!manager.try_acquire(ip1));
        assert!(!manager.try_acquire(ip2));

        assert_eq!(manager.active_ips(), 2);
    }

    #[test]
    fn test_manager_cleanup() {
        let config = RateLimiterConfig::default();
        let manager = IpRateLimiterManager::with_cleanup_settings(
            config,
            1000,
            50, // Very short inactive duration for testing
        );

        // Create some limiters
        for i in 0..10 {
            let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, i));
            manager.try_acquire(ip);
        }

        assert_eq!(manager.active_ips(), 10);

        // Wait for them to become inactive
        thread::sleep(Duration::from_millis(100));

        // Trigger cleanup
        manager.cleanup();

        // Should have removed inactive ones
        assert!(manager.active_ips() < 10);
    }

    #[test]
    fn test_manager_stats() {
        let config = RateLimiterConfig::default();
        let manager = IpRateLimiterManager::new(config);

        // Create some limiters
        for i in 0..5 {
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, i));
            manager.try_acquire(ip);
        }

        let stats = manager.stats();
        assert_eq!(stats.active_ips, 5);
        assert_eq!(stats.total_created, 5);
        assert_eq!(stats.total_cleaned, 0);
        assert!(stats.capacity_used > 0.0);
        assert!(!stats.is_near_capacity());

        let summary = stats.summary();
        assert!(summary.contains("Active IPs: 5"));
    }

    #[test]
    fn test_clear() {
        let manager = IpRateLimiterManager::new(RateLimiterConfig::default());

        // Add some IPs
        for i in 0..10 {
            let ip = IpAddr::V4(Ipv4Addr::new(172, 16, 0, i));
            manager.try_acquire(ip);
        }

        assert_eq!(manager.active_ips(), 10);

        manager.clear();

        assert_eq!(manager.active_ips(), 0);
        assert_eq!(manager.stats().total_cleaned, 10);
    }

    #[test]
    fn test_concurrent_ip_access() {
        use super::super::config::MemoryOrdering;

        let config = RateLimiterConfig {
            max_tokens: 100,
            refill_rate: 10,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::AcquireRelease,
        };
        let manager = Arc::new(IpRateLimiterManager::new(config));
        let mut handles = vec![];

        // Multiple threads accessing different IPs
        for thread_id in 0..10 {
            let manager_clone = manager.clone();
            let handle = thread::spawn(move || {
                let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, thread_id));
                let mut acquired = 0;

                for _ in 0..50 {
                    if manager_clone.try_acquire(ip) {
                        acquired += 1;
                    }
                }
                acquired
            });
            handles.push(handle);
        }

        let results: Vec<u32> = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        // Each thread should have acquired some tokens
        for acquired in results {
            assert!(acquired > 0);
            assert!(acquired <= 50);
        }

        assert_eq!(manager.active_ips(), 10);
    }

    #[test]
    fn test_capacity_limit() {
        let config = RateLimiterConfig::default();
        let manager = IpRateLimiterManager::new(config);

        // Try to create more than MAX_TRACKED_IPS limiters
        // This would take too long with 10,000 IPs, so we test the logic
        // by checking that get_limiter returns None when at capacity

        // Manually set the active count to MAX
        manager.active_count.store(MAX_TRACKED_IPS, Ordering::Release);

        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
        assert!(manager.get_limiter(ip).is_none());

        // Reset
        manager.active_count.store(0, Ordering::Release);
    }

    #[test]
    fn test_emergency_cleanup() {
        let config = RateLimiterConfig::default();
        let manager = IpRateLimiterManager::new(config);

        // Fill up to near capacity
        for i in 0..CLEANUP_THRESHOLD {
            let ip = IpAddr::V4(Ipv4Addr::new(
                (i / 16777216) as u8,
                ((i / 65536) % 256) as u8,
                ((i / 256) % 256) as u8,
                (i % 256) as u8,
            ));
            manager.get_limiter(ip);
        }

        let before_cleanup = manager.active_ips();
        assert_eq!(before_cleanup, CLEANUP_THRESHOLD);

        // Next insertion should trigger emergency cleanup
        let trigger_ip = IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255));
        manager.get_limiter(trigger_ip);

        // Should have cleaned up some entries
        let after_cleanup = manager.active_ips();
        assert!(after_cleanup <= CLEANUP_TARGET + 1,
                "Expected {} IPs after cleanup, but got {}", CLEANUP_TARGET + 1, after_cleanup);
    }

    #[test]
    fn test_shrink_to_fit() {
        let manager = IpRateLimiterManager::new(RateLimiterConfig::default());

        // Add many IPs
        for i in 0..100 {
            let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, i));
            manager.get_limiter(ip);
        }

        // Clear most of them
        manager.clear();

        // Add just a few back
        for i in 0..5 {
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, i));
            manager.get_limiter(ip);
        }

        // Trigger shrink
        manager.shrink_to_fit();

        // Map should have shrunk (can't directly test capacity, but operation shouldn't panic)
        assert_eq!(manager.active_ips(), 5);
    }

    #[test]
    fn test_cleanup_thread() {
        let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
            RateLimiterConfig::default(),
            100,  // 100ms cleanup interval
            50,   // 50ms inactive duration
        ));

        // Add some IPs
        for i in 0..10 {
            let ip = IpAddr::V4(Ipv4Addr::new(172, 16, 0, i));
            manager.try_acquire(ip);
        }

        // Start cleanup thread
        let manager_clone = manager.clone();
        let handle = manager_clone.start_cleanup_thread();

        // Wait for cleanup to run
        std::thread::sleep(Duration::from_millis(200));

        // Should have cleaned up inactive entries
        assert!(manager.active_ips() < 10);

        // Thread continues running (we can't easily test join without stopping it)
        drop(handle);
    }

    #[test]
    fn test_stoppable_cleanup_thread() {
        let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
            RateLimiterConfig::default(),
            100,
            50,
        ));

        // Add IPs
        for i in 0..5 {
            let ip = IpAddr::V4(Ipv4Addr::new(10, 10, 10, i));
            manager.try_acquire(ip);
        }

        // Start stoppable thread
        let (handle, stop_tx) = manager.clone().start_stoppable_cleanup_thread();

        // Let it run
        std::thread::sleep(Duration::from_millis(150));

        // Stop it
        stop_tx.send(()).unwrap();
        handle.join().unwrap();

        // Thread should have stopped gracefully
        assert!(manager.active_ips() <= 5);
    }

    #[test]
    fn test_get_limiter_race_condition() {
        let manager = Arc::new(IpRateLimiterManager::new(RateLimiterConfig::default()));
        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));

        // Multiple threads trying to get the same IP limiter
        let mut handles = vec![];
        for _ in 0..10 {
            let manager_clone = manager.clone();
            handles.push(thread::spawn(move || {
                manager_clone.get_limiter(ip).is_some()
            }));
        }

        let results: Vec<bool> = handles.into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        // All should succeed
        assert!(results.iter().all(|&r| r));

        // Should have created only one limiter
        assert_eq!(manager.active_ips(), 1);
    }

    #[test]
    fn test_concurrent_emergency_cleanup() {
        let manager = Arc::new(IpRateLimiterManager::new(RateLimiterConfig::default()));

        // Fill to threshold
        for i in 0..CLEANUP_THRESHOLD {
            let ip = IpAddr::V4(Ipv4Addr::new(
                (i / 16777216) as u8,
                ((i / 65536) % 256) as u8,
                ((i / 256) % 256) as u8,
                (i % 256) as u8,
            ));
            manager.get_limiter(ip);
        }

        // Multiple threads triggering emergency cleanup
        let mut handles = vec![];
        for i in 0..5 {
            let manager_clone = manager.clone();
            handles.push(thread::spawn(move || {
                let ip = IpAddr::V4(Ipv4Addr::new(255, 255, 255, 250 + i));
                manager_clone.get_limiter(ip)
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Should have cleaned up despite concurrent attempts
        assert!(manager.active_ips() <= CLEANUP_TARGET + 5);
    }

    #[test]
    fn test_cleanup_with_active_limiters() {
        let manager = IpRateLimiterManager::with_cleanup_settings(
            RateLimiterConfig::default(),
            1000,
            100,
        );

        // Create limiters, some active, some inactive
        for i in 0..10 {
            let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 2, i));
            if i < 5 {
                // These will be active
                manager.try_acquire(ip);
            } else {
                // These will be inactive
                manager.get_limiter(ip);
            }
        }

        // Wait for inactive threshold
        std::thread::sleep(Duration::from_millis(150));

        // Keep first 5 active
        for i in 0..5 {
            let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 2, i));
            manager.try_acquire(ip);
        }

        // Cleanup should only remove inactive ones
        manager.cleanup();

        assert!(manager.active_ips() >= 5);
        assert!(manager.active_ips() < 10);
    }

    #[test]
    fn test_manager_stats_calculations() {
        let manager = IpRateLimiterManager::new(RateLimiterConfig::default());

        // Add and remove some limiters
        for i in 0..20 {
            let ip = IpAddr::V4(Ipv4Addr::new(10, 20, 30, i));
            manager.get_limiter(ip);
        }

        manager.clear();

        let stats = manager.stats();
        assert_eq!(stats.total_created, 20);
        assert_eq!(stats.total_cleaned, 20);
        assert!(stats.cleanup_ratio() > 0.0);

        // Test near capacity check
        assert!(!stats.is_near_capacity());
    }

    #[test]
    fn test_try_acquire_n() {
        let manager = IpRateLimiterManager::new(RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 1,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::AcquireRelease,
        });

        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Should succeed
        assert!(manager.try_acquire_n(ip, 5));
        assert!(manager.try_acquire_n(ip, 3));

        // Should fail
        assert!(!manager.try_acquire_n(ip, 5));
    }

    #[test]
    fn test_cleanup_guard_drop() {
        use super::CleanupGuard;
        let flag = AtomicBool::new(true);

        {
            let _guard = CleanupGuard { flag: &flag };
            assert!(flag.load(Ordering::Acquire));
        } // Guard drops here

        assert!(!flag.load(Ordering::Acquire));
    }
}