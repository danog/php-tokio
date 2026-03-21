use ext_php_rs::exception::PhpResult;
use ext_php_rs::types::Zval;
use ext_php_rs::zend::Function;
use std::cell::RefCell;

thread_local! {
    static GET_CURRENT_SUSPENSION: RefCell<Option<Function>> =  const { RefCell::new(None) };
}

/// # Safety
///
/// `Suspension` contains a PHP `Zval`, but it is never accessed from Tokio threads.
/// The value is only moved across threads and sent back through a channel.
/// All interaction with the inner `Zval` (e.g. calling `resume()`) happens
/// exclusively on the original PHP thread in `EventLoop::wakeup()`.
///
/// Therefore, no PHP memory is accessed off-thread, making `Send` and `Sync` sound
/// under this invariant.
pub struct Suspension(Zval);
unsafe impl Send for Suspension {}
unsafe impl Sync for Suspension {}

impl Suspension {
    pub fn get_current() -> PhpResult<Self> {
        GET_CURRENT_SUSPENSION.with_borrow_mut(|e| {
            Ok(Suspension(
                match e {
                    None => e.insert(
                        Function::try_from_method("\\Revolt\\EventLoop", "getSuspension")
                            .ok_or("\\Revolt\\EventLoop::getSuspension does not exist")?,
                    ),
                    Some(ev) => ev,
                }
                .try_call(vec![])?,
            ))
        })
    }
    pub fn suspend(&self) -> ext_php_rs::error::Result<Zval> {
        self.0.try_call_method("suspend", vec![])
    }

    pub fn resume(&self) -> ext_php_rs::error::Result<Zval> {
        self.0.try_call_method("resume", vec![])
    }
}

impl Clone for Suspension {
    fn clone(&self) -> Self {
        Self(self.0.shallow_clone())
    }
}
