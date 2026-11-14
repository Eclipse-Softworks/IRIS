//! Cooperative executor for the interpreter's `task<T>` values.
//!
//! `async def f` compiles to a constructor that creates a task and schedules
//! it, plus a poll function that runs the body until it completes or reaches
//! an `await` whose target is not done (see `pass::async_lower`). This module
//! is the interpreter's half of the runtime contract; `iris_runtime.c`
//! implements the same contract natively:
//!
//! * a task holds its poll function, a frame of slots for values that live
//!   across an `await`, a resume state, and -- once done -- its result;
//! * awaiting an unfinished task registers the awaiting task as a waiter;
//!   completing a task schedules its waiters;
//! * leaf tasks (`async_sleep`, `async_recv`, `async_yield`,
//!   `async_readable`) are completed by the executor itself when their
//!   condition holds.
//!
//! The executor is per thread: a task belongs to the thread that created it.
//! It runs whenever its thread waits: on `await` outside an `async def`, and
//! on a blocking channel receive. When nothing is ready it sleeps until the
//! next timer or until some thread sends on a channel.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{InterpError, IrValue, SharedChannel};

/// What completes a leaf task.
#[derive(Debug)]
pub enum Leaf {
    /// A task with a poll function.
    None,
    /// Completes with `0` once the deadline passes.
    Sleep(Instant),
    /// Completes with the next value received from the channel.
    Recv(Arc<SharedChannel>),
    /// Completes with `0` the next time the executor runs out of ready work,
    /// letting every other ready task run first.
    Yield,
    /// Completes with the index of the first of these tasks to finish.
    Any(Vec<TaskRef>),
    /// Completes with `0` once this OS socket can be read without blocking
    /// (data, a pending connection, end of stream or an error); `-1`, an
    /// unknown handle, completes at once so the read reports it.
    Readable(i64),
    /// Completes with `0` once this OS socket can be written without blocking;
    /// `-1` completes at once so the write reports it.
    Writable(i64),
    /// Completes once a background non-blocking I/O operation writes its result.
    AsyncIo(Arc<Mutex<Option<IrValue>>>),
}

pub struct TaskState {
    pub poll_fn: Option<String>,
    pub frame: Vec<Option<IrValue>>,
    pub state: i64,
    pub done: bool,
    pub result: Option<IrValue>,
    pub waiters: Vec<TaskRef>,
    pub scheduled: bool,
    pub cancelled: bool,
    pub leaf: Leaf,
}

// Shallow on purpose: tasks reference each other in cycles (a task's frame
// holds the task it awaits, whose waiters hold the task), so a derived Debug
// could recurse forever.
impl std::fmt::Debug for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskState")
            .field("poll_fn", &self.poll_fn)
            .field("state", &self.state)
            .field("done", &self.done)
            .field("cancelled", &self.cancelled)
            .field("waiters", &self.waiters.len())
            .finish_non_exhaustive()
    }
}

pub type TaskRef = Arc<Mutex<TaskState>>;

fn lock(task: &TaskRef) -> std::sync::MutexGuard<'_, TaskState> {
    task.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
struct Executor {
    ready: VecDeque<TaskRef>,
    leaves: Vec<TaskRef>,
}

thread_local! {
    static EXECUTOR: RefCell<Executor> = RefCell::new(Executor::default());
}

fn new_task(poll_fn: Option<String>, leaf: Leaf) -> TaskRef {
    Arc::new(Mutex::new(TaskState {
        poll_fn,
        frame: Vec::new(),
        state: 0,
        done: false,
        result: None,
        waiters: Vec::new(),
        scheduled: false,
        cancelled: false,
        leaf,
    }))
}

/// A task that runs `poll_fn` when scheduled.
pub fn task_new(poll_fn: &str) -> TaskRef {
    new_task(Some(poll_fn.to_owned()), Leaf::None)
}

pub fn frame_set(task: &TaskRef, slot: i64, value: IrValue) -> Result<(), InterpError> {
    let slot = slot_index(slot)?;
    let mut state = lock(task);
    if state.frame.len() <= slot {
        state.frame.resize(slot + 1, None);
    }
    state.frame[slot] = Some(value);
    Ok(())
}

