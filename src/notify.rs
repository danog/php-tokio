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

//! Tokio threads tell PHP that futures completed through a pipe, whose read end PHP watches
//! (`EventLoop::onReadable` on `php://fd/<fd>`).

use std::{
    io::{self, Read, Write},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) struct Notifier {
    /// Set while a notification is in the pipe: at most one or two bytes are ever in it, so a
    /// write never meets a full pipe (which fails on Unix, and blocks on Windows).
    pending: AtomicBool,
    pipe: Pipe,
}

impl Notifier {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            pending: AtomicBool::new(false),
            pipe: Pipe::new()?,
        })
    }

    /// The descriptor PHP watches for readability.
    pub(crate) fn php_fd(&self) -> u64 {
        self.pipe.php_fd
    }

    /// Wakes PHP up: called by any thread, once the completion is published.
    pub(crate) fn notify(&self) {
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut writer = &self.pipe.writer;
        loop {
            match writer.write(&[0]) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
    }

    /// Consumes the notifications: called on the PHP thread, before collecting the completions.
    ///
    /// The pipe is emptied before the flag is cleared: until then nobody writes (the flag is set),
    /// and a completion published afterwards writes a byte that stays in the pipe. A completion
    /// that skipped its write published its result before seeing the flag set, so it is collected
    /// right after. Clearing the flag first would let the reads consume a byte written meanwhile,
    /// leaving the flag set with an empty pipe: no later completion would ever wake PHP up.
    pub(crate) fn drain(&self) {
        let mut buffer = [0; 64];
        while let Some(available) = self.pipe.available() {
            let mut reader = &self.pipe.reader;
            match reader.read(&mut buffer[..available.min(64)]) {
                Ok(n) if n > 0 => continue,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
        self.pending.store(false, Ordering::SeqCst);
    }
}

#[cfg(unix)]
struct Pipe {
    reader: std::fs::File,
    writer: std::fs::File,
    php_fd: u64,
}

#[cfg(unix)]
impl Pipe {
    fn new() -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};

        let (reader, writer) = sys_pipe()?;
        // SAFETY: both descriptors were just created, and are owned only here.
        let (reader, writer) = unsafe {
            (
                std::fs::File::from_raw_fd(reader),
                std::fs::File::from_raw_fd(writer),
            )
        };
        let php_fd = reader.as_raw_fd() as u64;
        Ok(Self {
            reader,
            writer,
            php_fd,
        })
    }

    /// The pipe is non-blocking: read until it is empty.
    fn available(&self) -> Option<usize> {
        Some(64)
    }
}

#[cfg(any(target_os = "linux", target_os = "solaris"))]
fn sys_pipe() -> io::Result<(i32, i32)> {
    let mut pipefd = [0; 2];
    let ret = unsafe { libc::pipe2(pipefd.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((pipefd[0], pipefd[1]))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "solaris"))))]
fn sys_pipe() -> io::Result<(i32, i32)> {
    use libc::{fcntl, FD_CLOEXEC, F_GETFD, F_GETFL, F_SETFD, F_SETFL, O_NONBLOCK};

    let mut pipefd = [0; 2];
    if unsafe { libc::pipe(pipefd.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    for fd in pipefd {
        let flags = unsafe { fcntl(fd, F_GETFD) };
        if flags == -1 || unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let flags = unsafe { fcntl(fd, F_GETFL) };
        if flags == -1 || unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((pipefd[0], pipefd[1]))
}

/// An anonymous pipe: PHP's select() emulation on Windows reports a pipe as readable only when
/// PeekNamedPipe finds data in it (a socket handle passed as a file descriptor would always be
/// reported as ready). Windows pipes have no non-blocking mode: reads only take what is there.
#[cfg(windows)]
struct Pipe {
    reader: io::PipeReader,
    writer: io::PipeWriter,
    php_fd: u64,
}

#[cfg(windows)]
impl Pipe {
    fn new() -> io::Result<Self> {
        use std::os::windows::io::IntoRawHandle;

        let (reader, writer) = io::pipe()?;
        // PHP opens php://fd/<fd> with a CRT file descriptor: give it one for a duplicate handle.
        let handle = reader.try_clone()?.into_raw_handle();
        let php_fd = unsafe { libc::open_osfhandle(handle as libc::intptr_t, libc::O_RDONLY) };
        if php_fd == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            reader,
            writer,
            php_fd: php_fd as u64,
        })
    }

    fn available(&self) -> Option<usize> {
        use std::os::windows::io::AsRawHandle;

        let mut available = 0u32;
        let ok = unsafe {
            windows_sys::Win32::System::Pipes::PeekNamedPipe(
                self.reader.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        (ok != 0 && available > 0).then_some(available as usize)
    }
}
