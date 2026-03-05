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

use super::channel::Channel;
use super::suspension::Suspension;
use crate::async_scope::Scope;
use ext_php_rs::{call_user_func, prelude::*, zend::Function};
use lazy_static::lazy_static;
use std::cell::RefCell;
use std::future::pending;
use std::future::Future;
use tokio::runtime::{Handle, Runtime};

lazy_static! {
    pub static ref RUNTIME: Handle = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create runtime");
            tx.send(rt.handle().clone()).expect("Failed to send handle");
            rt.block_on(pending::<()>());
        });
        rx.recv().expect("Failed to receive handle")
    };
}

thread_local! {
    static CHANNEL: RefCell<Option<Channel>> = const { RefCell::new(None) };
}

pub struct EventLoop;

impl EventLoop {
    pub fn suspend_on<F>(future: F) -> PhpResult<F::Output>
    where
        F: Future + Send,
        F::Output: Send + 'static,
    {
        let waker = CHANNEL.with_borrow(|c| c.as_ref().unwrap().waker());
        let suspension = Suspension::get_current()?;
        let suspension_finished = suspension.clone();
        let scope = Scope::spawn(RUNTIME.clone(), async {
            let response = future.await;
            waker.send(suspension_finished);
            response
        });

        // We suspend the fiber here, the Rust stack is kept alive until the result is ready.
        suspension.suspend()?;

        // We've resumed, the `future` is already resolved and is not used by the tokio thread, it's safe to drop it.
        Ok(scope.block_on().map_err(|e| e.to_string())?)
    }

    pub fn wakeup() -> PhpResult<()> {
        CHANNEL.with_borrow_mut(|c| c.as_mut().unwrap().resume_all());
        Ok(())
    }

    pub fn shutdown() {
        CHANNEL.set(None)
    }

    pub fn init() -> PhpResult<u64> {
        if !call_user_func!(
            Function::try_from_function("class_exists").unwrap(),
            "\\Revolt\\EventLoop"
        )?
        .bool()
        .unwrap_or(false)
        {
            return Err("\\Revolt\\EventLoop does not exist!".to_string().into());
        }
        if !call_user_func!(
            Function::try_from_function("interface_exists").unwrap(),
            "\\Revolt\\EventLoop\\Suspension"
        )?
        .bool()
        .unwrap_or(false)
        {
            return Err("\\Revolt\\EventLoop\\Suspension does not exist!"
                .to_string()
                .into());
        }

        CHANNEL.with_borrow_mut(|e| {
            Ok(match e {
                None => e.insert(Channel::new()),
                Some(ev) => ev,
            }
            .raw_fd() as u64)
        })
    }
}