pub fn frame_get(task: &TaskRef, slot: i64) -> Result<IrValue, InterpError> {
    let index = slot_index(slot)?;
    lock(task)
        .frame
        .get(index)
        .cloned()
        .flatten()
        .ok_or_else(|| InterpError::Unsupported {
            detail: format!("async frame slot {slot} read before it was written"),
        })
}

fn slot_index(slot: i64) -> Result<usize, InterpError> {
    usize::try_from(slot).map_err(|_| InterpError::Unsupported {
        detail: format!("invalid async frame slot {slot}"),
    })
}

pub fn state(task: &TaskRef) -> i64 {
    lock(task).state
}

pub fn set_state(task: &TaskRef, value: i64) {
    lock(task).state = value;
}

pub fn is_done(task: &TaskRef) -> bool {
    lock(task).done
}

/// Put `task` on this thread's ready queue unless it is done or queued.
pub fn schedule(task: &TaskRef) {
    {
        let mut state = lock(task);
        if state.done || state.scheduled {
            return;
        }
        state.scheduled = true;
    }
    EXECUTOR.with(|executor| executor.borrow_mut().ready.push_back(Arc::clone(task)));
}

/// `task` is suspended until `target` completes.
pub fn wait(task: &TaskRef, target: &TaskRef) {
    let ready_now = {
        let mut state = lock(target);
        if state.done {
            true
        } else {
            state.waiters.push(Arc::clone(task));
            false
        }
    };
    if ready_now {
        schedule(task);
    }
}

/// Finish `task` with `value` and wake everything waiting on it.
pub fn complete(task: &TaskRef, value: IrValue) {
    let waiters = {
        let mut state = lock(task);
        if state.done {
            return;
        }
        state.done = true;
        state.result = Some(value);
        state.frame.clear();
        std::mem::take(&mut state.waiters)
    };
    for waiter in &waiters {
        schedule(waiter);
    }
}

pub fn result(task: &TaskRef) -> Result<IrValue, InterpError> {
    let state = lock(task);
    if !state.done {
        return Err(InterpError::Unsupported {
            detail: "result of an unfinished task was read".into(),
        });
    }
    if state.cancelled {
        return Err(InterpError::Panic {
            msg: "awaited a task that was cancelled".into(),
        });
    }
    Ok(state.result.clone().unwrap_or(IrValue::Unit))
}

/// Stop `task`: it is never polled again, a timer or receive it stands for is
/// dropped (a cancelled receive takes no value), and whatever awaits it wakes;
/// awaiting a cancelled task is an error.
pub fn cancel(task: &TaskRef) {
    let waiters = {
        let mut state = lock(task);
        if state.done {
            return;
        }
        state.done = true;
        state.cancelled = true;
        state.frame.clear();
        std::mem::take(&mut state.waiters)
    };
    for waiter in &waiters {
        schedule(waiter);
    }
}

pub fn is_cancelled(task: &TaskRef) -> bool {
    lock(task).cancelled
}

/// `async_any(...)`: a task that completes with the index of the first of
/// `inputs` to finish.
pub fn any(inputs: Vec<TaskRef>) -> TaskRef {
    let task = new_task(None, Leaf::Any(inputs.clone()));
    for input in &inputs {
        // Schedules the any-task right away when the input has finished.
        wait(&task, input);
    }
    task
}

/// Complete an any-task once an input has finished, cancelling the losing
/// timers and receives so that a losing receive never takes a value. Other
/// losing tasks keep running: their handles may still be awaited.
fn finish_any(task: &TaskRef, inputs: &[TaskRef]) {
    let Some(winner) = inputs.iter().position(is_done) else {
        return;
    };
    for (index, input) in inputs.iter().enumerate() {
        let losing_leaf = index != winner
            && matches!(
                lock(input).leaf,
                Leaf::Sleep(_)
                    | Leaf::Recv(_)
                    | Leaf::Yield
                    | Leaf::Readable(_)
                    | Leaf::Writable(_)
                    | Leaf::AsyncIo(_)
            );
        if losing_leaf {
            cancel(input);
        }
    }
    complete(task, IrValue::I64(winner as i64));
}

