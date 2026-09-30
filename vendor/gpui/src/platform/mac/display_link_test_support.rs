//! Fake native handles for portable tests of the production frame registry.
//! Native GPUI and its own-window acceptance fixtures use the real dependencies.

use std::{
    ffi::c_void,
    ops::Deref,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
    },
};

pub type CGDirectDisplayID = u32;

#[expect(non_upper_case_globals, reason = "Mirror the native dispatch symbol")]
pub static _dispatch_source_type_data_add: u8 = 0;

pub struct DispatchRetained<T>(Arc<T>);

impl<T> Clone for DispatchRetained<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Deref for DispatchRetained<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct DispatchQueue;

impl DispatchQueue {
    pub fn main() -> &'static Self {
        &Self
    }
}

pub trait DispatchObject {
    /// # Safety
    /// Mirrors the native signature; this fake stores but never dereferences the pointer.
    unsafe fn set_context(&self, context: *mut c_void);
    fn resume(&self);
}

#[derive(Default)]
pub struct DispatchSource {
    resumed: AtomicBool,
    cancelled: AtomicBool,
    merged: AtomicUsize,
    context: AtomicPtr<c_void>,
}

static LINKS_CREATED: AtomicUsize = AtomicUsize::new(0);
static LINKS_STARTED: AtomicUsize = AtomicUsize::new(0);
static LINKS_STOPPED: AtomicUsize = AtomicUsize::new(0);
static SOURCES_CREATED: AtomicUsize = AtomicUsize::new(0);
static SOURCES_RELEASED: AtomicUsize = AtomicUsize::new(0);
static LOCK_VIOLATIONS: AtomicUsize = AtomicUsize::new(0);
static RELEASE_VIOLATIONS: AtomicUsize = AtomicUsize::new(0);
static FAIL_NEXT_START: AtomicBool = AtomicBool::new(false);
static SERIAL: Mutex<()> = Mutex::new(());

impl DispatchSource {
    /// # Safety
    /// Mirrors the native constructor; no supplied native arguments are dereferenced.
    pub unsafe fn new(
        _: *mut u8,
        _: usize,
        _: usize,
        _: Option<&DispatchQueue>,
    ) -> DispatchRetained<Self> {
        SOURCES_CREATED.fetch_add(1, Ordering::Relaxed);
        DispatchRetained(Arc::new(Self::default()))
    }

    pub fn set_event_handler_f(&self, _: extern "C" fn(*mut c_void)) {}

