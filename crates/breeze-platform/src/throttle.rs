//! Duty-cycle CPU throttle. Cycles suspend/resume on a worker thread so a
//! process gets only a fraction of wall-clock running time. Also supports a
//! full freeze (deep idle) and a typing boost that holds full speed briefly.
//!
//! Before every signal the target PID is re-validated by name (recycled PIDs
//! must never be signalled); the check is cached and only refreshed every few
//! seconds on the hot path.

use crate::{proc, suspend};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const PID_VALIDATION_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_BOOST_WINDOW: Duration = Duration::from_millis(300);

/// What the worker should be doing.
enum Mode {
    /// Full speed, no throttling.
    Idle,
    /// Duty-cycle throttling at the given fraction (0.01..1.0).
    Throttle(f64),
    /// Fully suspended (deep idle).
    Frozen,
    /// Full speed until the given instant, then fire the boost-expire callback.
    Boost(Instant),
}

struct Inner {
    mode: Mode,
    is_stopped: bool,
    pid_invalid: bool,
    pid_valid_cached: bool,
    last_validation: Instant,
    boosting: bool,
    on_boost_expire: Option<Box<dyn FnMut() + Send>>,
    shutdown: bool,
    /// Bumped on every control change so the worker abandons its current sleep.
    generation: u64,
}

struct Shared {
    pid: i32,
    allowed: Vec<String>,
    inner: Mutex<Inner>,
    cv: Condvar,
}

/// Throttles one process. Dropping it stops throttling and resumes the process.
pub struct CpuThrottle {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl CpuThrottle {
    /// Throttle `pid`, accepting it only while its process name is in the
    /// allow-list (a recycled PID with any other name is left alone).
    pub fn new(pid: i32) -> CpuThrottle {
        CpuThrottle::with_allowed(pid, breeze_core::agents::AGENT_THROTTLE_NAMES)
    }