fn leaf(kind: Leaf) -> TaskRef {
    let task = new_task(None, kind);
    EXECUTOR.with(|executor| executor.borrow_mut().leaves.push(Arc::clone(&task)));
    task
}

pub fn sleep(ms: i64) -> TaskRef {
    let duration = Duration::from_millis(ms.max(0) as u64);
    leaf(Leaf::Sleep(Instant::now() + duration))
}

pub fn recv(channel: Arc<SharedChannel>) -> TaskRef {
    leaf(Leaf::Recv(channel))
}

pub fn yield_now() -> TaskRef {
    leaf(Leaf::Yield)
}

pub fn readable(raw_socket: i64) -> TaskRef {
    leaf(Leaf::Readable(raw_socket))
}

pub fn writable(raw_socket: i64) -> TaskRef {
    leaf(Leaf::Writable(raw_socket))
}

/// Run a blocking I/O operation on a background thread and resolve a `task<T>`
/// without stalling the cooperative executor thread.
pub fn spawn_io<F>(op: F) -> TaskRef
where
    F: FnOnce() -> IrValue + Send + 'static,
{
    let slot = Arc::new(Mutex::new(None));
    let task = leaf(Leaf::AsyncIo(Arc::clone(&slot)));
    std::thread::spawn(move || {
        let value = op();
        *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
        notify_channel_activity();
    });
    task
}

/// How long an executor waiting on sockets polls them before re-checking its
/// other leaves: a channel send does not interrupt the poll.
const SOCKET_RECHECK: Duration = Duration::from_millis(50);

/// Readiness of raw OS sockets through `poll` / `WSAPoll`, the same check
/// the native runtime makes.
mod socket_poll {
    use std::time::Duration;

    #[cfg(windows)]
    #[repr(C)]
    struct PollFd {
        fd: usize,
        events: i16,
        revents: i16,
    }
    #[cfg(windows)]
    const READ: i16 = 0x0100; // POLLRDNORM
    #[cfg(windows)]
    const WRITE: i16 = 0x0010; // POLLWRNORM
    #[cfg(windows)]
    #[link(name = "ws2_32")]
    extern "system" {
        fn WSAPoll(fds: *mut PollFd, count: u32, timeout_ms: i32) -> i32;
    }

    #[cfg(unix)]
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    #[cfg(unix)]
    const READ: i16 = 0x0001; // POLLIN
    #[cfg(unix)]
    const WRITE: i16 = 0x0004; // POLLOUT
    #[cfg(all(unix, target_os = "macos"))]
    type Count = std::os::raw::c_uint;
    #[cfg(all(unix, not(target_os = "macos")))]
    type Count = std::os::raw::c_ulong;
    #[cfg(unix)]
    extern "C" {
        fn poll(fds: *mut PollFd, count: Count, timeout_ms: i32) -> i32;
    }

    /// Which `(socket, writable)` interests are ready, waiting up to `timeout`.
    /// Hang-ups and errors count as ready; so does everything when the poll
    /// itself fails, so the I/O that follows reports it.
    #[cfg(any(windows, unix))]
    pub fn ready_events(interests: &[(i64, bool)], timeout: Duration) -> Vec<bool> {
        if interests.is_empty() {
            return Vec::new();
        }
        let mut fds: Vec<PollFd> = interests
            .iter()
            .map(|&(s, writable)| PollFd {
                fd: s as _,
                events: if writable { WRITE } else { READ },
                revents: 0,
            })
            .collect();
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: `fds` is a live, correctly laid out array of `len` entries.
        #[cfg(windows)]
        let got = unsafe { WSAPoll(fds.as_mut_ptr(), fds.len() as u32, timeout_ms) };
        #[cfg(unix)]
        let got = unsafe { poll(fds.as_mut_ptr(), fds.len() as Count, timeout_ms) };
        fds.iter().map(|fd| got < 0 || fd.revents != 0).collect()
    }

    #[cfg(not(any(windows, unix)))]
    pub fn ready_events(interests: &[(i64, bool)], _timeout: Duration) -> Vec<bool> {
        vec![true; interests.len()]
    }
}

/// How long an executor waiting only on receives sleeps before re-checking
/// them, in case a value arrives without a wake-up.
const RECV_RECHECK: Duration = Duration::from_millis(50);

