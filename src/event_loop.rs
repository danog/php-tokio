// Copyright 2023-2024 Daniil Gentili
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::notify::{self, Notifier, Receiver};
use ext_php_rs::{
    boxed::ZBox,
    call_user_func,
    exception::{throw_object, PhpException},
    prelude::*,
    types::{ZendObject, Zval},
    zend::{ClassEntry, ExecutorGlobals, Function},
};
use lazy_static::lazy_static;
use std::{
    cell::RefCell,
    collections::HashMap,
    future::Future,
    io,
    sync::{mpsc, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use tokio::runtime::{Handle, Runtime};

lazy_static! {
    /// The runtime of the event loops initialized with `EventLoop::init()`, shared by every
    /// request of the process.
    pub static ref RUNTIME: Runtime = Runtime::new().expect("Could not allocate runtime");
}

thread_local! {
    static EVENTLOOP: RefCell<Option<EventLoop>> = const { RefCell::new(None) };
}

/// The result of a future, handed over from a Tokio thread to the PHP thread.
struct Slot<T> {
    value: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T> Slot<T> {
    fn set(&self, value: T) {
        *self.value.lock().unwrap() = Some(value);
        self.ready.notify_all();
    }

    /// Blocks until the future completes.
    fn wait(&self) -> T {
        let mut value = self.value.lock().unwrap();
        loop {
            if let Some(value) = value.take() {
                return value;
            }
            value = self.ready.wait(value).unwrap();
        }
    }
}

/// The futures of `suspend_on()` and `try_suspend_on()` running on a runtime that an event loop
/// owns (`EventLoop::init_with()`), and that runtime once the event loop is shut down: it is
/// released only when none of them runs any more.
///
/// Dropping it earlier would cancel them, and a caller of `suspend_on()` interrupted by an
/// exception waits for its future, which may borrow from the caller's stack, to complete.
#[derive(Default)]
struct Tasks {
    state: Mutex<TasksState>,
}

#[derive(Default)]
struct TasksState {
    running: usize,
    /// The runtime to release once `running` is 0.
    release: Option<Runtime>,
}

/// Held by a running future.
struct Task(Arc<Tasks>);

impl Tasks {
    fn start(self: &Arc<Self>) -> Task {
        self.state.lock().unwrap().running += 1;
        Task(self.clone())
    }

    /// Releases the runtime on the PHP thread if none of the futures runs, else lets the last
    /// one release it.
    fn release(&self, runtime: Runtime) {
        let mut state = self.state.lock().unwrap();
        if state.running == 0 {
            drop(state);
            drop(runtime);
        } else {
            state.release = Some(runtime);
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        let runtime = {
            let mut state = self.0.state.lock().unwrap();
            state.running -= 1;
            if state.running == 0 {
                state.release.take()
            } else {
                None
            }
        };
        // On one of the runtime's own threads: it can't wait for them to stop.
        if let Some(runtime) = runtime {
            runtime.shutdown_background();
        }
    }
}

/// Tokio's threads don't survive a fork: only the process that initialized an event loop can use
/// it.
#[cfg(unix)]
const PENDING_ACROSS_FORK: &str =
    "php-tokio: this call was pending when the process forked, and can only complete in the parent";

#[cfg(unix)]
const FORKED: &str = "php-tokio cannot be used after fork: Tokio's threads are not inherited";

pub struct EventLoop {
    /// The process that initialized the event loop.
    #[cfg(unix)]
    pid: u32,

    /// The suspensions of the callers waiting for a future, by ID. PHP values never leave the
    /// PHP thread: only the IDs of the completed futures cross threads.
    suspensions: HashMap<u64, Zval>,
    next: u64,
    /// The IDs of the completed futures.
    completions: mpsc::Sender<u64>,
    completed: mpsc::Receiver<u64>,

    /// Wakes PHP up when futures complete: shared by every future.
    notifier: Arc<Notifier>,
    receiver: Receiver,

    /// Where the futures run.
    handle: Handle,
    /// The runtime from `init_with()`, released by `shutdown()`.
    runtime: Option<Runtime>,
    tasks: Arc<Tasks>,

    get_current_suspension: Function,
}

impl EventLoop {
    /// Initializes the event loop, returning the stream to watch for readability, calling
    /// `wakeup()` when it is readable:
    ///
    /// ```php
    /// $stream = Extension::init();
    /// Revolt\EventLoop::onReadable($stream, fn () => Extension::wakeup());
    /// ```
    ///
    /// The stream is the same every time: don't close it before `shutdown()` (on Windows, closing
    /// a socket stream shuts the socket down). The futures run on `RUNTIME`, shared by every
    /// request.
    pub fn init() -> PhpResult<Zval> {
        Self::initialize(|| Ok(None))
    }

    /// Initializes the event loop like `init()`, with the futures running on the runtime that
    /// `runtime` builds, for example to choose its threads:
    ///
    /// ```no_run
    /// // In a PHP method.
    /// php_tokio::EventLoop::init_with(|| {
    ///     tokio::runtime::Builder::new_multi_thread()
    ///         .worker_threads(1)
    ///         .enable_all()
    ///         .build()
    /// })
    /// # ;
    /// ```
    ///
    /// `runtime` is only called if the event loop isn't initialized yet. The event loop owns the
    /// runtime: `shutdown()` releases it, once the futures of `suspend_on()` and
    /// `try_suspend_on()` running on it completed.
    pub fn init_with(runtime: impl FnOnce() -> io::Result<Runtime>) -> PhpResult<Zval> {
        Self::initialize(|| runtime().map(Some))
    }

    fn initialize(runtime: impl FnOnce() -> io::Result<Option<Runtime>>) -> PhpResult<Zval> {
        EVENTLOOP.with_borrow_mut(|e| {
            let event_loop = match e {
                None => {
                    let runtime = runtime()
                        .map_err(|err| format!("Could not build the Tokio runtime: {}", err))?;
                    e.insert(Self::new(runtime)?)
                }
                Some(event_loop) => event_loop,
            };
            event_loop.check_owned()?;
            event_loop.receiver.php_stream()
        })
    }

    /// Suspends the current fiber until `future` completes, returning its result.
    ///
    /// If the fiber is interrupted by an exception (an event-loop error thrown into `{main}`, or
    /// the fiber being destroyed), this waits for the future anyway, blocking the PHP thread, and
    /// returns its result with the exception pending: the future may borrow from the caller's
    /// stack (see `php_async_impl`).
    ///
    /// Panics if the event loop is not initialized.
    pub fn suspend_on<T: Send + 'static, F: Future<Output = T> + Send + 'static>(future: F) -> T {
        // What's going on here? Unsafe borrows???
        // NO: this is actually 100% safe, and here's why.
        //
        // Rust thinks we're Sending the Future to another thread (tokio's event loop),
        // where it may be used even after its lifetime expires in the main (PHP) thread.
        //
        // In reality, the Future is only used by Tokio until the result is ready.
        //
        // Rust does not understand that when we suspend the current fiber in suspend_on,
        // we basically keep alive the the entire stack,
        // including the Rust stack and the Future on it, until the result of the future is ready.
        //
        // Once the result of the Future is ready, tokio doesn't need it anymore,
        // the suspend_on function is resumed, and we safely drop the Future upon exiting.
        //
        // This only holds if suspend_on never returns (or unwinds) before the future has
        // completed: nothing below may panic once the future is spawned, and if the caller
        // is not resumed by wakeup(), we wait for the future before returning.
        let (id, suspension, slot) = Self::spawn(future).unwrap_or_else(|err| panic!("{}", err));
        #[cfg(unix)]
        let pid = std::process::id();
        let interrupted = match Self::suspend(id, &suspension) {
            Suspended::Resumed | Suspended::Failed(_) => false,
            // Pending for the caller.
            Suspended::Exited => true,
            Suspended::Threw(exception) => {
                rethrow(exception);
                true
            }
        };
        // In a process forked meanwhile (whose copy of the fiber PHP destroys), the future never
        // completes: it runs on Tokio's threads, in the parent. Nothing here uses what it borrows,
        // so unwind rather than wait forever: ext-php-rs throws nothing over the pending exception.
        #[cfg(unix)]
        if interrupted && pid != std::process::id() {
            panic!("{}", PENDING_ACROSS_FORK);
        }
        #[cfg(not(unix))]
        let _ = interrupted;
        // Resumed by wakeup() once the future completed, this doesn't block; interrupted, this
        // waits for the future.
        slot.wait()
    }

    /// Like `suspend_on()`, but fails instead of panicking, and never waits for the future if
    /// the fiber is interrupted: the future completes on its own. It must therefore own all its
    /// data: never pass it anything borrowed with `borrow_unchecked`.
    ///
    /// Interrupted by an exception, returns it, unchanged. Interrupted by an exit (`exit()`, or
    /// PHP destroying the fiber, at the end of the request or once nothing references it),
    /// returns `None`: the exit stays pending, as PHP must keep unwinding the fiber, so nothing
    /// may be thrown. Return any value then: PHP discards it.
    pub fn try_suspend_on<T: Send + 'static, F: Future<Output = T> + Send + 'static>(
        future: F,
    ) -> PhpResult<Option<T>> {
        let (id, suspension, slot) = Self::spawn(future)?;
        match Self::suspend(id, &suspension) {
            // Resumed by wakeup() or drain() once the future completed: this doesn't block.
            Suspended::Resumed => Ok(Some(slot.wait())),
            Suspended::Failed(err) => Err(format!("Could not suspend the caller: {}", err).into()),
            Suspended::Exited => Ok(None),
            Suspended::Threw(exception) => {
                Err(PhpException::from("Interrupted by an exception").with_object(zval(exception)))
            }
        }
    }

    /// Spawns `future`, registering the caller's suspension.
    fn spawn<T: Send + 'static, F: Future<Output = T> + Send + 'static>(
        future: F,
    ) -> Result<(u64, Zval, Arc<Slot<T>>), String> {
        EVENTLOOP.with_borrow_mut(|c| {
            let c = c
                .as_mut()
                .ok_or("php-tokio's EventLoop::init() must be called first")?;
            c.check_owned()?;
            // An exception thrown by getSuspension() stays pending, as the previous one.
            let suspension = call_user_func!(c.get_current_suspension)
                .ok()
                .filter(|_| !ExecutorGlobals::has_exception())
                .ok_or("Could not get the current suspension")?;
            c.next += 1;
            let id = c.next;
            c.suspensions.insert(id, suspension.shallow_clone());

            let slot = Arc::new(Slot {
                value: Mutex::new(None),
                ready: Condvar::new(),
            });
            let completions = c.completions.clone();
            let notifier = c.notifier.clone();
            let result = slot.clone();
            let task = c.tasks.start();
            c.handle.spawn(async move {
                let _task = task;
                result.set(future.await);
                // Only the ID crosses threads, never a PHP value.
                let _ = completions.send(id);
                notifier.notify();
            });
            Ok((id, suspension, slot))
        })
    }

    /// Suspends the caller until wakeup() or drain() resumes it. Interrupted, unregisters it.
    fn suspend(id: u64, suspension: &Zval) -> Suspended {
        // try_call_method returns Ok even when suspend() throws: the exception is left pending.
        let suspended = suspension.try_call_method("suspend", vec![]);
        let result = if exit_pending() {
            Suspended::Exited
        } else if let Some(exception) = ExecutorGlobals::take_exception() {
            Suspended::Threw(exception)
        } else {
            match suspended {
                Ok(_) => return Suspended::Resumed,
                Err(err) => Suspended::Failed(err),
            }
        };
        EVENTLOOP.with_borrow_mut(|c| {
            if let Some(c) = c.as_mut() {
                c.suspensions.remove(&id);
            }
        });
        result
    }

    /// Resumes the callers whose future completed.
    ///
    /// In a process forked from the one that initialized the event loop, does nothing: the
    /// notification socket is shared with the parent, whose notifications must stay there.
    pub fn wakeup() -> PhpResult<()> {
        let ready = EVENTLOOP.with_borrow_mut(|c| -> PhpResult<Vec<Zval>> {
            let c = c
                .as_mut()
                .ok_or("php-tokio's EventLoop::init() must be called first")?;
            if c.check_owned().is_err() {
                return Ok(vec![]);
            }
            c.receiver.drain();
            c.notifier.rearm();
            Ok(c.completed
                .try_iter()
                .filter_map(|id| c.suspensions.remove(&id))
                .collect())
        })?;
        resume(ready)
    }

    /// Waits up to `timeout` for the futures of the suspended callers to complete, and resumes
    /// the callers whose future completed, like `wakeup()`. This doesn't run the PHP event loop:
    /// the resumed callers continue once it runs.
    ///
    /// Use it before `shutdown()` when the pending futures are about to complete (their work was
    /// finished or rejected), so that their callers are resumed.
    pub fn drain(timeout: Duration) -> PhpResult<()> {
        let deadline = Instant::now() + timeout;
        let ready = EVENTLOOP.with_borrow_mut(|c| {
            let Some(c) = c.as_mut().filter(|c| c.check_owned().is_ok()) else {
                return vec![];
            };
            let mut ready = vec![];
            while !c.suspensions.is_empty() {
                match c
                    .completed
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(id) => ready.extend(c.suspensions.remove(&id)),
                    Err(_) => break,
                }
            }
            // As wakeup() does: the completions published meanwhile are collected next.
            c.receiver.drain();
            c.notifier.rearm();
            ready.extend(
                c.completed
                    .try_iter()
                    .filter_map(|id| c.suspensions.remove(&id)),
            );
            ready
        });
        resume(ready)
    }

    /// The number of suspended callers.
    pub fn pending() -> usize {
        EVENTLOOP.with_borrow(|c| {
            c.as_ref()
                .filter(|c| c.check_owned().is_ok())
                .map_or(0, |c| c.suspensions.len())
        })
    }

    /// Releases the event loop. Callers still suspended are never resumed: their suspensions are
    /// released here, on the PHP thread. A runtime from `init_with()` is released once the
    /// futures running on it completed.
    pub fn shutdown() {
        let event_loop = EVENTLOOP.with_borrow_mut(Option::take);
        if let Some(mut event_loop) = event_loop {
            #[cfg(unix)]
            if event_loop.check_owned().is_err() {
                // Inherited across fork: the parent owns the socket and the runtime.
                std::mem::forget(event_loop);
                return;
            }
            if let Some(runtime) = event_loop.runtime.take() {
                event_loop.tasks.release(runtime);
            }
        }
    }

    fn new(runtime: Option<Runtime>) -> PhpResult<Self> {
        let (completions, completed) = mpsc::channel();
        let (notifier, receiver) = notify::channel()
            .map_err(|err| format!("Could not create the notification socket: {}", err))?;

        // Autoloaded if needed.
        for name in ["Revolt\\EventLoop", "Revolt\\EventLoop\\Suspension"] {
            if ClassEntry::try_find(name).is_none() {
                return Err(format!("\\{} does not exist", name).into());
            }
        }

        Ok(Self {
            #[cfg(unix)]
            pid: std::process::id(),
            suspensions: HashMap::new(),
            next: 0,
            completions,
            completed,
            notifier: Arc::new(notifier),
            receiver,
            handle: runtime
                .as_ref()
                .map_or_else(|| RUNTIME.handle().clone(), |r| r.handle().clone()),
            runtime,
            tasks: Arc::default(),
            get_current_suspension: Function::try_from_method(
                "\\Revolt\\EventLoop",
                "getSuspension",
            )
            .ok_or("\\Revolt\\EventLoop::getSuspension does not exist")?,
        })
    }
}

impl EventLoop {
    /// Fails in a process forked from the one that initialized the event loop.
    #[cfg(unix)]
    fn check_owned(&self) -> Result<(), &'static str> {
        if self.pid == std::process::id() {
            Ok(())
        } else {
            Err(FORKED)
        }
    }

    /// Windows has no fork.
    #[cfg(not(unix))]
    fn check_owned(&self) -> Result<(), &'static str> {
        Ok(())
    }
}

/// How a caller's suspension ended.
enum Suspended {
    /// By wakeup() or drain(), once its future completed.
    Resumed,
    /// By an exit, left pending (see exit_pending()).
    Exited,
    /// By an exception, taken.
    Threw(ZBox<ZendObject>),
    /// Without suspending.
    Failed(ext_php_rs::error::Error),
}

/// `object` as a zval, holding its reference.
fn zval(mut object: ZBox<ZendObject>) -> Zval {
    let mut zval = Zval::new();
    // Adds a reference: dropping the box releases its own.
    zval.set_object(&mut object);
    zval
}

/// Throws an exception taken with take_exception() again.
fn rethrow(exception: ZBox<ZendObject>) {
    // Fails only for a zval that isn't an object.
    let _ = throw_object(zval(exception));
}

extern "C" {
    fn zend_is_unwind_exit(ex: *const ext_php_rs::ffi::zend_object) -> bool;
    fn zend_is_graceful_exit(ex: *const ext_php_rs::ffi::zend_object) -> bool;
}

/// Whether the pending exception is an exit: `exit()` (unwind exit), or PHP destroying a fiber
/// (graceful exit). Neither is a Throwable, and PHP must keep unwinding: it can't be taken and
/// thrown again, nor replaced by another exception.
fn exit_pending() -> bool {
    let exception = ExecutorGlobals::get().exception;
    !exception.is_null()
        && unsafe { zend_is_unwind_exit(exception) || zend_is_graceful_exit(exception) }
}

/// Resumes the callers, outside the event loop's borrow: resuming can re-enter php-tokio.
fn resume(ready: Vec<Zval>) -> PhpResult<()> {
    let mut first = None;
    for suspension in ready {
        let _ = suspension.try_call_method("resume", vec![]);
        // An exception thrown by resume() is left pending, and PHP calls nothing while one is:
        // take it to resume the others, and rethrow the first one.
        if let Some(exception) = ExecutorGlobals::take_exception() {
            first.get_or_insert(exception);
        }
    }
    if let Some(exception) = first {
        rethrow(exception);
    }
    Ok(())
}
