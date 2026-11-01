#![cfg_attr(windows, feature(abi_vectorcall))]

use ext_php_rs::{prelude::*, types::Zval};
use php_tokio::{php_async_impl, EventLoop};

#[php_class]
struct Client {}

#[php_async_impl]
impl Client {
    pub fn init() -> PhpResult<Zval> {
        EventLoop::init()
    }
    pub fn wakeup() -> PhpResult<()> {
        EventLoop::wakeup()
    }
    pub async fn get(url: &str) -> anyhow::Result<String> {
        Ok(reqwest::get(url).await?.text().await?)
    }
}

#[php_module]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    // php-tokio shuts the event loop down at the end of the request.
    module.class::<Client>()
}