// Idle executors sleep until the next timer or until any thread sends on a
// channel. Every send bumps the epoch; an executor reads it before checking
// its leaves and sleeps only while it is unchanged, so no send is missed.
static WAKE_EPOCH: AtomicU64 = AtomicU64::new(0);
static IDLE_WAITERS: AtomicUsize = AtomicUsize::new(0);
static WAKE_LOCK: Mutex<()> = Mutex::new(());
static WAKE: Condvar = Condvar::new();

/// Called after every channel send.
pub fn notify_channel_activity() {
    WAKE_EPOCH.fetch_add(1, Ordering::SeqCst);
    if IDLE_WAITERS.load(Ordering::SeqCst) > 0 {
        let _guard = WAKE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        WAKE.notify_all();
    }
}

pub fn wake_epoch() -> u64 {
    WAKE_EPOCH.load(Ordering::SeqCst)
}

/// Sleep for up to `wait`, or until a channel send after `epoch` was read.
/// With `async_readable` sockets pending, wait on those instead.
pub fn idle_wait(epoch: u64, wait: Duration) {
    if wait.is_zero() {
        return;
    }
    let interests: Vec<(i64, bool)> = EXECUTOR.with(|e| {
        e.borrow()
            .leaves
            .iter()
            .filter_map(|task| match lock(task).leaf {
                Leaf::Readable(socket) => Some((socket, false)),
                Leaf::Writable(socket) => Some((socket, true)),
                _ => None,
            })
            .collect()
    });
    if !interests.is_empty() {
        socket_poll::ready_events(&interests, wait);
        return;
    }
    let guard = WAKE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    IDLE_WAITERS.fetch_add(1, Ordering::SeqCst);
    if WAKE_EPOCH.load(Ordering::SeqCst) == epoch {
        let _ = WAKE.wait_timeout(guard, wait);
    }
    IDLE_WAITERS.fetch_sub(1, Ordering::SeqCst);
}

/// Complete every leaf whose condition holds. Returns whether any did, and
/// how long the executor may sleep before a leaf could next become ready
/// (`None` when there are no leaves at all).
fn tick_leaves(after_idle: bool) -> (bool, Option<Duration>) {
    let leaves: Vec<TaskRef> = EXECUTOR.with(|e| std::mem::take(&mut e.borrow_mut().leaves));
    if leaves.is_empty() {
        return (false, None);
    }
    let now = Instant::now();
    // (task, value, when it became ready): expired timers complete in
    // deadline order even when several expire between two ticks.
    let mut finished: Vec<(TaskRef, IrValue, Instant)> = Vec::new();
    let mut pending: Vec<TaskRef> = Vec::new();
    let mut wait: Option<Duration> = None;
    let shorter = |current: Option<Duration>, candidate: Duration| {
        Some(current.map_or(candidate, |c: Duration| c.min(candidate)))
    };
    for task in leaves {
        let outcome = {
            let state = lock(&task);
            if state.done {
                // Cancelled: drop it.
                continue;
            }
            match &state.leaf {
                Leaf::Sleep(deadline) if *deadline <= now => Some((IrValue::I64(0), *deadline)),
                Leaf::Sleep(deadline) => {
                    wait = shorter(wait, *deadline - now);
                    None
                }
                Leaf::Recv(channel) => {
                    let value = channel
                        .queue
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .pop_front();
                    if value.is_some() {
                        channel.not_full.notify_one();
                    } else {
                        // A send wakes the executor; re-check now and then
                        // regardless, as a safety net.
                        wait = shorter(wait, RECV_RECHECK);
                    }
                    value.map(|value| (value, now))
                }
                Leaf::Yield if after_idle => Some((IrValue::I64(0), now)),
                Leaf::Yield => {
                    wait = Some(Duration::ZERO);
                    None
                }
                Leaf::Readable(socket) => {
                    if *socket < 0
                        || socket_poll::ready_events(&[(*socket, false)], Duration::ZERO)[0]
                    {
                        Some((IrValue::I64(0), now))
                    } else {
                        wait = shorter(wait, SOCKET_RECHECK);
                        None
                    }
                }
                Leaf::Writable(socket) => {
                    if *socket < 0
                        || socket_poll::ready_events(&[(*socket, true)], Duration::ZERO)[0]
                    {
                        Some((IrValue::I64(0), now))
                    } else {
                        wait = shorter(wait, SOCKET_RECHECK);
                        None
                    }
                }
                Leaf::AsyncIo(slot) => {
                    let ready_val = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
                    if let Some(val) = ready_val {
                        Some((val, now))
                    } else {
                        wait = shorter(wait, RECV_RECHECK);
                        None
                    }
                }
                Leaf::None | Leaf::Any(_) => None,
            }
        };
        match outcome {
            Some((value, ready_at)) => finished.push((task, value, ready_at)),
            None => pending.push(task),
        }
    }
    finished.sort_by_key(|(_, _, ready_at)| *ready_at);
    let cutoff = finished
        .first()
        .map(|(_, _, earliest)| *earliest + Duration::from_millis(5));
    let mut ready_now = Vec::new();
    for item in finished {
        if cutoff.is_some_and(|c| item.2 > c) {
            wait = Some(Duration::ZERO);
            pending.push(item.0);
        } else {
            ready_now.push(item);
        }
    }
    EXECUTOR.with(|e| e.borrow_mut().leaves.extend(pending));
    let progressed = !ready_now.is_empty();
    for (task, value, _) in ready_now {
        complete(&task, value);
    }
    (progressed, wait)
}

