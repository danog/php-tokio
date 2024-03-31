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

use crate::borrow_unchecked::borrow_unchecked;
use ext_php_rs::{boxed::ZBox, call_user_func, prelude::*, types::ZendHashTable, zend::Function};
use lazy_static::lazy_static;
use std::{
    cell::RefCell,
    fs::File,
    future::Future,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio::runtime::Runtime;

lazy_static! {
    pub static ref RUNTIME: Runtime = Runtime::new().expect("Could not allocate runtime");
}

thread_local! {
    static EVENTLOOP: RefCell<Option<EventLoop>> = RefCell::new(None);
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
    use libc::{F_SETFD, FD_CLOEXEC, F_GETFD};
    use libc::fcntl;

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

pub struct EventLoop {
    fibers: ZBox<ZendHashTable>,

    sender: Sender<u64>,
    receiver: Receiver<u64>,

    notify_sender: File,
    notify_receiver: File,

    get_current_suspension: Function,

    dummy: [u8; 1],
}

impl EventLoop {
    pub fn init() -> PhpResult<u64> {
        EVENTLOOP.with_borrow_mut(|e| {
            Ok(match e {
                None => e.insert(Self::new()?),
                Some(ev) => ev,
            }
            .notify_receiver
            .as_raw_fd() as u64)
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
        let (future, get_current_suspension) = EVENTLOOP.with_borrow_mut(move |c| {
            let c = c.as_mut().unwrap();
            let idx = c.fibers.len() as u64;
            c.fibers
                .insert_at_index(idx, call_user_func!(c.get_current_suspension).unwrap())
                .unwrap();

            let sender = c.sender.clone();
            let mut notifier = c.notify_sender.try_clone().unwrap();

            (
                RUNTIME.spawn(async move {
                    let res = future.await;
                    sender.send(idx).unwrap();
                    notifier.write_all(&[0]).unwrap();
                    res
                }),
                unsafe { borrow_unchecked(&c.get_current_suspension) },
            )
        });

        // We suspend the fiber here, the Rust stack is kept alive until the result is ready.
        call_user_func!(get_current_suspension)
            .unwrap()
            .try_call_method("suspend", vec![])
            .unwrap();

        // We've resumed, the `future` is already resolved and is not used by the tokio thread, it's safe to drop it.

        return RUNTIME.block_on(future).unwrap();
    }

    pub fn wakeup() -> PhpResult<()> {
        EVENTLOOP.with_borrow_mut(|c| {
            let c = c.as_mut().unwrap();

            c.notify_receiver.read_exact(&mut c.dummy).unwrap();

            for fiber_id in c.receiver.try_iter() {
                if let Some(fiber) = c.fibers.get_index_mut(fiber_id) {
                    fiber
                        .object_mut()
                        .unwrap()
                        .try_call_method("resume", vec![])?;
                    c.fibers.remove_index(fiber_id);
                }
            }
            Ok(())
        })
    }

    pub fn shutdown() {
        EVENTLOOP.set(None)
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
            return Err(format!("\\Revolt\\EventLoop does not exist!").into());
        }
        if !call_user_func!(
            Function::try_from_function("interface_exists").unwrap(),
            "\\Revolt\\EventLoop\\Suspension"
        )?
        .bool()
        .unwrap_or(false)
        {
            return Err(format!("\\Revolt\\EventLoop\\Suspension does not exist!").into());
        }

        Ok(Self {
            fibers: ZendHashTable::new(),
            sender: sender,
            receiver: receiver,
            notify_sender: unsafe { File::from_raw_fd(notify_sender) },
            notify_receiver: unsafe { File::from_raw_fd(notify_receiver) },
            dummy: [0; 1],
            get_current_suspension: Function::try_from_method(
                "\\Revolt\\EventLoop",
                "getSuspension",
            )
            .ok_or("\\Revolt\\EventLoop::getSuspension does not exist")?,
        })
    }
}
