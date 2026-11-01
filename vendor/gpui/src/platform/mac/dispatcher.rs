#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use crate::{PlatformDispatcher, TaskLabel};
use async_task::Runnable;
use objc::{
    class, msg_send,
    runtime::{BOOL, YES},
    sel, sel_impl,
};
use std::{
    ffi::c_void,
    ptr::{NonNull, addr_of},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// All items in the generated file are marked as pub, so we're gonna wrap it in a separate mod to prevent
/// these pub items from leaking into public API.
pub(crate) mod dispatch_sys {
    include!(concat!(env!("OUT_DIR"), "/dispatch_sys.rs"));
}

use dispatch_sys::*;
pub(crate) fn dispatch_get_main_queue() -> dispatch_queue_t {
    addr_of!(_dispatch_main_q) as *const _ as dispatch_queue_t
}

pub(crate) struct MacDispatcher;

// The context-free native trampoline brackets the sole foreground executor. Native termination
// must observe that scope before borrowing App; no application event can report a blocked callback.
static FOREGROUND_DISPATCH: AtomicBool = AtomicBool::new(false);

impl MacDispatcher {
    pub(crate) fn is_dispatching_foreground() -> bool {
        FOREGROUND_DISPATCH.load(Ordering::Relaxed)
    }
}

impl PlatformDispatcher for MacDispatcher {
    fn is_main_thread(&self) -> bool {
        let is_main_thread: BOOL = unsafe { msg_send![class!(NSThread), isMainThread] };
        is_main_thread == YES
    }

    fn dispatch(&self, runnable: Runnable, _: Option<TaskLabel>) {
        unsafe {
            dispatch_async_f(
                dispatch_get_global_queue(DISPATCH_QUEUE_PRIORITY_HIGH.try_into().unwrap(), 0),
                runnable.into_raw().as_ptr() as *mut c_void,
                Some(trampoline),
            );
        }
    }

    fn dispatch_on_main_thread(&self, runnable: Runnable) {
        unsafe {
            dispatch_async_f(
                dispatch_get_main_queue(),
                runnable.into_raw().as_ptr() as *mut c_void,
                Some(foreground_trampoline),
            );
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: Runnable) {
        unsafe {
            let queue =
                dispatch_get_global_queue(DISPATCH_QUEUE_PRIORITY_HIGH.try_into().unwrap(), 0);
            let when = dispatch_time(DISPATCH_TIME_NOW as u64, duration.as_nanos() as i64);
            dispatch_after_f(
                when,
                queue,
                runnable.into_raw().as_ptr() as *mut c_void,
                Some(trampoline),
            );
        }
    }
}

extern "C" fn trampoline(runnable: *mut c_void) {
    let task = unsafe { Runnable::<()>::from_raw(NonNull::new_unchecked(runnable as *mut ())) };
    task.run();
}

extern "C" fn foreground_trampoline(runnable: *mut c_void) {
    struct Scope(bool);

    impl Drop for Scope {
        fn drop(&mut self) {
            FOREGROUND_DISPATCH.store(self.0, Ordering::Relaxed);
        }
    }

    let _scope = Scope(FOREGROUND_DISPATCH.swap(true, Ordering::Relaxed));
    trampoline(runnable);
}