/// What one executor step did.
pub enum Step {
    /// A task was polled or a leaf completed.
    Progress,
    /// Nothing was ready; a timer or receive may be ready after this long.
    Idle(Duration),
    /// Nothing is ready and no task is sleeping or receiving.
    Empty,
}

/// Make progress without blocking: poll one ready task with
/// `poll(task, poll_fn_name)`, or else complete the leaves whose condition
/// holds (yields first, as the ready queue has just drained).
pub fn step(
    poll: &mut impl FnMut(&TaskRef, &str) -> Result<(), InterpError>,
) -> Result<Step, InterpError> {
    let next = EXECUTOR.with(|e| e.borrow_mut().ready.pop_front());
    if let Some(task) = next {
        let (poll_fn, any_inputs) = {
            let mut state = lock(&task);
            state.scheduled = false;
            match (&state.leaf, state.done) {
                (_, true) => (None, None),
                (Leaf::Any(inputs), false) => (None, Some(inputs.clone())),
                (_, false) => (state.poll_fn.clone(), None),
            }
        };
        if let Some(name) = poll_fn {
            poll(&task, &name)?;
        } else if let Some(inputs) = any_inputs {
            finish_any(&task, &inputs);
        }
        return Ok(Step::Progress);
    }
    Ok(match tick_leaves(true) {
        (true, _) => Step::Progress,
        (false, Some(wait)) => Step::Idle(wait),
        (false, None) => Step::Empty,
    })
}

/// Run the executor until `target` is done. Called by `await` outside an
/// `async def`.
pub fn block_on(
    target: &TaskRef,
    mut poll: impl FnMut(&TaskRef, &str) -> Result<(), InterpError>,
) -> Result<IrValue, InterpError> {
    loop {
        if is_done(target) {
            return result(target);
        }
        let epoch = wake_epoch();
        match step(&mut poll)? {
            Step::Progress => {}
            Step::Idle(wait) => idle_wait(epoch, wait),
            Step::Empty => {
                return Err(InterpError::Panic {
                    msg: "await can never complete: no task is ready, sleeping or \
receiving, so the awaited task is waiting on itself or on a task nothing will finish"
                        .into(),
                });
            }
        }
    }
}

/// Multi-threaded work-stealing async task scheduler.
///
/// Each worker owns a local FIFO/LIFO deque and steals from sibling workers'
/// deques (from the back) when both its local deque and the shared global
/// injection queue are empty, allowing tasks to migrate across worker threads.
pub struct WorkStealingScheduler {
    queues: Vec<Mutex<VecDeque<TaskRef>>>,
    global: Mutex<VecDeque<TaskRef>>,
    steals: AtomicU64,
    wake_mu: Mutex<()>,
    wake_cv: Condvar,
}

