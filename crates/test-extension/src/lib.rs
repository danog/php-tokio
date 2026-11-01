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

//! The PHP extension the test suite (tests/) runs against.

#![cfg_attr(windows, feature(abi_vectorcall))]

use ext_php_rs::{binary_slice::BinarySlice, prelude::*, types::Zval};
use php_tokio::{borrow_unchecked::borrow_unchecked, php_async_impl, EventLoop};
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// The name of the threads of the runtime from `initWith()`.
const THREAD_NAME: &str = "php-tokio-test";

/// The threads of the runtime from `initWith()` that stopped: they all stop once it's released.
static STOPPED_THREADS: AtomicUsize = AtomicUsize::new(0);

#[php_class]
#[php(name = "PhpTokio\\Test\\Native")]
pub struct Native;

#[php_async_impl]
impl Native {
    pub fn init() -> PhpResult<Zval> {
        EventLoop::init()
    }

    /// Initializes the event loop with a runtime of its own: one worker, named THREAD_NAME.
    pub fn init_with() -> PhpResult<Zval> {
        EventLoop::init_with(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name(THREAD_NAME)
                .on_thread_stop(|| {
                    STOPPED_THREADS.fetch_add(1, Ordering::SeqCst);
                })
                .enable_time()
                .build()
        })
    }

    /// Initializes the event loop with a runtime that fails to build.
    pub fn init_with_failing_runtime() -> PhpResult<Zval> {
        EventLoop::init_with(|| Err(io::Error::other("no runtime today")))
    }

    /// The number of threads of the runtime from `initWith()` that stopped.
    pub fn stopped_threads() -> usize {
        STOPPED_THREADS.load(Ordering::SeqCst)
    }

    pub fn shutdown() {
        EventLoop::shutdown();
    }

    /// The lengths of `bytes` borrowed with every kind of borrow_unchecked().
    pub fn borrowed_lengths(bytes: BinarySlice<u8>) -> Vec<usize> {
        let mut owned = bytes.to_vec();
        // SAFETY: the borrows don't outlive this call.
        unsafe {
            vec![
                borrow_unchecked(&owned).len(),
                borrow_unchecked(&mut owned).len(),
                borrow_unchecked(bytes).len(),
            ]
        }
    }

    pub fn drain(ms: u64) -> PhpResult<()> {
        EventLoop::drain(Duration::from_millis(ms))
    }

    pub fn pending() -> usize {
        EventLoop::pending()
    }

    /// Like `sleep()`, through `try_suspend_on()`: null if an exit interrupted it.
    pub fn try_sleep(ms: u64) -> PhpResult<Option<u64>> {
        EventLoop::try_suspend_on(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            ms
        })
    }

    /// The name of the thread the futures run on.
    pub async fn thread_name() -> String {
        std::thread::current().name().unwrap_or_default().to_owned()
    }

    pub fn wakeup() -> PhpResult<()> {
        EventLoop::wakeup()
    }

    /// Completes after `ms` milliseconds.
    pub async fn sleep(ms: u64) -> u64 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        ms
    }

    /// Returns `value`, borrowed from the caller across the suspension, after `ms` milliseconds.
    pub async fn echo(value: &str, ms: u64) -> String {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        value.to_owned()
    }

    /// Panics after `ms` milliseconds.
    pub async fn panic(ms: u64) -> u64 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        panic!("the future of panic()");
    }

    /// Like `panic()`, through `try_suspend_on()`.
    pub fn try_panic(ms: u64) -> PhpResult<Option<u64>> {
        EventLoop::try_suspend_on(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            panic!("the future of tryPanic()");
        })
    }

    /// Fails with `message` after `ms` milliseconds.
    pub async fn fail(message: String, ms: u64) -> anyhow::Result<()> {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Err(anyhow::anyhow!(message))
    }
}

pub extern "C" fn request_shutdown(_type: i32, _module_number: i32) -> i32 {
    EventLoop::shutdown();
    0
}

#[php_module]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .request_shutdown_function(request_shutdown)
        .class::<Native>()
}
