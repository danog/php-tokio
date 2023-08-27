// Borrowed from https://github.com/jeremyBanks/you-can

// What's going on here? Unsafe borrows???
// NO: this is actually 100% safe, and here's why.
//
// This is needed because of https://github.com/danog/php-tokio/blob/master/src/event_loop.rs#L72
//
// Rust thinks we're Sending the Future to another thread (tokio's event loop), 
// where it may be used even after its lifetime expires in the main (PHP) thread.
//
// In reality, the Future is only used by Tokio until the result is ready.
//
// Rust does not understand that when we suspend the current fiber in suspend_on, 
// we basically keep alive the the entire stack, 
// including the Rust stack and the Future on it, until the result of the future is ready.
//
// Once the result of the Future is ready, tokio doesn't need it anymore, 
// the suspend_on function is resumed, and we safely drop the Future upon exiting.

use nicelocal_ext_php_rs::binary_slice::{BinarySlice, PackSlice};


#[inline(always)]
pub unsafe fn borrow_unchecked<
    'original,
    'unbounded,
    Ref: BorrowUnchecked<'original, 'unbounded>,
>(
    reference: Ref,
) -> Ref::Unbounded {
    unsafe { BorrowUnchecked::borrow_unchecked(reference) }
}

#[doc(hidden)]
pub unsafe trait BorrowUnchecked<'original, 'unbounded> {
    type Unbounded;

    unsafe fn borrow_unchecked(self) -> Self::Unbounded;
}

unsafe impl<'original, 'unbounded, T: 'unbounded> BorrowUnchecked<'original, 'unbounded>
    for &'original T
{
    type Unbounded = &'unbounded T;

    #[inline(always)]
    unsafe fn borrow_unchecked(self) -> Self::Unbounded {
        unsafe { ::core::mem::transmute(self) }
    }
}

unsafe impl<'original, 'unbounded, T: 'unbounded> BorrowUnchecked<'original, 'unbounded>
    for &'original mut T
{
    type Unbounded = &'unbounded mut T;

    #[inline(always)]
    unsafe fn borrow_unchecked(self) -> Self::Unbounded {
        unsafe { ::core::mem::transmute(self) }
    }
}

unsafe impl<'original, 'unbounded, T: 'unbounded + PackSlice> BorrowUnchecked<'original, 'unbounded>
    for BinarySlice<'original, T>
{
    type Unbounded = BinarySlice<'unbounded, T>;

    #[inline(always)]
    unsafe fn borrow_unchecked(self) -> Self::Unbounded {
        unsafe { ::core::mem::transmute(self) }
    }
}