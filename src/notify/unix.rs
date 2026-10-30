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

//! A socket pair.

use std::{io, os::fd::IntoRawFd};

pub(super) type Socket = std::os::unix::net::UnixStream;

/// php_socket_t.
pub(super) type PhpSocket = std::ffi::c_int;

pub(super) fn pair() -> io::Result<(Socket, Socket)> {
    Socket::pair()
}

pub(super) fn into_php_socket(socket: Socket) -> PhpSocket {
    socket.into_raw_fd()
}
