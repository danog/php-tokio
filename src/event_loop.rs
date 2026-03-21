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
use ext_php_rs::types::Zval;
use ext_php_rs::{call_user_func, prelude::*, zend::Function};
use lazy_static::lazy_static;
use std::{
    cell::RefCell,
    future::Future,
    io,
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio::runtime::Runtime;

lazy_static! {
    pub static ref RUNTIME: Runtime = Runtime::new().expect("Could not allocate runtime");
}

thread_local! {
    static EVENTLOOP: RefCell<Option<EventLoop>> = RefCell::new(None);
}

/// Number of bytes written/read per notification event.
const NOTIFY_BYTE_LEN: usize = 1;

/// Create a non-blocking pipe and return `(read_fd, write_fd)`.
///
/// On Linux/Solaris we use `pipe2` for a one-syscall, race-free setup.
/// On macOS we fall back to `pipe` + `fcntl`.
/// On Windows we create a loopback TCP socket pair and convert the
/// SOCKET handles to CRT file descriptors with `_open_osfhandle` so that
/// the same `libc::read` / `libc::write` / `libc::close` calls work on
/// every platform.
#[cfg(any(target_os = "linux", target_os = "solaris"))]
fn sys_pipe() -> io::Result<(i32, i32)> {
    let mut pipefd = [0i32; 2];
    let ret = unsafe { libc::pipe2(pipefd.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((pipefd[0], pipefd[1]))
}

#[cfg(target_os = "macos")]
fn sys_pipe() -> io::Result<(i32, i32)> {
    use libc::{fcntl, FD_CLOEXEC, F_GETFD, F_SETFD, F_SETFL, O_NONBLOCK};

    let mut pipefd = [0i32; 2];
    let ret = unsafe { libc::pipe(pipefd.as_mut_ptr()) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }

    for &fd in &pipefd {
        let flags = unsafe { fcntl(fd, F_GETFD, 0) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        let ret = unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) };
        if ret == -1 {
            return Err(io::Error::last_os_error());
        }
        let ret = unsafe { fcntl(fd, F_SETFL, O_NONBLOCK) };
        if ret == -1 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok((pipefd[0], pipefd[1]))
}

/// Windows: create a connected TCP loopback socket pair and expose both
/// ends as CRT file descriptors so that `libc::read` / `libc::write` /
/// `libc::close` can be used uniformly across all platforms.
#[cfg(windows)]
fn sys_pipe() -> io::Result<(i32, i32)> {
    use std::net::{TcpListener, TcpStream};
    use std::os::windows::io::IntoRawSocket;

    // Bind to an ephemeral loopback port.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;

    // Connect the write end before accepting so the handshake completes
    // before we block on `accept`.
    let writer = TcpStream::connect(addr)?;
    let (reader, _peer_addr) = listener.accept()?;

    reader.set_nonblocking(true)?;
    writer.set_nonblocking(true)?;

    // Consume the Rust wrappers without closing the underlying OS handles.
    let reader_socket = reader.into_raw_socket(); // SOCKET (uintptr_t)
    let writer_socket = writer.into_raw_socket();

    // Wrap each SOCKET in a CRT file descriptor.  Windows CRT routes
    // _read/_write through ReadFile/WriteFile on the underlying HANDLE,
    // which works correctly for connected TCP sockets.
    let reader_fd = unsafe {
        libc::_open_osfhandle(reader_socket as libc::intptr_t, libc::O_RDONLY | libc::O_BINARY)
    };
    let writer_fd = unsafe {
        libc::_open_osfhandle(writer_socket as libc::intptr_t, libc::O_WRONLY | libc::O_BINARY)
    };

    if reader_fd == -1 || writer_fd == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok((reader_fd, writer_fd))
}

pub struct EventLoop {
    sender: Sender<Suspension>,
    receiver: Receiver<Suspension>,

    /// Raw OS / CRT file descriptor for the *write* end of the
    /// notification channel.  An `i32` is `Copy`, so it can be
    /// captured by value in async tasks without requiring `dup`.
    notify_sender: i32,

    /// Raw OS / CRT file descriptor for the *read* end.
    /// Returned to PHP so that Revolt can poll it for readability.
    notify_receiver: i32,

    get_current_suspension: Function,

    dummy: [u8; 1],
}

/// # Safety
///
/// `Suspension` contains a PHP `Zval`, but it is never accessed from Tokio threads.
/// The value is only moved across threads and sent back through a channel.
/// All interaction with the inner `Zval` (e.g. calling `resume()`) happens
/// exclusively on the original PHP thread in `EventLoop::wakeup()`.
///
/// Therefore, no PHP memory is accessed off-thread, making `Send` and `Sync` sound
/// under this invariant.
struct Suspension(Zval);
unsafe impl Send for Suspension {}
unsafe impl Sync for Suspension {}

impl Drop for EventLoop {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.notify_sender);
            libc::close(self.notify_receiver);
        }
    }
}

impl EventLoop {
    pub fn init() -> PhpResult<u64> {
        EVENTLOOP.with_borrow_mut(|e| {
            Ok(match e {
                None => e.insert(Self::new()?),
                Some(ev) => ev,
            }
            .notify_receiver as u64)
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
            let suspension = Suspension(call_user_func!(c.get_current_suspension).unwrap());

            let sender = c.sender.clone();
            // `i32` is `Copy` — no need to dup the fd.
            // Single-byte writes to a pipe / connected socket are atomic,
            // so concurrent writes from multiple tasks are safe.
            let notify_fd = c.notify_sender;

            (
                RUNTIME.spawn(async move {
                    let res = future.await;
                    sender.send(suspension).unwrap();
                    // Notify the PHP event loop that a suspension is ready.
                    unsafe {
                        libc::write(
                            notify_fd,
                            b"\x00".as_ptr() as *const libc::c_void,
                            NOTIFY_BYTE_LEN as _,
                        )
                    };
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

            // Drain all pending notification bytes.  A non-blocking read
            // returns ≤ 0 when no more data is available (EAGAIN / EOF).
            loop {
                let n = unsafe {
                    libc::read(
                        c.notify_receiver,
                        c.dummy.as_mut_ptr() as *mut libc::c_void,
                        NOTIFY_BYTE_LEN as _,
                    )
                };
                if n <= 0 {
                    break;
                }
            }

            for mut suspension in c.receiver.try_iter() {
                suspension
                    .0
                    .object_mut()
                    .unwrap()
                    .try_call_method("resume", vec![])?;
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
            sender,
            receiver,
            notify_sender,
            notify_receiver,
            dummy: [0; 1],
            get_current_suspension: Function::try_from_method(
                "\\Revolt\\EventLoop",
                "getSuspension",
            )
            .ok_or("\\Revolt\\EventLoop::getSuspension does not exist")?,
        })
    }
}
