//! Per-session orchestrator: samples CPU/RSS, derives the session state, and
//! drives the throttle, child lifecycle, polling cadence, and renderer focus
//! hints. Polls less as the session quiets and stops entirely in deep idle.

use crate::child_lifecycle::ChildLifecycle;
use crate::proc;
use crate::throttle::CpuThrottle;
use breeze_core::process_monitor::{ProcessMonitor as Accounting, Snapshot};
use breeze_core::session_state::{evaluate, SessionState, IGNORED_CHILD_NAMES};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const PRIO_PROCESS: i32 = 0;
const RENICE_PRIORITY: i32 = 10;

// Adaptive polling intervals (seconds).
const ACTIVE_INTERVAL: f64 = 2.0;
const WAITING_INTERVAL: f64 = 5.0;
const IDLE_INTERVAL: f64 = 10.0;
const LOW_POWER_ACTIVE_INTERVAL: f64 = 3.0;
const LOW_POWER_WAITING_INTERVAL: f64 = 10.0;
const LOW_POWER_IDLE_INTERVAL: f64 = 20.0;

const CHILD_SCAN_SAMPLE_RATE: u32 = 16;
const STATS_DISPATCH_MIN_INTERVAL: f64 = 3.0;

/// CPU-throttle policy for a state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThrottleAction {
    /// Full speed (no throttling).
    Full,
    /// Duty-cycle at this fraction.
    Duty(f64),
    /// Fully suspended.
    Freeze,
}

/// Throttle policy by state. Streaming needs a usable frame rate; waiting just
/// needs to feed the idle render clock; tools get full CPU; idle freezes.
pub fn throttle_action(state: SessionState, low_power: bool) -> ThrottleAction {
    match state {
        SessionState::RunningTools => ThrottleAction::Full,
        SessionState::StreamingAPI => ThrottleAction::Duty(if low_power { 0.08 } else { 0.15 }),
        SessionState::WaitingInput => ThrottleAction::Duty(if low_power { 0.015 } else { 0.03 }),
        SessionState::DeepIdle => ThrottleAction::Freeze,
    }
}

/// Poll interval by state. `None` means stop polling (deep idle — the process
/// is suspended and can't change state without an event that wakes it).
pub fn poll_interval(state: SessionState, low_power: bool) -> Option<f64> {
    let (active, waiting, idle) = if low_power {
        (LOW_POWER_ACTIVE_INTERVAL, LOW_POWER_WAITING_INTERVAL, LOW_POWER_IDLE_INTERVAL)
    } else {
        (ACTIVE_INTERVAL, WAITING_INTERVAL, IDLE_INTERVAL)
    };
    let _ = idle;
    match state {
        SessionState::RunningTools | SessionState::StreamingAPI => Some(active),
        SessionState::WaitingInput => Some(waiting),
        SessionState::DeepIdle => None,
    }
}

/// Whether the renderer should be told it has focus (full clock) vs blurred
/// (halved clock).
pub fn focus_in(state: SessionState) -> bool {
    matches!(state, SessionState::RunningTools | SessionState::StreamingAPI)
}

fn mono() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(unix)]
fn renice(pid: i32) {
    unsafe {
        libc::setpriority(PRIO_PROCESS, pid as libc::id_t, RENICE_PRIORITY);
    }
}
#[cfg(not(unix))]
fn renice(_pid: i32) {}

type Cb1<T> = Option<Box<dyn FnMut(T) + Send>>;

#[derive(Default)]
struct Callbacks {
    on_state_change: Cb1<SessionState>,
    on_stats_update: Option<Box<dyn FnMut(Snapshot, SessionState) + Send>>,
    on_memory_alert: Cb1<f64>,
    on_send_focus: Cb1<bool>,
}

struct Inner {
    current_state: SessionState,
    last_keystroke: f64,
    last_output: f64,
    forced_idle: bool,
    low_power: bool,
    child_scan_counter: u32,
    polling: bool,
    current_interval: f64,
    last_dispatched_state: SessionState,
    last_stats_dispatch: f64,
    shutdown: bool,
    generation: u64,
    // Recorded for observability/tests.
    last_action: Option<ThrottleAction>,
    last_interval_set: Option<f64>,
}

struct Shared {
    pid: i32,
    inner: Mutex<Inner>,
    cv: Condvar,
    throttle: CpuThrottle,
    lifecycle: ChildLifecycle,
    accounting: Mutex<Accounting>,
    callbacks: Mutex<Callbacks>,
}

