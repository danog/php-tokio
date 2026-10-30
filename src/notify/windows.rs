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

//! Two TCP sockets connected on the loopback interface: Windows has no socketpair().

use std::{
    io,
    net::{Ipv4Addr, TcpListener},
    os::windows::io::IntoRawSocket,
};

pub(super) type Socket = std::net::TcpStream;

/// php_socket_t.
pub(super) type PhpSocket = usize;

/// As PHP's and Python's socketpair() emulations do. Only the connection from our own socket is
/// accepted.
pub(super) fn pair() -> io::Result<(Socket, Socket)> {
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

pub(super) fn into_php_socket(socket: Socket) -> PhpSocket {
    socket.into_raw_socket() as PhpSocket
}
