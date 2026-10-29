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

//! Tokio threads tell PHP that futures completed through a connected pair of stream sockets:
//! PHP watches its end for readability.
//!
//! A socket, not a pipe: on Windows, PHP's select() emulation hands sockets alone to the real
//! select(), but polls any other descriptor (a pipe, or a socket opened as a file through
//! php://fd/) in 100 ms slices interleaved with the sockets, delaying every wakeup. PHP gets
//! its end as a socket stream of its own (`Receiver::php_stream`).

use ext_php_rs::{ffi, prelude::*, types::Zval};
use std::{
    io::{self, Read, Write},
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(unix)]
type Socket = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Socket = std::net::TcpStream;

/// Written from any thread.
pub(crate) struct Notifier {
    /// Set while a notification is in the socket: at most one or two bytes are ever in it, so a
    /// write never meets a full buffer.
    pending: AtomicBool,
    writer: Socket,
}

/// PHP's end, used on the PHP thread.
pub(crate) struct Receiver {
    /// Rust's own handle on PHP's end, to drain it.
    reader: Socket,
}

pub(crate) fn channel() -> io::Result<(Notifier, Receiver)> {
    let (writer, reader) = pair()?;
    writer.set_nonblocking(true)?;
    reader.set_nonblocking(true)?;
    Ok((
        Notifier {
            pending: AtomicBool::new(false),
            writer,
        },
        Receiver { reader },
    ))
}

#[cfg(unix)]
fn pair() -> io::Result<(Socket, Socket)> {
    Socket::pair()
}

/// Windows has no socketpair(): connect two TCP sockets on the loopback interface, as PHP's and
/// Python's socketpair() emulations do. Only the connection from our own socket is accepted.
#[cfg(windows)]
fn pair() -> io::Result<(Socket, Socket)> {
    use std::net::{Ipv4Addr, TcpListener};

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let writer = Socket::connect(listener.local_addr()?)?;
    let ours = writer.local_addr()?;
    let reader = loop {
        let (reader, peer) = listener.accept()?;
        if peer == ours {
            break reader;
        }
    };
    // One-byte writes: don't let Nagle's algorithm hold them back.
    writer.set_nodelay(true)?;
    Ok((writer, reader))
}

impl Notifier {
    /// Wakes PHP up: called by any thread, once the completion is published.
    pub(crate) fn notify(&self) {
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut writer = &self.writer;
        loop {
            match writer.write(&[0]) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
    }

    /// Re-arms the notification: called on the PHP thread after draining the receiver, and
    /// before collecting the completions.
    ///
    /// The socket is drained before the flag is cleared: until then nobody writes (the flag is
    /// set), and a completion published afterwards writes a byte that stays in the socket. A
    /// completion that skipped its write published its result before seeing the flag set, so it
    /// is collected right after. Clearing the flag first would let the drain consume a byte
    /// written meanwhile, leaving the flag set with an empty socket: no later completion would
    /// ever wake PHP up.
    pub(crate) fn rearm(&self) {
        self.pending.store(false, Ordering::SeqCst);
    }
}

impl Receiver {
    /// Consumes the notifications in PHP's end.
    pub(crate) fn drain(&self) {
        let mut buffer = [0; 64];
        let mut reader = &self.reader;
        loop {
            match reader.read(&mut buffer) {
                Ok(n) if n > 0 => continue,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // Empty (WouldBlock), or closed.
                _ => break,
            }
        }
    }

    /// The descriptor of PHP's end, for `php://fd/<fd>`.
    #[cfg(unix)]
    pub(crate) fn fd(&self) -> u64 {
        use std::os::fd::AsRawFd;
        self.reader.as_raw_fd() as u64
    }

    /// PHP's end as a PHP socket stream, for `EventLoop::onReadable`: a new descriptor for the
    /// same socket, owned by the stream.
    pub(crate) fn php_stream(&self) -> PhpResult<Zval> {
        let socket = into_php_socket(self.reader.try_clone().map_err(|e| e.to_string())?);
        let stream = unsafe {
            if ext_php_rs::PHP_DEBUG {
                let file = c"php-tokio".as_ptr();
                _php_stream_sock_open_from_socket_debug(socket, std::ptr::null(), file, 0, file, 0)
            } else {
                _php_stream_sock_open_from_socket(socket, std::ptr::null())
            }
        };
        if stream.is_null() {
            close_php_socket(socket);
            return Err("Could not open the notification socket as a PHP stream".into());
        }
        // php_stream_to_zval(): the zval takes over the stream's reference.
        let mut zval = Zval::new();
        unsafe {
            zval.set_resource((*stream).res);
            (*stream).set___exposed(1);
        }
        Ok(zval)
    }
}

#[cfg(unix)]
type PhpSocket = std::ffi::c_int;
#[cfg(windows)]
type PhpSocket = usize;

#[cfg(unix)]
fn into_php_socket(socket: Socket) -> PhpSocket {
    std::os::fd::IntoRawFd::into_raw_fd(socket)
}
#[cfg(windows)]
fn into_php_socket(socket: Socket) -> PhpSocket {
    std::os::windows::io::IntoRawSocket::into_raw_socket(socket) as PhpSocket
}

#[cfg(unix)]
fn close_php_socket(socket: PhpSocket) {
    drop(unsafe { <Socket as std::os::fd::FromRawFd>::from_raw_fd(socket) });
}
#[cfg(windows)]
fn close_php_socket(socket: PhpSocket) {
    drop(unsafe { <Socket as std::os::windows::io::FromRawSocket>::from_raw_socket(socket as _) });
}

// PHPAPI php_stream *_php_stream_sock_open_from_socket(php_socket_t socket,
//     const char *persistent_id STREAMS_DC), from main/php_network.h: STREAMS_DC adds the
// caller's file and line, twice, in debug builds of PHP.
#[allow(clashing_extern_declarations)]
extern "C" {
    fn _php_stream_sock_open_from_socket(
        socket: PhpSocket,
        persistent_id: *const std::ffi::c_char,
    ) -> *mut ffi::php_stream;

    #[link_name = "_php_stream_sock_open_from_socket"]
    fn _php_stream_sock_open_from_socket_debug(
        socket: PhpSocket,
        persistent_id: *const std::ffi::c_char,
        file: *const std::ffi::c_char,
        line: u32,
        orig_file: *const std::ffi::c_char,
        orig_line: u32,
    ) -> *mut ffi::php_stream;
}