    /// Throttle `pid`, accepting it only while its name is one of `allowed`.
    pub fn with_allowed(pid: i32, allowed: &[&str]) -> CpuThrottle {
        let shared = Arc::new(Shared {
            pid,
            allowed: allowed.iter().map(|s| s.to_string()).collect(),
            inner: Mutex::new(Inner {
                mode: Mode::Idle,
                is_stopped: false,
                pid_invalid: false,
                pid_valid_cached: false,
                last_validation: Instant::now() - PID_VALIDATION_INTERVAL,
                boosting: false,
                on_boost_expire: None,
                shutdown: false,
                generation: 0,
            }),
            cv: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::spawn(move || run_worker(worker_shared));
        CpuThrottle { shared, worker: Some(worker) }
    }

    /// Set the duty cycle (0.01..1.0). `>= 1.0` stops throttling entirely.
    /// Ignored while a typing boost is active.
    pub fn set_duty_cycle(&self, cycle: f64) {
        let mut inner = self.shared.inner.lock().unwrap();
        if inner.boosting {
            return;
        }
        let clamped = cycle.clamp(0.01, 1.0);
        if clamped >= 1.0 {
            stop_throttling(&self.shared, &mut inner);
        } else {
            inner.mode = Mode::Throttle(clamped);
        }
        bump(&self.shared, &mut inner);
    }

    /// Fully suspend the process (deep idle). Ignored while boosting.
    pub fn freeze(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        if inner.boosting {
            return;
        }
        inner.mode = Mode::Frozen;
        bump(&self.shared, &mut inner);
    }

    /// Resume from a freeze back to full speed (no throttling).
    pub fn resume(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.mode = Mode::Idle;
        bump(&self.shared, &mut inner);
    }

    /// Cease all throttling and clear any pending typing boost — full speed.
    pub fn stop(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.boosting = false;
        inner.on_boost_expire = None;
        stop_throttling(&self.shared, &mut inner);
        bump(&self.shared, &mut inner);
    }

    /// Hold full speed for `window` after the last keystroke, then run `expire`
    /// (typically restores the state-appropriate duty cycle). Re-arming just
    /// pushes the deadline out.
    pub fn boost(&self, window: Duration, expire: impl FnMut() + Send + 'static) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.boosting = true;
        inner.on_boost_expire = Some(Box::new(expire));
        inner.mode = Mode::Boost(Instant::now() + window);
        bump(&self.shared, &mut inner);
    }

    /// Convenience: boost with the default ~300ms window.
    pub fn boost_default(&self, expire: impl FnMut() + Send + 'static) {
        self.boost(DEFAULT_BOOST_WINDOW, expire);
    }
}

impl Drop for CpuThrottle {
    fn drop(&mut self) {
        // Tolerate a poisoned lock so drop never double-panics.
        if let Ok(mut inner) = self.shared.inner.lock() {
            inner.shutdown = true;
            inner.mode = Mode::Idle;
            inner.generation += 1;
        }
        self.shared.cv.notify_all();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Re-validate the PID by name. Returns false (and forces Idle + resume) if the
/// PID was recycled to a process we don't recognise.
fn validate(shared: &Shared, inner: &mut Inner, force: bool) -> bool {
    if inner.pid_invalid {
        // Already known dead: make sure we're idle so the worker parks instead
        // of busy-spinning in a throttle/freeze branch.
        stop_throttling(shared, inner);
        return false;
    }
    if !force && inner.pid_valid_cached && inner.last_validation.elapsed() < PID_VALIDATION_INTERVAL {
        return true;
    }
    let name = proc::process_name(shared.pid);
    if !shared.allowed.iter().any(|a| a == &name) {
        inner.pid_invalid = true;
        stop_throttling(shared, inner);
        return false;
    }
    inner.pid_valid_cached = true;
    inner.last_validation = Instant::now();
    true
}

/// Drop to Idle and resume the process if it was suspended.
fn stop_throttling(shared: &Shared, inner: &mut Inner) {
    inner.mode = Mode::Idle;
    if inner.is_stopped {
        suspend::resume(shared.pid);
        inner.is_stopped = false;
    }
}

fn bump(shared: &Shared, inner: &mut Inner) {
    inner.generation += 1;
    shared.cv.notify_all();
}

/// Sleep up to `dur`, returning early if a control change bumped `generation`
/// or shutdown was requested. Returns the still-locked guard.
fn sleep_or_wake<'a>(
    shared: &Shared,
    inner: std::sync::MutexGuard<'a, Inner>,
    dur: Duration,
    gen: u64,
) -> std::sync::MutexGuard<'a, Inner> {
    let (guard, _) = shared
        .cv
        .wait_timeout_while(inner, dur, |i| i.generation == gen && !i.shutdown)
        .unwrap();
    guard
}

fn run_worker(shared: Arc<Shared>) {
    let mut inner = shared.inner.lock().unwrap();
    loop {
        if inner.shutdown {
            if inner.is_stopped {
                suspend::resume(shared.pid);
                inner.is_stopped = false;
            }
            return;
        }
        let gen = inner.generation;
        match inner.mode {
            Mode::Idle => {
                if inner.is_stopped {
                    suspend::resume(shared.pid);
                    inner.is_stopped = false;
                }
                inner = shared
                    .cv
                    .wait_while(inner, |i| i.generation == gen && !i.shutdown)
                    .unwrap();
            }
            Mode::Frozen => {
                if !inner.is_stopped && validate(&shared, &mut inner, false) {
                    suspend::suspend(shared.pid);
                    inner.is_stopped = true;
                }
                inner = shared
                    .cv
                    .wait_while(inner, |i| i.generation == gen && !i.shutdown)
                    .unwrap();
            }
            Mode::Boost(expire) => {
                if inner.is_stopped {
                    suspend::resume(shared.pid);
                    inner.is_stopped = false;
                }
                let now = Instant::now();
                if now >= expire {
                    let cb = inner.on_boost_expire.take();
                    inner.boosting = false;
                    inner.mode = Mode::Idle;
                    drop(inner);
                    if let Some(mut cb) = cb {
                        cb();
                    }
                    inner = shared.inner.lock().unwrap();
                } else {
                    inner = sleep_or_wake(&shared, inner, expire - now, gen);
                }
            }
            Mode::Throttle(duty) => {
                // Lower duty → longer cycle, fewer signals.
                let cycle = if duty < 0.10 { 0.2 } else { 0.1 };
                let run_time = cycle * duty;

                // Run phase: ensure the process is running.
                if !validate(&shared, &mut inner, false) {
                    continue;
                }
                if inner.is_stopped {
                    suspend::resume(shared.pid);
                    inner.is_stopped = false;
                }
                inner = sleep_or_wake(&shared, inner, Duration::from_secs_f64(run_time), gen);
                if inner.generation != gen || inner.shutdown {
                    continue;
                }

                // Stop phase: suspend for the remainder of the cycle.
                if !validate(&shared, &mut inner, false) {
                    continue;
                }
                if !inner.is_stopped {
                    suspend::suspend(shared.pid);
                    inner.is_stopped = true;
                }
                inner = sleep_or_wake(&shared, inner, Duration::from_secs_f64(cycle - run_time), gen);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::proc;
    use std::process::{Child, Command};

    /// A CPU-burning child whose name we whitelist for the throttle.
    fn spawn_burner() -> (Child, i32, String) {
        // `yes` writes endlessly to a discarded pipe — pure CPU burn.
        let child = Command::new("yes")
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn yes");
        let pid = child.id() as i32;
        std::thread::sleep(Duration::from_millis(80));
        let name = proc::process_name(pid);
        (child, pid, name)
    }

    fn user_ns(pid: i32) -> u64 {
        proc::cpu_times(pid).map(|(u, s)| u + s).unwrap_or(0)
    }

    #[test]
    fn freeze_halts_cpu_progress_and_resume_restarts_it() {
        let (mut child, pid, name) = spawn_burner();
        let t = CpuThrottle::with_allowed(pid, &[name.as_str()]);

        // Running: CPU time advances.
        let a = user_ns(pid);
        std::thread::sleep(Duration::from_millis(150));
        let b = user_ns(pid);
        assert!(b > a, "burner should accumulate CPU time while running");

        // Frozen: CPU time is essentially flat.
        t.freeze();
        std::thread::sleep(Duration::from_millis(120));
        let c = user_ns(pid);
        std::thread::sleep(Duration::from_millis(200));
        let d = user_ns(pid);
        assert!(
            d.saturating_sub(c) < (b - a) / 4,
            "frozen process should barely advance: dc={}, running_delta={}",
            d.saturating_sub(c),
            b - a
        );

        // Resumed: advances again.
        t.resume();
        std::thread::sleep(Duration::from_millis(150));
        let e = user_ns(pid);
        assert!(e > d, "resumed process should accumulate CPU time again");

        drop(t);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn duty_cycle_reduces_cpu_progress() {
        let (mut child, pid, name) = spawn_burner();

        // Full-speed baseline.
        let f0 = user_ns(pid);
        std::thread::sleep(Duration::from_millis(400));
        let full_delta = user_ns(pid) - f0;

        // Throttle to 5%.
        let t = CpuThrottle::with_allowed(pid, &[name.as_str()]);
        t.set_duty_cycle(0.05);
        std::thread::sleep(Duration::from_millis(200)); // let it settle
        let g0 = user_ns(pid);
        std::thread::sleep(Duration::from_millis(800));
        let throttled_delta = user_ns(pid) - g0;

        // Normalise to per-ms; throttled should be far below full speed.
        let full_rate = full_delta as f64 / 400.0;
        let throttled_rate = throttled_delta as f64 / 800.0;
        assert!(
            throttled_rate < full_rate * 0.5,
            "throttled rate {throttled_rate} should be < half of full rate {full_rate}"
        );

        drop(t);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn recycled_pid_with_wrong_name_is_not_signalled() {
        let (mut child, pid, _name) = spawn_burner();
        // Allow only the agent names — our `yes` child won't match, so freeze must no-op.
        let t = CpuThrottle::with_allowed(pid, breeze_core::agents::AGENT_THROTTLE_NAMES);
        let a = user_ns(pid);
        t.freeze();
        std::thread::sleep(Duration::from_millis(200));
        let b = user_ns(pid);
        assert!(b > a, "process must keep running when name isn't whitelisted");
        drop(t);
        let _ = child.kill();
        let _ = child.wait();
    }
}
