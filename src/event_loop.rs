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

use ext_php_rs::{
    call_user_func,
    convert::IntoZval,
    exception::throw_object,
    prelude::*,
    types::Zval,
    zend::{ExecutorGlobals, Function},
};
use lazy_static::lazy_static;
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::File,
    future::Future,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
};
use tokio::runtime::Runtime;

lazy_static! {
    pub static ref RUNTIME: Runtime = Runtime::new().expect("Could not allocate runtime");
}

thread_local! {
    static EVENTLOOP: RefCell<Option<EventLoop>> = const { RefCell::new(None) };
}

#[cfg(any(target_os = "linux", target_os = "solaris"))]
fn sys_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut pipefd = [0; 2];
    let ret = unsafe { libc::pipe2(pipefd.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((pipefd[0], pipefd[1]))
}

#[cfg(any(target_os = "macos"))]
fn set_cloexec(fd: RawFd) -> io::Result<()> {
    use libc::fcntl;
    use libc::{FD_CLOEXEC, F_GETFD, F_SETFD};

    let flags = unsafe { fcntl(fd, F_GETFD, 0) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let ret = unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "macos"))]
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    use libc::{fcntl, F_SETFL, O_NONBLOCK};

    let ret = unsafe { fcntl(fd, F_SETFL, O_NONBLOCK) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(any(target_os = "macos"))]
fn sys_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut pipefd = [0; 2];
    let ret = unsafe { libc::pipe(pipefd.as_mut_ptr()) };

    if ret == -1 {
        return Err(io::Error::last_os_error());
    }

    for fd in &pipefd {
        set_cloexec(*fd)?;
        set_nonblocking(*fd)?;
    }
    Ok((pipefd[0], pipefd[1]))
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

pub struct EventLoop {
    /// The process that initialized the event loop: Tokio's threads don't survive a fork.
    pid: u32,

    /// The suspensions of the callers waiting for a future, by ID. PHP values never leave the
    /// PHP thread: only the IDs of the completed futures cross threads.
    suspensions: HashMap<u64, Zval>,
    next: u64,
    sender: Sender<u64>,
    receiver: Receiver<u64>,

    /// One write end shared by every future, instead of a descriptor duplicated for each.
    notify_sender: Arc<File>,
    notify_receiver: File,

    get_current_suspension: Function,
}

impl EventLoop {
    pub fn init() -> PhpResult<u64> {
        EVENTLOOP.with_borrow_mut(|e| {
            let event_loop = match e {
                None => e.insert(Self::new()?),
                Some(event_loop) => event_loop,
            };
            if event_loop.pid != std::process::id() {
                return Err(
                    "php-tokio cannot be used after fork: Tokio's threads are not inherited".into(),
                );
            }
            Ok(event_loop.notify_receiver.as_raw_fd() as u64)
        })
    }

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
        let slot = Arc::new(Slot {
            value: Mutex::new(None),
            ready: Condvar::new(),
        });
        let (id, suspension) = EVENTLOOP.with_borrow_mut(|c| {
            let c = c
                .as_mut()
                .expect("php-tokio's EventLoop::init() must be called first");
            assert_eq!(
                c.pid,
                std::process::id(),
                "php-tokio cannot be used after fork: Tokio's threads are not inherited"
            );
            let suspension = call_user_func!(c.get_current_suspension)
                .expect("Could not get the current suspension");
            c.next += 1;
            let id = c.next;
            c.suspensions.insert(id, suspension.shallow_clone());

            let sender = c.sender.clone();
            let notifier = c.notify_sender.clone();
            let result = slot.clone();
            RUNTIME.spawn(async move {
                result.set(future.await);
                // Only the ID crosses threads, never a PHP value.
                let _ = sender.send(id);
                let mut notifier = &*notifier;
                loop {
                    match notifier.write(&[0]) {
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        // A full pipe (EAGAIN) already has a wakeup pending.
                        _ => break,
                    }
                }
            });
            (id, suspension)
        });

        // We suspend the fiber here, the Rust stack is kept alive until the result is ready.
        //
        // try_call_method returns Ok even when suspend() throws (an event-loop error thrown into
        // {main}, or the fiber being destroyed): the exception is left pending, so check for it.
        // The caller then continues without being resumed: unregister it, and wait for the
        // future, which may borrow from this stack; the pending exception then propagates.
        let suspended = suspension.try_call_method("suspend", vec![]);
        if suspended.is_err() || ExecutorGlobals::has_exception() {
            EVENTLOOP.with_borrow_mut(|c| {
                if let Some(c) = c.as_mut() {
                    c.suspensions.remove(&id);
                }
            });
        }

        // Resumed by wakeup() once the future completed: this doesn't block.
        slot.wait()
    }

    /// Resumes the callers whose future completed.
    pub fn wakeup() -> PhpResult<()> {
        let ready = EVENTLOOP.with_borrow_mut(|c| -> PhpResult<Vec<Zval>> {
            let c = c
                .as_mut()
                .ok_or("php-tokio's EventLoop::init() must be called first")?;
            let mut buffer = [0; 4096];
            while c.notify_receiver.read(&mut buffer).is_ok_and(|n| n != 0) {}
            Ok(c.receiver
                .try_iter()
                .filter_map(|id| c.suspensions.remove(&id))
                .collect())
        })?;
        // Resume outside the borrow: resuming can re-enter php-tokio.
        let mut first = None;
        for suspension in ready {
            let _ = suspension.try_call_method("resume", vec![]);
            // An exception thrown by resume() is left pending, and PHP calls nothing while one
            // is: take it to resume the others, and rethrow the first one.
            if let Some(exception) = ExecutorGlobals::take_exception() {
                first.get_or_insert(exception);
            }
        }
        if let Some(exception) = first {
            throw_object(exception.into_zval(false)?)?;
        }
        Ok(())
    }

    /// Releases the event loop. Callers still suspended are never resumed: their suspensions are
    /// released here, on the PHP thread.
    pub fn shutdown() {
        let event_loop = EVENTLOOP.with_borrow_mut(Option::take);
        if let Some(event_loop) = event_loop {
            if event_loop.pid != std::process::id() {
                // Inherited across fork: the parent owns the pipe and the runtime.
                std::mem::forget(event_loop);
            }
        }
    }

    pub fn new() -> PhpResult<Self> {
        let (sender, receiver) = channel();
        let (notify_receiver, notify_sender) =
            sys_pipe().map_err(|err| format!("Could not create pipe: {}", err))?;

        if !call_user_func!(
            Function::try_from_function("class_exists").unwrap(),
            "\\Revolt\\EventLoop"
        )?
        .bool()
        .unwrap_or(false)
        {
            return Err("\\Revolt\\EventLoop does not exist!".into());
        }
        if !call_user_func!(
            Function::try_from_function("interface_exists").unwrap(),
            "\\Revolt\\EventLoop\\Suspension"
        )?
        .bool()
        .unwrap_or(false)
        {
            return Err("\\Revolt\\EventLoop\\Suspension does not exist!".into());
        }

        Ok(Self {
            pid: std::process::id(),
            suspensions: HashMap::new(),
            next: 0,
            sender,
            receiver,
            notify_sender: Arc::new(unsafe { File::from_raw_fd(notify_sender) }),
            notify_receiver: unsafe { File::from_raw_fd(notify_receiver) },
            get_current_suspension: Function::try_from_method(
                "\\Revolt\\EventLoop",
                "getSuspension",
            )
            .ok_or("\\Revolt\\EventLoop::getSuspension does not exist")?,
        })
    }
}
