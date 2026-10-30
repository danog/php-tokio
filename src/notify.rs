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
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
use sys::{into_php_socket, pair, PhpSocket, Socket};
#[cfg(windows)]
use windows as sys;

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

impl Notifier {
    /// Wakes PHP up: called by any thread, once the completion is published.
    pub(crate) fn notify(&self) {
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        // Non-blocking, so never interrupted by a signal (EINTR), nor full (see `pending`): it
        // only fails once PHP's end is closed, and then nobody is waiting.
        let _ = (&self.writer).write(&[0]);
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
        // Until empty (WouldBlock): non-blocking, so never interrupted by a signal (EINTR).
        while matches!((&self.reader).read(&mut buffer), Ok(n) if n > 0) {}
    }

    /// PHP's end as a PHP socket stream, for `EventLoop::onReadable`: a new descriptor for the
    /// same socket, owned by the stream.
    pub(crate) fn php_stream(&self) -> PhpResult<Zval> {
        let socket = into_php_socket(self.reader.try_clone().map_err(|e| e.to_string())?);
        // Never null: PHP's allocator bails out instead of failing.
        let stream = unsafe { sock_open_from_socket(socket) };
        // php_stream_to_zval(): the zval takes over the stream's reference.
        let mut zval = Zval::new();
        unsafe {
            zval.set_resource((*stream).res);
            (*stream).set___exposed(1);
        }
        Ok(zval)
    }
}

// PHPAPI php_stream *_php_stream_sock_open_from_socket(php_socket_t socket,
//     const char *persistent_id STREAMS_DC), from main/php_network.h: STREAMS_DC adds the
// caller's file and line, twice, in debug builds of PHP (cfg php_debug, from build.rs).
extern "C" {
    #[cfg(not(php_debug))]
    fn _php_stream_sock_open_from_socket(
        socket: PhpSocket,
        persistent_id: *const std::ffi::c_char,
    ) -> *mut ffi::php_stream;

    #[cfg(php_debug)]
    fn _php_stream_sock_open_from_socket(
        socket: PhpSocket,
        persistent_id: *const std::ffi::c_char,
        file: *const std::ffi::c_char,
        line: u32,
        orig_file: *const std::ffi::c_char,
        orig_line: u32,
    ) -> *mut ffi::php_stream;
}

/// php_stream_sock_open_from_socket(socket, NULL).
#[cfg(not(php_debug))]
unsafe fn sock_open_from_socket(socket: PhpSocket) -> *mut ffi::php_stream {
    _php_stream_sock_open_from_socket(socket, std::ptr::null())
}

/// php_stream_sock_open_from_socket(socket, NULL).
#[cfg(php_debug)]
unsafe fn sock_open_from_socket(socket: PhpSocket) -> *mut ffi::php_stream {
    let file = c"php-tokio".as_ptr();
    _php_stream_sock_open_from_socket(socket, std::ptr::null(), file, 0, file, 0)
}
