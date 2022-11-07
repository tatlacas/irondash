#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(non_upper_case_globals)]

use std::ffi::c_void;

use objc::{class, msg_send, rc::StrongPtr, sel, sel_impl};

use self::cocoa::id;

#[link(name = "Foundation", kind = "framework")]
extern "C" {}

#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[repr(C)]
pub struct dispatch_object_s {
    _private: [u8; 0],
}

pub type dispatch_function_t = extern "C" fn(*mut c_void);
pub type dispatch_queue_t = *mut dispatch_object_s;

pub fn dispatch_get_main_queue() -> dispatch_queue_t {
    unsafe { &_dispatch_main_q as *const _ as dispatch_queue_t }
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "ios")),
    link(name = "dispatch", kind = "dylib")
)]
extern "C" {
    static _dispatch_main_q: dispatch_object_s;
    pub fn dispatch_async_f(
        queue: dispatch_queue_t,
        context: *mut c_void,
        work: dispatch_function_t,
    );
}

pub mod cocoa {
    use objc::{class, msg_send, runtime, sel, sel_impl};

    pub use objc::runtime::{BOOL, NO, YES};

    pub type id = *mut runtime::Object;
    pub const nil: id = 0 as id;

    #[cfg(target_pointer_width = "64")]
    pub type CGFloat = std::ffi::c_double;
    #[cfg(not(target_pointer_width = "64"))]
    pub type CGFloat = std::ffi::c_float;

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct NSPoint {
        pub x: CGFloat,
        pub y: CGFloat,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(u64)] // NSUInteger
    pub enum NSEventType {
        NSApplicationDefined = 15,
    }

    impl NSPoint {
        #[inline]
        pub fn new(x: CGFloat, y: CGFloat) -> NSPoint {
            NSPoint { x, y }
        }
    }

    pub trait NSApplication: Sized {
        unsafe fn sharedApplication(_: Self) -> id {
            msg_send![class!(NSApplication), sharedApplication]
        }
        unsafe fn activateIgnoringOtherApps_(self, ignore: BOOL);
        unsafe fn run(self);
        unsafe fn stop_(self, sender: id);
    }

    impl NSApplication for id {
        unsafe fn activateIgnoringOtherApps_(self, ignore: BOOL) {
            msg_send![self, activateIgnoringOtherApps: ignore]
        }

        unsafe fn run(self) {
            msg_send![self, run]
        }

        unsafe fn stop_(self, sender: id) {
            msg_send![self, stop: sender]
        }
    }
}

const UTF8_ENCODING: usize = 4;

pub fn to_nsstring(string: &str) -> StrongPtr {
    unsafe {
        let s: id = msg_send![class!(NSString), alloc];
        let s: id = msg_send![s, initWithBytes:string.as_ptr()
                                 length:string.len()
                                 encoding:UTF8_ENCODING as id];
        StrongPtr::new(s)
    }
}