pub struct SessionManager {
    shared: Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl SessionManager {
    pub fn new(agent_pid: i32) -> SessionManager {
        SessionManager::build(agent_pid, CpuThrottle::new(agent_pid))
    }

    /// Construct with a throttle that accepts the given process names (for
    /// driving against a test process not literally named like the agent).
    pub fn with_allowed(agent_pid: i32, allowed: &[&str]) -> SessionManager {
        SessionManager::build(agent_pid, CpuThrottle::with_allowed(agent_pid, allowed))
    }

    fn build(agent_pid: i32, throttle: CpuThrottle) -> SessionManager {
        renice(agent_pid);
        proc::for_each_child(agent_pid, renice);

        let now = mono();
        let shared = Arc::new(Shared {
            pid: agent_pid,
            inner: Mutex::new(Inner {
                current_state: SessionState::WaitingInput,
                last_keystroke: now,
                last_output: now,
                forced_idle: false,
                low_power: false,
                child_scan_counter: 0,
                polling: false,
                current_interval: WAITING_INTERVAL,
                last_dispatched_state: SessionState::WaitingInput,
                last_stats_dispatch: 0.0,
                shutdown: false,
                generation: 0,
                last_action: None,
                last_interval_set: None,
            }),
            cv: Condvar::new(),
            throttle,
            lifecycle: ChildLifecycle::new(agent_pid),
            accounting: Mutex::new(Accounting::new(agent_pid)),
            callbacks: Mutex::new(Callbacks::default()),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::spawn(move || run_worker(worker_shared));
        SessionManager { shared, worker: Some(worker) }
    }

    pub fn on_state_change(&self, f: impl FnMut(SessionState) + Send + 'static) {
        self.shared.callbacks.lock().unwrap().on_state_change = Some(Box::new(f));
    }
    pub fn on_stats_update(&self, f: impl FnMut(Snapshot, SessionState) + Send + 'static) {
        self.shared.callbacks.lock().unwrap().on_stats_update = Some(Box::new(f));
    }
    pub fn on_memory_alert(&self, f: impl FnMut(f64) + Send + 'static) {
        self.shared.callbacks.lock().unwrap().on_memory_alert = Some(Box::new(f));
    }
    pub fn on_send_focus_event(&self, f: impl FnMut(bool) + Send + 'static) {
        self.shared.callbacks.lock().unwrap().on_send_focus = Some(Box::new(f));
    }

    pub fn current_state(&self) -> SessionState {
        self.shared.inner.lock().unwrap().current_state
    }

    /// Begin polling at the waiting cadence.
    pub fn start(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.polling = true;
        inner.current_interval =
            if inner.low_power { LOW_POWER_WAITING_INTERVAL } else { WAITING_INTERVAL };
        bump(&self.shared, &mut inner);
    }

    pub fn stop(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.polling = false;
        bump(&self.shared, &mut inner);
        drop(inner);
        self.shared.throttle.stop();
    }

    /// Force deep idle (background tab): freeze + stop polling.
    pub fn force_deep_idle(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.forced_idle = true;
        transition_to(&self.shared, &mut inner, SessionState::DeepIdle);
        inner.polling = false;
        bump(&self.shared, &mut inner);
    }

    /// Resume from forced idle (tab visible again).
    pub fn resume_from_forced(&self) {
        self.shared.throttle.resume();
        let mut inner = self.shared.inner.lock().unwrap();
        inner.forced_idle = false;
        inner.polling = true;
        inner.current_interval =
            if inner.low_power { LOW_POWER_WAITING_INTERVAL } else { WAITING_INTERVAL };
        transition_to(&self.shared, &mut inner, SessionState::WaitingInput);
        bump(&self.shared, &mut inner);
    }

    pub fn set_low_power_mode(&self, enabled: bool) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.low_power = enabled;
        let state = inner.current_state;
        transition_to(&self.shared, &mut inner, state);
        bump(&self.shared, &mut inner);
    }

    pub fn record_keystroke(&self) {
        // Suspend duty-cycle throttling while keys flow; decay back after.
        let decay_shared = Arc::clone(&self.shared);
        self.shared.throttle.boost_default(move || {
            let mut inner = decay_shared.inner.lock().unwrap();
            let state = inner.current_state;
            apply_throttle(&decay_shared, &mut inner, state);
        });
        let mut inner = self.shared.inner.lock().unwrap();
        inner.last_keystroke = mono();
        if inner.current_state == SessionState::DeepIdle {
            inner.polling = true;
            inner.current_interval =
                if inner.low_power { LOW_POWER_WAITING_INTERVAL } else { WAITING_INTERVAL };
            transition_to(&self.shared, &mut inner, SessionState::WaitingInput);
            bump(&self.shared, &mut inner);
        }
    }

    pub fn record_output(&self) {
        self.shared.inner.lock().unwrap().last_output = mono();
    }

    /// One evaluation step. `has_child` is the child-process scan. Public for
    /// the worker and for transition-table testing with injected signals.
    pub fn step(&self, now: f64, has_child: impl Fn() -> bool) {
        let mut inner = self.shared.inner.lock().unwrap();
        evaluate_step(&self.shared, &mut inner, now, &has_child);
    }

    #[cfg(test)]
    fn set_times(&self, output_ago: f64, keystroke_ago: f64, now: f64) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.last_output = now - output_ago;
        inner.last_keystroke = now - keystroke_ago;
    }

    #[cfg(test)]
    fn last_action(&self) -> Option<ThrottleAction> {
        self.shared.inner.lock().unwrap().last_action
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        // Tolerate a poisoned lock (a panicking test) so drop never double-panics.
        if let Ok(mut inner) = self.shared.inner.lock() {
            inner.shutdown = true;
            bump(&self.shared, &mut inner);
        } else {
            self.shared.cv.notify_all();
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn bump(shared: &Shared, inner: &mut Inner) {
    inner.generation += 1;
    shared.cv.notify_all();
}

fn apply_throttle(shared: &Shared, inner: &mut Inner, state: SessionState) {
    let action = throttle_action(state, inner.low_power);
    inner.last_action = Some(action);
    match action {
        ThrottleAction::Full => shared.throttle.stop(),
        ThrottleAction::Duty(d) => shared.throttle.set_duty_cycle(d),
        ThrottleAction::Freeze => shared.throttle.freeze(),
    }
}

fn transition_to(shared: &Shared, inner: &mut Inner, new_state: SessionState) {
    let old = inner.current_state;
    inner.current_state = new_state;

    // Waking from deep idle restarts polling.
    if old == SessionState::DeepIdle && new_state != SessionState::DeepIdle {
        inner.polling = true;
    }

    match poll_interval(new_state, inner.low_power) {
        Some(i) => {
            inner.current_interval = i;
            inner.last_interval_set = Some(i);
            inner.polling = true;
        }
        None => {
            inner.polling = false; // deep idle: stop polling
            inner.last_interval_set = None;
        }
    }

    if new_state == SessionState::RunningTools {
        proc::for_each_child(shared.pid, renice);
    }

    let focus = focus_in(new_state);
    apply_throttle(shared, inner, new_state);
    shared.lifecycle.handle_state_transition(old, new_state);

    // Focus + state-change callbacks fire on every transition (matching the
    // original's transitionTo). Locks only the callbacks mutex, not `inner`.
    notify_transition(shared, focus, new_state);
}

fn notify_transition(shared: &Shared, focus: bool, new_state: SessionState) {
    let mut cbs = shared.callbacks.lock().unwrap();
    if let Some(cb) = cbs.on_send_focus.as_mut() {
        cb(focus);
    }
    if let Some(cb) = cbs.on_state_change.as_mut() {
        cb(new_state);
    }
}

fn evaluate_step(shared: &Shared, inner: &mut Inner, now: f64, has_child: &dyn Fn() -> bool) {
    if inner.forced_idle {
        if inner.current_state != SessionState::DeepIdle {
            transition_to(shared, inner, SessionState::DeepIdle);
        }
        return;
    }

    let time_since_output = now - inner.last_output;
    let time_since_keystroke = now - inner.last_keystroke;

    inner.child_scan_counter = inner.child_scan_counter.wrapping_add(1);
    let skip = inner.child_scan_counter % CHILD_SCAN_SAMPLE_RATE != 0
        && matches!(
            inner.current_state,
            SessionState::StreamingAPI | SessionState::WaitingInput
        );

    let new_state = evaluate(
        time_since_output,
        time_since_keystroke,
        inner.current_state,
        skip,
        has_child,
    );

    if new_state != inner.current_state {
        transition_to(shared, inner, new_state);
    }
}

fn run_worker(shared: Arc<Shared>) {
    let mut inner = shared.inner.lock().unwrap();
    loop {
        if inner.shutdown {
            return;
        }
        if !inner.polling {
            let gen = inner.generation;
            inner = shared
                .cv
                .wait_while(inner, |i| i.generation == gen && !i.shutdown && !i.polling)
                .unwrap();
            continue;
        }

        let pid = shared.pid;
        let interval = inner.current_interval;
        let gen = inner.generation;

        // Sample raw counters and update accounting (release inner during syscalls).
        drop(inner);
        let task = proc::cpu_times(pid).map(|(u, s)| (u, s, proc::rss_bytes(pid)));
        let now = mono();

        let mut snapshot: Option<Snapshot> = None;
        let mut alert: Option<f64> = None;
        if let Some((u, s, rss)) = task {
            let mut acct = shared.accounting.lock().unwrap();
            let cpu = acct.record_cpu(u, s, now);
            acct.push_rss(rss);
            let child_count = acct.next_child_count(|| proc::child_count(pid));
            alert = acct.check_memory_alert(rss as f64 / (1024.0 * 1024.0));
            snapshot = Some(Snapshot { pid, cpu_percent: cpu, rss_bytes: rss, child_count, timestamp: now });
        }

        if let Some(mb) = alert {
            let mut cbs = shared.callbacks.lock().unwrap();
            if let Some(cb) = cbs.on_memory_alert.as_mut() {
                cb(mb);
            }
        }

        // Evaluate + transition.
        inner = shared.inner.lock().unwrap();
        evaluate_step(&shared, &mut inner, now, &|| {
            proc::has_active_child(pid, &IGNORED_CHILD_NAMES)
        });
        let state = inner.current_state;

        // Throttled stats dispatch.
        if let Some(snap) = snapshot {
            let should = state != inner.last_dispatched_state
                || now - inner.last_stats_dispatch >= STATS_DISPATCH_MIN_INTERVAL;
            if should {
                inner.last_dispatched_state = state;
                inner.last_stats_dispatch = now;
                drop(inner);
                let mut cbs = shared.callbacks.lock().unwrap();
                if let Some(cb) = cbs.on_stats_update.as_mut() {
                    cb(snap, state);
                }
                inner = shared.inner.lock().unwrap();
            }
        }

        // Sleep until the next tick (interruptible by control changes).
        if inner.generation == gen && !inner.shutdown && inner.polling {
            let (g, _) = shared
                .cv
                .wait_timeout_while(inner, Duration::from_secs_f64(interval), |i| {
                    i.generation == gen && !i.shutdown
                })
                .unwrap();
            inner = g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_throttle_actions() {
        assert_eq!(throttle_action(SessionState::RunningTools, false), ThrottleAction::Full);
        assert_eq!(throttle_action(SessionState::StreamingAPI, false), ThrottleAction::Duty(0.15));
        assert_eq!(throttle_action(SessionState::StreamingAPI, true), ThrottleAction::Duty(0.08));
        assert_eq!(throttle_action(SessionState::WaitingInput, false), ThrottleAction::Duty(0.03));
        assert_eq!(throttle_action(SessionState::WaitingInput, true), ThrottleAction::Duty(0.015));
        assert_eq!(throttle_action(SessionState::DeepIdle, false), ThrottleAction::Freeze);
    }

    #[test]
    fn policy_intervals_and_focus() {
        assert_eq!(poll_interval(SessionState::RunningTools, false), Some(2.0));
        assert_eq!(poll_interval(SessionState::WaitingInput, false), Some(5.0));
        assert_eq!(poll_interval(SessionState::WaitingInput, true), Some(10.0));
        assert_eq!(poll_interval(SessionState::DeepIdle, false), None);
        assert!(focus_in(SessionState::StreamingAPI));
        assert!(!focus_in(SessionState::WaitingInput));
    }

    #[test]
    fn transition_table_via_step() {
        // PID 1 (launchd) won't match throttle's allowed names → throttle no-ops,
        // but the state machine + policy still run. We drive step() with injected
        // time signals and child-scan results.
        let sm = SessionManager::with_allowed(1, &["nonexistent-name"]);
        sm.stop(); // halt the worker's own polling; we step manually
        std::thread::sleep(Duration::from_millis(20));

        let now = 1000.0;

        // Recent output, no tools → streaming.
        sm.set_times(1.0, 1.0, now);
        sm.step(now, || false);
        assert_eq!(sm.current_state(), SessionState::StreamingAPI);
        assert_eq!(sm.last_action(), Some(ThrottleAction::Duty(0.15)));

        // Active child → tools (full). The child scan is subsampled during
        // streaming/waiting (every 16th tick), so it takes up to 16 ticks to
        // notice a tool — faithful to the syscall-saving design.
        sm.set_times(10.0, 10.0, now);
        for _ in 0..CHILD_SCAN_SAMPLE_RATE {
            sm.step(now, || true);
        }
        assert_eq!(sm.current_state(), SessionState::RunningTools);
        assert_eq!(sm.last_action(), Some(ThrottleAction::Full));

        // Quiet but recent-ish, no tools → waiting.
        sm.set_times(10.0, 10.0, now);
        sm.step(now, || false);
        assert_eq!(sm.current_state(), SessionState::WaitingInput);
        assert_eq!(sm.last_action(), Some(ThrottleAction::Duty(0.03)));

        // Long silence, no tools → deep idle (freeze).
        sm.set_times(100.0, 100.0, now);
        sm.step(now, || false);
        assert_eq!(sm.current_state(), SessionState::DeepIdle);
        assert_eq!(sm.last_action(), Some(ThrottleAction::Freeze));
    }
}
