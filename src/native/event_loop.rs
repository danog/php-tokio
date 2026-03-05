use crate::async_scope::Scope;
use ext_php_rs::convert::IntoZval;
use ext_php_rs::prelude::*;
use ext_php_rs::types::{ZendObject, Zval};
use ext_php_rs::zend::{ClassEntry, Function};
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::{pending, Future};
use std::time::Duration;
use tokio::runtime::{Handle, Runtime};
use tokio_util::sync::CancellationToken;

thread_local! {
    static DRIVER: RefCell<Option<EventLoop>> = const { RefCell::new(None) };
}

pub struct EventLoop {
    next: u64,
    fibers: HashMap<u64, Fiber>,
    handle: Handle,
    cancellation_token: CancellationToken,
    fiber_get_current: Function,
    rx: Option<std::sync::mpsc::Receiver<u64>>,
    tx: std::sync::mpsc::Sender<u64>,
}

impl Drop for EventLoop {
    fn drop(&mut self) {
        self.cancellation_token.cancel()
    }
}

fn with_driver<F, R>(f: F) -> R
where
    F: FnOnce(&mut EventLoop) -> R,
{
    DRIVER.with_borrow_mut(|d| {
        let d = d.get_or_insert_with(EventLoop::new);
        f(d)
    })
}

struct Fiber(Zval);

impl Fiber {
    fn suspend(&self) -> ext_php_rs::error::Result<Zval> {
        self.0.try_call_method("suspend", vec![])
    }

    fn resume(&self) -> ext_php_rs::error::Result<Zval> {
        self.0.try_call_method("resume", vec![])
    }
}

impl Clone for Fiber {
    fn clone(&self) -> Self {
        Fiber(self.0.shallow_clone())
    }
}

#[php_class]
#[php(name = "PhpTokio\\Task")]
pub struct Task {
    cancelled: CancellationToken,
}

#[php_impl]
impl Task {
    pub fn cancel(&self) {
        self.cancelled.cancel()
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel()
    }
}

impl EventLoop {
    fn new() -> Self {
        let (rt_tx, rt_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create runtime");
            let cancel = CancellationToken::new();
            rt_tx
                .send((rt.handle().clone(), cancel.clone()))
                .expect("Failed to send handle");
            rt.block_on(cancel.run_until_cancelled(pending::<()>()));
            rt.shutdown_timeout(Duration::from_secs(1))
        });

        let (handle, cancellation_token) = rt_rx.recv().expect("Failed to receive handle");

        let fiber_get_current = Function::try_from_method("\\Fiber", "getCurrent")
            .expect("Fiber::getCurrent not found");

        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            next: 0,
            fibers: HashMap::new(),
            handle,
            cancellation_token,
            tx,
            rx: Some(rx),
            fiber_get_current,
        }
    }

    pub fn suspend_on<F>(future: F) -> PhpResult<F::Output>
    where
        F: Future + Send,
        F::Output: Send + 'static,
    {
        let (fiber, id_tx, next, handle) = with_driver(|d| -> PhpResult<_> {
            let id_tx = d.tx.clone();
            let fiber = Fiber(d.fiber_get_current.try_call(vec![])?);
            let next = d.next;
            d.fibers.insert(next, fiber.clone());
            d.next += 1;
            Ok((fiber, id_tx, next, d.handle.clone()))
        })?;
        let scope = Scope::spawn(handle, async move {
            let result = future.await;
            let _ = id_tx.send(next);
            result
        });
        fiber.suspend()?;
        Ok(scope.finish_or_abort().map_err(|e| e.to_string())?)
    }

    pub fn shutdown() {
        DRIVER.with_borrow_mut(|d| *d = None)
    }
}

#[php_function]
#[php(name = "PhpTokio\\sleep")]
pub fn sleep(seconds: f64) -> PhpResult<()> {
    EventLoop::suspend_on(async move {
        let duration = Duration::from_secs_f64(seconds);
        tokio::time::sleep(duration).await;
    })
}

#[php_function]
#[php(name = "PhpTokio\\async")]
pub fn create_fiber(callable: &Zval) -> PhpResult<Zval> {
    let ce = ClassEntry::try_find("Fiber").ok_or("Fiber class not found")?;

    let mut obj = ZendObject::new(ce);
    let mut fiber = Zval::new();
    fiber.set_object(&mut obj);

    // call constructor
    fiber.try_call_method("__construct", vec![callable])?;

    fiber.try_call_method("start", vec![])?;

    Ok(fiber)
}

#[php_function]
#[php(name = "PhpTokio\\delay")]
pub fn delay(seconds: f64, callable: &Zval) -> PhpResult<Task> {
    let cancel = CancellationToken::new();
    let cancelled = cancel.clone();
    let callable = callable.shallow_clone();
    let closure = Closure::wrap_once(Box::new(move || -> PhpResult<()> {
        EventLoop::suspend_on(cancel.run_until_cancelled(async move {
            let duration = Duration::from_secs_f64(seconds);
            tokio::time::sleep(duration).await;
        }))?;
        if !cancel.is_cancelled() {
            let _ = callable.try_call(vec![]);
        }
        Ok(())
    }) as Box<dyn FnOnce() -> PhpResult<()>>);
    create_fiber(&closure.into_zval(false)?)?;
    Ok(Task { cancelled })
}

#[php_function]
#[php(name = "PhpTokio\\repeat")]
pub fn repeat(seconds: f64, callable: &Zval) -> PhpResult<Task> {
    let cancel = CancellationToken::new();
    let cancelled = cancel.clone();
    let callable = callable.shallow_clone();
    let closure = Closure::wrap_once(Box::new(move || -> PhpResult<()> {
        loop {
            EventLoop::suspend_on(cancel.run_until_cancelled(async move {
                let duration = Duration::from_secs_f64(seconds);
                tokio::time::sleep(duration).await;
            }))?;
            if cancel.is_cancelled() {
                return Ok(());
            }
            let _ = callable.try_call(vec![]);
        }
    }) as Box<dyn FnOnce() -> PhpResult<()>>);
    create_fiber(&closure.into_zval(false)?)?;
    Ok(Task { cancelled })
}

#[php_function]
#[php(name = "PhpTokio\\run")]
pub fn run() -> PhpResult<()> {
    let Some(rx) = with_driver(|d| d.rx.take()) else {
        return Err("Event loop already running".into());
    };
    loop {
        if with_driver(|d| d.fibers.is_empty()) {
            break;
        }
        let fiber_id = rx.recv().unwrap();
        if let Some(fiber) = with_driver(|d| d.fibers.remove(&fiber_id)) {
            fiber.resume()?;
        }
    }
    with_driver(|d| d.rx = Some(rx));
    Ok(())
}

pub fn setup_module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .class::<Task>()
        .function(wrap_function!(delay))
        .function(wrap_function!(repeat))
        .function(wrap_function!(sleep))
        .function(wrap_function!(run))
        .function(wrap_function!(create_fiber))
}