impl WorkStealingScheduler {
    /// Create a scheduler with `num_workers` (minimum 1) local deques.
    pub fn new(num_workers: usize) -> Arc<Self> {
        let count = num_workers.max(1);
        let queues = (0..count).map(|_| Mutex::new(VecDeque::new())).collect();
        Arc::new(Self {
            queues,
            global: Mutex::new(VecDeque::new()),
            steals: AtomicU64::new(0),
            wake_mu: Mutex::new(()),
            wake_cv: Condvar::new(),
        })
    }

    /// Number of worker deques in this pool.
    pub fn worker_count(&self) -> usize {
        self.queues.len()
    }

    /// Total number of tasks stolen across worker deques.
    pub fn steal_count(&self) -> u64 {
        self.steals.load(Ordering::Relaxed)
    }

    /// Push a ready task onto `worker_id`'s local deque (or global queue if out of range).
    pub fn push_local(&self, worker_id: usize, task: TaskRef) {
        {
            let mut state = lock(&task);
            if state.done || state.scheduled {
                return;
            }
            state.scheduled = true;
        }
        if let Some(q) = self.queues.get(worker_id) {
            q.lock().unwrap_or_else(|p| p.into_inner()).push_front(task);
        } else {
            self.global
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push_back(task);
        }
        self.wake_cv.notify_one();
    }

    /// Inject a task into the shared global queue so any idle worker can pick it up.
    pub fn spawn_global(&self, task: TaskRef) {
        {
            let mut state = lock(&task);
            if state.done || state.scheduled {
                return;
            }
            state.scheduled = true;
        }
        self.global
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push_back(task);
        self.wake_cv.notify_one();
    }

    /// Pop a task for `worker_id`: local front -> global front -> steal back from siblings.
    /// Returns `(task, stolen_from_sibling)`.
    pub fn pop_or_steal(&self, worker_id: usize) -> Option<(TaskRef, bool)> {
        if let Some(q) = self.queues.get(worker_id) {
            if let Some(task) = q.lock().unwrap_or_else(|p| p.into_inner()).pop_front() {
                return Some((task, false));
            }
        }
        if let Some(task) = self
            .global
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
        {
            return Some((task, false));
        }
        let n = self.queues.len();
        for offset in 1..n {
            let victim = (worker_id + offset) % n;
            if let Some(task) = self.queues[victim]
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .pop_back()
            {
                self.steals.fetch_add(1, Ordering::Relaxed);
                return Some((task, true));
            }
        }
        None
    }

