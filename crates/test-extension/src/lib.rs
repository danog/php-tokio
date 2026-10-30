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

use ext_php_rs::{prelude::*, types::Zval};
use php_tokio::{php_async_impl, EventLoop};
use std::time::Duration;

#[php_class]
#[php(name = "PhpTokio\\Test\\Native")]
pub struct Native;

#[php_async_impl]
impl Native {
    pub fn init() -> PhpResult<Zval> {
        EventLoop::init()
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
