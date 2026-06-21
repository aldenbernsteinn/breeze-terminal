//! CPU/RSS accounting for a monitored process. The platform layer drives this:
//! it reads raw counters (user/system nanoseconds, resident bytes) on a timer
//! and feeds them in; all the derived math lives here so it can be tested
//! without any syscalls.

/// Fixed RSS-history window. 40 entries cover ~60s at a 1.5s active interval.
pub const RSS_RING_SIZE: usize = 40;

/// Child count is re-scanned only every Nth tick — children spawn rarely, so a
/// few seconds of staleness is fine and saves lock contention + syscalls.
pub const CHILD_COUNT_SAMPLE_RATE: u32 = 16;

/// Default memory-alert threshold (MB).
pub const DEFAULT_MEMORY_THRESHOLD_MB: f64 = 1500.0;

/// A single point-in-time reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub pid: i32,
    pub cpu_percent: f64,
    pub rss_bytes: u64,
    pub child_count: i64,
    /// Seconds, from a monotonic clock supplied by the caller.
    pub timestamp: f64,
}

impl Snapshot {
    pub fn rss_mb(&self) -> f64 {
        self.rss_bytes as f64 / (1024.0 * 1024.0)
    }
}

/// Holds the rolling state needed to derive CPU%, RSS growth, and the one-shot
/// memory alert from raw counters.
pub struct ProcessMonitor {
    pid: i32,

    last_user_time: u64,
    last_system_time: u64,
    last_sample_time: f64,

    rss_ring: [u64; RSS_RING_SIZE],
    rss_ring_index: usize,
    rss_ring_count: usize,
    rss_first: u64,

    pub memory_threshold_mb: f64,
    memory_alert_fired: bool,

    pub last_cpu: f64,
    pub last_rss_mb: f64,

    cached_child_count: i64,
    child_count_sample_counter: u32,

    current_interval: f64,
}

