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
    static DRIVER: RefCell<EventLoop> = RefCell::new(EventLoop::new());
}

pub struct EventLoop {
    next: u64,
    fibers: HashMap<u64, Fiber>,
    handle: Handle,
    fiber_get_current: Function,
    rx: Option<std::sync::mpsc::Receiver<u64>>,
    tx: std::sync::mpsc::Sender<u64>,
}

struct Fiber(Zval);

impl Fiber {
    fn suspend(&self) {
        self.0
            .object()
            .expect("fiber suspend object")
            .try_call_method("suspend", vec![])
            .expect("suspend fiber");
    }

    fn resume(&self) {
        self.0
            .try_call_method("resume", vec![])
            .expect("fiber resume");
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
            rt_tx
                .send(rt.handle().clone())
                .expect("Failed to send handle");
            rt.block_on(pending::<()>());
        });

        let handle = rt_rx.recv().expect("Failed to receive handle");

        let fiber_get_current = Function::try_from_method("\\Fiber", "getCurrent")
            .expect("Fiber::getCurrent not found");

        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            next: 0,
            fibers: HashMap::new(),
            handle,
            tx,
            rx: Some(rx),
            fiber_get_current,
        }
    }

    pub fn suspend_on<F>(future: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        Self::execute(future).unwrap()
    }

    pub fn execute_cancellable<F>(
        future: F,
        cancel: CancellationToken,
    ) -> PhpResult<Option<F::Output>>
    where
        F: Future + Send,
        F::Output: Send,
    {
        let (tx, rx) = std::sync::mpsc::channel::<F::Output>();
        let (fiber, id_tx, next, handle) = DRIVER.with_borrow_mut(|d| {
            let id_tx = d.tx.clone();
            let fiber = Fiber(d.fiber_get_current.try_call(vec![]).unwrap());
            let next = d.next;
            d.fibers.insert(next, fiber.clone());
            d.next += 1;
            (fiber, id_tx, next, d.handle.clone())
        });
        let (_, spawn_result) = async_scoped::TokioScope::scope_and_block(|s| {
            let _handle = Handle::try_current().map_err(|_| handle.enter());
            s.spawn(cancel.run_until_cancelled(async move {
                tx.send(future.await).unwrap();
                let _ = id_tx.send(next);
            }));
            fiber.suspend();
        });
        match spawn_result.first() {
            Some(Ok(None)) => Ok(None),
            _ => rx.try_recv().map(Some).map_err(|_| {
                PhpException::default("unexpected rust driver receive async error".into())
            }),
        }
    }

    pub fn execute<F>(future: F) -> PhpResult<F::Output>
    where
        F: Future + Send,
        F::Output: Send,
    {
        let result = Self::execute_cancellable(future, CancellationToken::new());
        result.map(|r| {
            r.ok_or_else(|| PhpException::default("unexpected cancelled async function".into()))
        })?
    }
}

#[php_function]
#[php(name = "PhpTokio\\sleep")]
pub fn sleep(seconds: f64) -> PhpResult<()> {
    EventLoop::execute(async move {
        let duration = Duration::from_secs_f64(seconds);
        tokio::time::sleep(duration).await;
    })
}

#[php_function]
#[php(name = "PhpTokio\\async")]
fn create_fiber(callable: &Zval) -> PhpResult<Zval> {
    let ce = ClassEntry::try_find("Fiber").ok_or("Fiber class not found")?;

    // create empty object
    let mut fiber = Zval::new();
    let mut obj = ZendObject::new(ce);
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
        EventLoop::execute_cancellable(
            async move {
                let duration = Duration::from_secs_f64(seconds);
                tokio::time::sleep(duration).await;
            },
            cancel.clone(),
        )?;
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
            EventLoop::execute_cancellable(
                async move {
                    let duration = Duration::from_secs_f64(seconds);
                    tokio::time::sleep(duration).await;
                },
                cancel.clone(),
            )?;
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
pub fn run() {
    let rx = DRIVER.with_borrow_mut(|d| d.rx.take()).unwrap();
    loop {
        if DRIVER.with_borrow(|d| d.fibers.is_empty()) {
            break;
        }
        let fiber_id = rx.recv().unwrap();
        if let Some(fiber) = DRIVER.with_borrow_mut(|d| d.fibers.remove(&fiber_id)) {
            fiber.resume()
        }
    }
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