    /// Execute tasks across `worker_count()` OS threads until `target` finishes.
    pub fn run_until_done<F, P>(
        &self,
        target: &TaskRef,
        poll_factory: F,
    ) -> Result<IrValue, InterpError>
    where
        F: Fn() -> P + Send + Sync,
        P: FnMut(&TaskRef, &str) -> Result<(), InterpError>,
    {
        let stop = AtomicBool::new(false);
        let first_err: Mutex<Option<InterpError>> = Mutex::new(None);
        let num_workers = self.queues.len();

        std::thread::scope(|scope| {
            for worker_id in 0..num_workers {
                let stop_ref = &stop;
                let err_ref = &first_err;
                let factory_ref = &poll_factory;
                scope.spawn(move || {
                    let mut poll = factory_ref();
                    while !stop_ref.load(Ordering::Acquire) {
                        if is_done(target) {
                            stop_ref.store(true, Ordering::Release);
                            self.wake_cv.notify_all();
                            break;
                        }
                        if let Some((task, _stolen)) = self.pop_or_steal(worker_id) {
                            let (poll_fn, any_inputs) = {
                                let mut state = lock(&task);
                                state.scheduled = false;
                                match (&state.leaf, state.done) {
                                    (_, true) => (None, None),
                                    (Leaf::Any(inputs), false) => (None, Some(inputs.clone())),
                                    (_, false) => (state.poll_fn.clone(), None),
                                }
                            };
                            if let Some(name) = poll_fn {
                                if let Err(err) = poll(&task, &name) {
                                    let mut guard =
                                        err_ref.lock().unwrap_or_else(|p| p.into_inner());
                                    if guard.is_none() {
                                        *guard = Some(err);
                                    }
                                    stop_ref.store(true, Ordering::Release);
                                    self.wake_cv.notify_all();
                                    break;
                                }
                            } else if let Some(inputs) = any_inputs {
                                finish_any(&task, &inputs);
                            }
                            if is_done(target) {
                                stop_ref.store(true, Ordering::Release);
                                self.wake_cv.notify_all();
                                break;
                            }
                            continue;
                        }
                        if worker_id == 0 {
                            let (progressed, _) = tick_leaves(true);
                            if progressed {
                                continue;
                            }
                        }
                        let guard = self.wake_mu.lock().unwrap_or_else(|p| p.into_inner());
                        if !stop_ref.load(Ordering::Acquire) && !is_done(target) {
                            let _ = self.wake_cv.wait_timeout(guard, Duration::from_millis(5));
                        }
                    }
                });
            }
        });

        if let Some(err) = first_err.into_inner().unwrap_or_else(|p| p.into_inner()) {
            return Err(err);
        }
        result(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn work_stealing_transfers_tasks_between_workers() {
        let sched = WorkStealingScheduler::new(2);
        for _ in 0..4 {
            let t = new_task(Some("p".into()), Leaf::None);
            sched.push_local(0, t);
        }
        let mut stolen_count = 0;
        while let Some((_task, stolen)) = sched.pop_or_steal(1) {
            if stolen {
                stolen_count += 1;
            }
        }
        assert_eq!(stolen_count, 4);
        assert_eq!(sched.steal_count(), 4);
    }

    #[test]
    fn work_stealing_scheduler_completes_target_across_threads() {
        let sched = WorkStealingScheduler::new(4);
        let target = new_task(Some("finish".into()), Leaf::None);
        let seen_threads = Arc::new(Mutex::new(HashSet::new()));
        let remaining = Arc::new(AtomicUsize::new(16));

        for i in 0..16 {
            let t = new_task(Some(format!("work_{i}")), Leaf::None);
            sched.push_local(0, t);
        }

        let target_clone = Arc::clone(&target);
        let seen_clone = Arc::clone(&seen_threads);
        let rem_clone = Arc::clone(&remaining);
        let sched_clone = Arc::clone(&sched);

        let val = sched
            .run_until_done(&target, move || {
                let target_inner = Arc::clone(&target_clone);
                let seen_inner = Arc::clone(&seen_clone);
                let rem_inner = Arc::clone(&rem_clone);
                let sched_inner = Arc::clone(&sched_clone);
                move |task: &TaskRef, name: &str| {
                    seen_inner
                        .lock()
                        .unwrap()
                        .insert(std::thread::current().id());
                    if name == "finish" {
                        complete(task, IrValue::I64(42));
                    } else {
                        std::thread::sleep(Duration::from_millis(2));
                        complete(task, IrValue::I64(1));
                        if rem_inner.fetch_sub(1, Ordering::AcqRel) == 1 {
                            sched_inner.spawn_global(Arc::clone(&target_inner));
                        }
                    }
                    Ok(())
                }
            })
            .expect("scheduler run_until_done");

        assert_eq!(val, IrValue::I64(42));
        assert!(
            sched.steal_count() > 0,
            "expected sibling workers to steal tasks"
        );
        assert!(
            seen_threads.lock().unwrap().len() >= 2,
            "expected at least 2 worker threads to execute tasks"
        );
    }

    #[test]
    fn async_writable_completes_for_connected_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let stream = TcpStream::connect(addr).expect("connect");

        #[cfg(windows)]
        let raw = {
            use std::os::windows::io::AsRawSocket;
            stream.as_raw_socket() as i64
        };
        #[cfg(unix)]
        let raw = {
            use std::os::unix::io::AsRawFd;
            stream.as_raw_fd() as i64
        };

        let task = writable(raw);
        let res = block_on(&task, |_, _| Ok(())).expect("block_on writable");
        assert_eq!(res, IrValue::I64(0));
    }

    #[test]
    fn spawn_io_completes_nonblocking_task() {
        let task = spawn_io(|| {
            std::thread::sleep(Duration::from_millis(5));
            IrValue::Str("async-io-ok".into())
        });
        let res = block_on(&task, |_, _| Ok(())).expect("block_on spawn_io");
        assert_eq!(res, IrValue::Str("async-io-ok".into()));
    }
}
