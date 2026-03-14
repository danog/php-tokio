use ext_php_rs::prelude::*;
use php_tokio::php_async_impl;

#[php_class]
#[php(name = "Reqwest\\Client")]
struct Client {}

#[php_async_impl]
impl Client {
    pub async fn get(url: &str) -> anyhow::Result<String> {
        Ok(reqwest::get(url).await?.text().await?)
    }
}

// pub extern "C" fn request_shutdown(_type: i32, _module_number: i32) -> i32 {
//     EventLoop::shutdown();
//     0
// }

#[php_module]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    let module = php_tokio::setup_module(module);
    module
        //.request_shutdown_function(request_shutdown)
        .class::<Client>()
}
