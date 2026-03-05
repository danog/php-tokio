use std::future::Future;
use std::pin::Pin;
use tokio::runtime;
use tokio::task::{JoinError, JoinHandle};

pub(crate) struct Scope<'a, T> {
    runtime: runtime::Handle,
    handle: Option<JoinHandle<T>>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<T> Drop for Scope<'_, T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = self.runtime.block_on(handle);
        }
    }
}

impl<'a, T: Send + 'static> Scope<'a, T> {
    pub fn spawn<F>(runtime: runtime::Handle, fut: F) -> Scope<'a, T>
    where
        F: Future<Output = T> + Send + 'a,
    {
        let handle = runtime.spawn(unsafe {
            // SAFETY: We erase `'a` to `'static` so Tokio can spawn the future.
            // This is sound because `Scope` guarantees the task cannot outlive `'a`:
            // `Drop` aborts the task and synchronously waits for it to finish,
            // ensuring the future is dropped before the scope ends. This is the
            // same scoped-task pattern used by the `async-scoped` crate.
            std::mem::transmute::<
                Pin<Box<dyn Future<Output = T> + Send + 'a>>,
                Pin<Box<dyn Future<Output = T> + Send>>,
            >(Box::pin(fut))
        });

        Scope {
            runtime,
            handle: Some(handle),
            _marker: std::marker::PhantomData,
        }
    }

    #[allow(unused)]
    pub fn block_on(mut self) -> Result<T, JoinError> {
        let handle = self.handle.take().expect("spawn join handle");
        self.runtime.block_on(handle)
    }

    #[allow(unused)]
    pub fn finish_or_abort(mut self) -> Result<T, JoinError> {
        let task = self.handle.take().expect("task");
        task.abort();
        self.runtime.block_on(task)
    }
}
