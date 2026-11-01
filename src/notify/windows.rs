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

//! Windows has no socketpair(): two connected Unix domain sockets (Windows 10 1803 and later),
//! else two TCP sockets connected on the loopback interface.
//!
//! Unix domain sockets first: unlike loopback TCP, no firewall filters them. Both are Winsock
//! sockets, which PHP's select() emulation hands to the real select() (see the parent module);
//! PHP wraps either kind in the same socket stream, without looking at its address family.

use socket2::{Domain, SockAddr, Type};
use std::{
    env, fs,
    hash::{BuildHasher, RandomState},
    io,
    net::{Ipv4Addr, TcpListener, TcpStream},
    os::windows::io::{AsRawSocket, IntoRawSocket},
    path::PathBuf,
    process, ptr,
};
use windows_sys::Win32::Networking::WinSock::{WSAIoctl, SIO_AF_UNIX_GETPEERPID};

pub(super) type Socket = socket2::Socket;

/// php_socket_t.
pub(super) type PhpSocket = usize;

pub(super) fn pair() -> io::Result<(Socket, Socket)> {
    unix_pair().or_else(|_| tcp_pair())
}

/// Connected through a socket file in the temporary directory, deleted once connected. Fails on
/// Windows before 10 1803 (no AF_UNIX), with a temporary directory whose path is too long for a
/// socket address, or if another process connected first.
fn unix_pair() -> io::Result<(Socket, Socket)> {
    let name = format!(
        "php-tokio-{:016x}.sock",
        RandomState::new().hash_one(process::id())
    );
    let file = Delete(env::temp_dir().join(name));
    let address = SockAddr::unix(&file.0)?;
    let listener = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    listener.bind(&address)?;
    listener.listen(1)?;
    let writer = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    writer.connect(&address)?;
    let (reader, _) = listener.accept()?;
    // A process of the same user could have connected between listen() and connect(). The error
    // is made beforehand: in a closure, it would be on a line that no test runs.
    let ours = peer_pid(&reader)? == process::id();
    let foreign = io::Error::from(io::ErrorKind::AddrInUse);
    ours.then_some((writer, reader)).ok_or(foreign)
}

/// Deletes the socket file: connected sockets don't need it.
struct Delete(PathBuf);

impl Drop for Delete {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The process ID of a Unix domain socket's peer.
fn peer_pid(socket: &Socket) -> io::Result<u32> {
    let mut pid: u32 = 0;
    let mut returned: u32 = 0;
    let result = unsafe {
        WSAIoctl(
            socket.as_raw_socket() as _,
            SIO_AF_UNIX_GETPEERPID,
            ptr::null(),
            0,
            (&raw mut pid).cast(),
            size_of::<u32>() as u32,
            &mut returned,
            ptr::null_mut(),
            None,
        )
    };
    // WSAGetLastError() is GetLastError().
    (result == 0)
        .then_some(pid)
        .ok_or_else(io::Error::last_os_error)
}

/// As PHP's and Python's socketpair() emulations do, but listening on the loopback interface
/// alone (PHP's listens on every interface). Only the connection from our own socket is accepted.
fn tcp_pair() -> io::Result<(Socket, Socket)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let writer = TcpStream::connect(listener.local_addr()?)?;
    let ours = writer.local_addr()?;
    // The first accepted connection that is ours (or the first error). incoming() never ends.
    let mine = |accepted: &io::Result<TcpStream>| {
        accepted
            .as_ref()
            .map_or(true, |s| s.peer_addr().ok() == Some(ours))
    };
    let reader = listener.incoming().find(mine).expect("endless")?;
    // One-byte writes: don't let Nagle's algorithm hold them back.
    writer.set_nodelay(true)?;
    Ok((writer.into(), reader.into()))
}

pub(super) fn into_php_socket(socket: Socket) -> PhpSocket {
    socket.into_raw_socket() as PhpSocket
}