impl ProcessMonitor {
    pub fn new(pid: i32) -> ProcessMonitor {
        ProcessMonitor {
            pid,
            last_user_time: 0,
            last_system_time: 0,
            last_sample_time: 0.0,
            rss_ring: [0; RSS_RING_SIZE],
            rss_ring_index: 0,
            rss_ring_count: 0,
            rss_first: 0,
            memory_threshold_mb: DEFAULT_MEMORY_THRESHOLD_MB,
            memory_alert_fired: false,
            last_cpu: 0.0,
            last_rss_mb: 0.0,
            cached_child_count: 0,
            child_count_sample_counter: 0,
            current_interval: 0.5,
        }
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn current_interval(&self) -> f64 {
        self.current_interval
    }

    /// CPU% since the previous reading. `user_time`/`system_time` are cumulative
    /// nanoseconds; `now` is monotonic seconds. The first call returns 0 (no
    /// prior baseline).
    pub fn record_cpu(&mut self, user_time: u64, system_time: u64, now: f64) -> f64 {
        let mut cpu_percent = 0.0;
        if self.last_sample_time > 0.0 {
            let elapsed = now - self.last_sample_time;
            if elapsed > 0.0 {
                let user_delta = (user_time - self.last_user_time) as f64 / 1_000_000_000.0;
                let system_delta = (system_time - self.last_system_time) as f64 / 1_000_000_000.0;
                cpu_percent = ((user_delta + system_delta) / elapsed) * 100.0;
            }
        }
        self.last_user_time = user_time;
        self.last_system_time = system_time;
        self.last_sample_time = now;
        self.last_cpu = cpu_percent;
        cpu_percent
    }

    /// Record a resident-size reading into the ring buffer.
    pub fn push_rss(&mut self, rss: u64) {
        self.last_rss_mb = rss as f64 / (1024.0 * 1024.0);
        self.rss_ring[self.rss_ring_index] = rss;
        self.rss_ring_index = (self.rss_ring_index + 1) % RSS_RING_SIZE;
        if self.rss_ring_count < RSS_RING_SIZE {
            self.rss_ring_count += 1;
        }
        if self.rss_ring_count == 1 {
            self.rss_first = rss;
        }
    }

    /// Returns the alert MB the first time RSS crosses the threshold, then never
    /// again.
    pub fn check_memory_alert(&mut self, rss_mb: f64) -> Option<f64> {
        if rss_mb > self.memory_threshold_mb && !self.memory_alert_fired {
            self.memory_alert_fired = true;
            Some(rss_mb)
        } else {
            None
        }
    }

    /// Resolve the child count, running `scan` only on every
    /// [`CHILD_COUNT_SAMPLE_RATE`]th call and returning the cached value
    /// otherwise.
    pub fn next_child_count<F: FnOnce() -> i64>(&mut self, scan: F) -> i64 {
        self.child_count_sample_counter = self.child_count_sample_counter.wrapping_add(1);
        if self.child_count_sample_counter % CHILD_COUNT_SAMPLE_RATE == 0 {
            self.cached_child_count = scan();
        }
        self.cached_child_count
    }

    /// Update the polling interval. Returns `true` if the interval decreased, a
    /// signal to the caller to take a fresh sample immediately rather than wait.
    pub fn set_interval(&mut self, interval: f64) -> bool {
        let should_sample_now = interval < self.current_interval;
        self.current_interval = interval;
        should_sample_now
    }

    /// RSS growth in MB per minute across the ring window.
    pub fn rss_growth_rate(&self) -> f64 {
        if self.rss_ring_count < 2 {
            return 0.0;
        }
        let oldest = self.rss_first;
        let newest = self.rss_ring[(self.rss_ring_index + RSS_RING_SIZE - 1) % RSS_RING_SIZE];
        let approx_minutes = self.rss_ring_count as f64 * self.current_interval / 60.0;
        if approx_minutes <= 0.0 {
            return 0.0;
        }
        let delta_mb = (newest as f64 - oldest as f64) / (1024.0 * 1024.0);
        delta_mb / approx_minutes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn first_cpu_reading_is_zero() {
        let mut m = ProcessMonitor::new(1);
        assert_eq!(m.record_cpu(1_000_000_000, 0, 10.0), 0.0);
    }

    #[test]
    fn cpu_percent_from_delta() {
        let mut m = ProcessMonitor::new(1);
        m.record_cpu(0, 0, 100.0); // baseline
        // 0.5s user + 0.0s system over 1.0s wall = 50%
        let cpu = m.record_cpu(500_000_000, 0, 101.0);
        assert!((cpu - 50.0).abs() < 1e-9, "{cpu}");
        assert_eq!(m.last_cpu, cpu);
    }

    #[test]
    fn memory_alert_fires_once() {
        let mut m = ProcessMonitor::new(1);
        m.memory_threshold_mb = 1000.0;
        assert_eq!(m.check_memory_alert(500.0), None);
        assert_eq!(m.check_memory_alert(1500.0), Some(1500.0));
        assert_eq!(m.check_memory_alert(2000.0), None); // already fired
    }

    #[test]
    fn child_count_subsampled() {
        let mut m = ProcessMonitor::new(1);
        let mut scans = 0;
        // First 15 calls return the (cached) default 0 without scanning.
        for _ in 0..(CHILD_COUNT_SAMPLE_RATE - 1) {
            let c = m.next_child_count(|| {
                scans += 1;
                7
            });
            assert_eq!(c, 0);
        }
        // 16th call scans.
        let c = m.next_child_count(|| {
            scans += 1;
            7
        });
        assert_eq!(c, 7);
        assert_eq!(scans, 1);
    }

    #[test]
    fn growth_rate_zero_until_two_samples() {
        let mut m = ProcessMonitor::new(1);
        assert_eq!(m.rss_growth_rate(), 0.0);
        m.push_rss(10 * MB);
        assert_eq!(m.rss_growth_rate(), 0.0);
    }

    #[test]
    fn growth_rate_mb_per_minute() {
        let mut m = ProcessMonitor::new(1);
        m.set_interval(0.5);
        m.push_rss(10 * MB); // rss_first
        m.push_rss(70 * MB); // newest; +60 MB
        // approx_minutes = 2 * 0.5 / 60 = 1/60 min → 60 MB / (1/60) = 3600 MB/min
        assert!((m.rss_growth_rate() - 3600.0).abs() < 1e-6, "{}", m.rss_growth_rate());
    }

    #[test]
    fn set_interval_signals_immediate_sample_on_decrease() {
        let mut m = ProcessMonitor::new(1);
        assert!(!m.set_interval(2.0)); // 2.0 < 0.5 is false (increase)
        assert!(m.set_interval(0.5)); // 0.5 < 2.0 → sample now
        assert_eq!(m.current_interval(), 0.5);
    }
}