    pub fn merge_data(&self, value: usize) {
        self.merged.fetch_add(value, Ordering::Relaxed);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl DispatchObject for DispatchSource {
    unsafe fn set_context(&self, context: *mut c_void) {
        self.context.store(context, Ordering::Relaxed);
    }

    fn resume(&self) {
        assert!(
            !self.resumed.swap(true, Ordering::Relaxed),
            "Source resumed twice"
        );
    }
}

impl Drop for DispatchSource {
    fn drop(&mut self) {
        if !self.resumed.load(Ordering::Relaxed) || !self.cancelled.load(Ordering::Relaxed) {
            RELEASE_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        }
        SOURCES_RELEASED.fetch_add(1, Ordering::Relaxed);
    }
}

pub mod sys {
    use super::*;

    pub struct CVDisplayLink;
    pub struct CVTimeStamp;
    pub type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
        *mut CVDisplayLink,
        *const CVTimeStamp,
        *const CVTimeStamp,
        i64,
        *mut i64,
        *mut c_void,
    ) -> i32;

    #[derive(Clone)]
    pub struct DisplayLink;

    fn outside_registry_lock() -> anyhow::Result<()> {
        let available = super::super::REGISTRY.try_lock().is_ok();
        if !available {
            LOCK_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        }
        anyhow::ensure!(
            available,
            "CoreVideo called while holding the registry lock"
        );
        Ok(())
    }

    impl DisplayLink {
        /// # Safety
        /// Mirrors CoreVideo creation; this fake retains no native pointers or callbacks.
        pub unsafe fn new(
            _: CGDirectDisplayID,
            _: CVDisplayLinkOutputCallback,
            _: *mut c_void,
        ) -> anyhow::Result<Self> {
            outside_registry_lock()?;
            LINKS_CREATED.fetch_add(1, Ordering::Relaxed);
            Ok(Self)
        }

        /// # Safety
        /// Mirrors CoreVideo start; this fake only checks registry ownership.
        pub unsafe fn start(&mut self) -> anyhow::Result<()> {
            outside_registry_lock()?;
            LINKS_STARTED.fetch_add(1, Ordering::Relaxed);
            anyhow::ensure!(
                !FAIL_NEXT_START.swap(false, Ordering::Relaxed),
                "Simulated display failure"
            );
            Ok(())
        }

        /// # Safety
        /// Mirrors CoreVideo stop; this fake only checks registry ownership.
        pub unsafe fn stop(&mut self) -> anyhow::Result<()> {
            outside_registry_lock()?;
            LINKS_STOPPED.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
}

extern "C" fn frame(_: *mut c_void) {}

fn tick(display_id: CGDirectDisplayID) {
    // SAFETY: Production callback ignores all native arguments except the integer
    // display id. Fake handles never touch a native view, dispatch source or OS API.
    unsafe {
        super::display_link_output_callback(
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            display_id as usize as *mut c_void,
        );
    }
}

fn serialized() -> anyhow::Result<std::sync::MutexGuard<'static, ()>> {
    SERIAL
        .lock()
        .map_err(|_| anyhow::anyhow!("Fixture lock poisoned"))
}

#[test]
fn a_source_can_be_dropped_before_its_first_subscription() -> anyhow::Result<()> {
    let _serial = serialized()?;
    let links = LINKS_CREATED.load(Ordering::Relaxed);
    let released = SOURCES_RELEASED.load(Ordering::Relaxed);
    let window = super::WindowFrameSource::new(std::ptr::null_mut(), frame);
    drop(window);
    assert_eq!(LINKS_CREATED.load(Ordering::Relaxed), links);
    assert_eq!(SOURCES_RELEASED.load(Ordering::Relaxed) - released, 1);
    assert_eq!(RELEASE_VIOLATIONS.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn repeated_visibility_and_screen_changes_reuse_links_and_one_source() -> anyhow::Result<()> {
    let _serial = serialized()?;
    let links = LINKS_CREATED.load(Ordering::Relaxed);
    let sources = SOURCES_CREATED.load(Ordering::Relaxed);
    let released = SOURCES_RELEASED.load(Ordering::Relaxed);
    let mut window = super::WindowFrameSource::new(std::ptr::null_mut(), frame);
    for _ in 0..100 {
        window.start(1_001)?;
        tick(1_001);
        window.stop();
        window.stop();
        window.start(1_002)?;
        window.stop();
    }
    assert_eq!(LINKS_CREATED.load(Ordering::Relaxed) - links, 2);
    assert_eq!(SOURCES_CREATED.load(Ordering::Relaxed) - sources, 1);
    assert_eq!(window.frame_requests.merged.load(Ordering::Relaxed), 100);
    drop(window);
    assert_eq!(SOURCES_RELEASED.load(Ordering::Relaxed) - released, 1);
    assert_eq!(LOCK_VIOLATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(RELEASE_VIOLATIONS.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn display_ticks_fan_out_only_to_live_subscribers() -> anyhow::Result<()> {
    let _serial = serialized()?;
    let starts = LINKS_STARTED.load(Ordering::Relaxed);
    let stops = LINKS_STOPPED.load(Ordering::Relaxed);
    let mut first = super::WindowFrameSource::new(std::ptr::null_mut(), frame);
    let mut second = super::WindowFrameSource::new(std::ptr::null_mut(), frame);
    first.start(2_001)?;
    second.start(2_001)?;
    assert_eq!(LINKS_STARTED.load(Ordering::Relaxed) - starts, 1);
    tick(2_001);
    assert_eq!(first.frame_requests.merged.load(Ordering::Relaxed), 1);
    assert_eq!(second.frame_requests.merged.load(Ordering::Relaxed), 1);
    first.stop();
    assert_eq!(LINKS_STOPPED.load(Ordering::Relaxed) - stops, 0);
    tick(2_001);
    assert_eq!(first.frame_requests.merged.load(Ordering::Relaxed), 1);
    assert_eq!(second.frame_requests.merged.load(Ordering::Relaxed), 2);
    let source = second.frame_requests.clone();
    drop(second);
    assert_eq!(LINKS_STOPPED.load(Ordering::Relaxed) - stops, 1);
    tick(2_001); // A final native tick cannot reach the removed window source.
    assert_eq!(source.merged.load(Ordering::Relaxed), 2);
    assert!(source.cancelled.load(Ordering::Relaxed));
    assert!(
        super::lock_registry().displays[&2_001]
            .subscribers
            .is_empty()
    );
    drop(source);
    drop(first);
    assert_eq!(LOCK_VIOLATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(RELEASE_VIOLATIONS.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn failed_start_rolls_back_subscription_and_retry_reuses_the_link() -> anyhow::Result<()> {
    let _serial = serialized()?;
    let links = LINKS_CREATED.load(Ordering::Relaxed);
    let mut window = super::WindowFrameSource::new(std::ptr::null_mut(), frame);
    FAIL_NEXT_START.store(true, Ordering::Relaxed);
    assert!(window.start(3_001).is_err());
    assert!(window.registration.is_none());
    tick(3_001);
    assert_eq!(window.frame_requests.merged.load(Ordering::Relaxed), 0);
    assert!(!super::lock_registry().displays[&3_001].running);
    window.start(3_001)?;
    tick(3_001);
    assert_eq!(window.frame_requests.merged.load(Ordering::Relaxed), 1);
    assert_eq!(LINKS_CREATED.load(Ordering::Relaxed) - links, 1);
    drop(window);
    assert_eq!(LOCK_VIOLATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(RELEASE_VIOLATIONS.load(Ordering::Relaxed), 0);
    Ok(())
}
