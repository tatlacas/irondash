use std::{
    cell::Cell,
    collections::HashMap,
    ffi::c_void,
    mem::ManuallyDrop,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use core_foundation::{
    base::{kCFAllocatorDefault, CFRelease, CFRetain, TCFType},
    date::CFAbsoluteTimeGetCurrent,
    runloop::{
        kCFRunLoopCommonModes, kCFRunLoopDefaultMode, kCFRunLoopRunFinished, kCFRunLoopRunStopped,
        CFRunLoopAddSource, CFRunLoopAddTimer, CFRunLoopGetCurrent, CFRunLoopGetMain, CFRunLoopRef,
        CFRunLoopRemoveTimer, CFRunLoopRunInMode, CFRunLoopSource, CFRunLoopSourceContext,
        CFRunLoopSourceCreate, CFRunLoopSourceSignal, CFRunLoopStop, CFRunLoopTimer,
        CFRunLoopTimerContext, CFRunLoopTimerRef, CFRunLoopWakeUp,
    },
    string::CFStringRef,
};
use objc::rc::{autoreleasepool, StrongPtr};

use self::sys::{dispatch_async_f, dispatch_get_main_queue, to_nsstring};

mod sys;

pub type HandleType = usize;
pub const INVALID_HANDLE: HandleType = 0;

type Callback = Box<dyn FnOnce()>;

struct Timer {
    scheduled: Instant,
    callback: Callback,
}

struct State {
    callbacks: Vec<Callback>,
    timers: HashMap<HandleType, Timer>,
    timer: Option<CFRunLoopTimer>,
    source: Option<CFRunLoopSource>,
    run_loop: CFRunLoopRef,
    run_loop_mode: StrongPtr,
}

// CFRunLoopTimer is thread safe
unsafe impl Send for State {}

struct StatePendingExecution {
    callbacks: Vec<Callback>,
    timers: Vec<Timer>,
}

impl State {
    fn new() -> Self {
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        let run_loop: CFRunLoopRef = unsafe { CFRetain(run_loop as *mut _) } as *mut _;
        Self {
            callbacks: Vec::new(),
            timers: HashMap::new(),
            timer: None,
            source: None,
            run_loop_mode: to_nsstring("NativeShellRunLoopMode"),
            run_loop,
        }
    }

    fn get_pending_execution(&mut self) -> StatePendingExecution {
        let now = Instant::now();
        let pending: Vec<HandleType> = self
            .timers
            .iter()
            .filter(|v| v.1.scheduled <= now)
            .map(|v| *v.0)
            .collect();

        StatePendingExecution {
            callbacks: self.callbacks.drain(0..).collect(),
            timers: pending
                .iter()
                .map(|h| self.timers.remove(h).unwrap())
                .collect(),
        }
    }

    fn remove_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            unsafe {
                CFRunLoopRemoveTimer(
                    self.run_loop,
                    timer.as_concrete_TypeRef(),
                    kCFRunLoopCommonModes,
                );
                CFRunLoopRemoveTimer(
                    self.run_loop,
                    timer.as_concrete_TypeRef(),
                    *self.run_loop_mode as CFStringRef,
                );
            };
        }
    }

    fn create_source(&mut self, state: Arc<Mutex<State>>) {
        let mutex = Arc::as_ptr(&state);
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: mutex as *mut c_void,
            retain: Some(Self::retain),
            release: Some(Self::release),
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Self::on_source,
        };
        let source: CFRunLoopSource = unsafe {
            let source_ref = CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &mut context as *mut _);
            TCFType::wrap_under_create_rule(source_ref)
        };
        unsafe {
            CFRunLoopAddSource(
                self.run_loop,
                source.as_concrete_TypeRef(),
                kCFRunLoopCommonModes,
            );
            // Register source with custom RunLoopMode. This lets clients to only process
            // events scheduled through RunLoopSender (which also includes MessageChannel).
            CFRunLoopAddSource(
                self.run_loop,
                source.as_concrete_TypeRef(),
                *self.run_loop_mode as CFStringRef,
            );
        }
        self.source = Some(source);
    }

    fn remove_source(&mut self) {
        use core_foundation::runloop::CFRunLoopRemoveSource;
        if let Some(source) = self.source.take() {
            unsafe {
                CFRunLoopRemoveSource(
                    self.run_loop,
                    source.as_concrete_TypeRef(),
                    kCFRunLoopCommonModes,
                );
                CFRunLoopRemoveSource(
                    self.run_loop,
                    source.as_concrete_TypeRef(),
                    *self.run_loop_mode as CFStringRef,
                )
            };
        }
    }

    fn next_instant(&self) -> Instant {
        if !self.callbacks.is_empty() {
            Instant::now()
        } else {
            let min = self.timers.values().map(|x| x.scheduled).min();
            min.unwrap_or_else(|| Instant::now() + Duration::from_secs(60 * 60))
        }
    }

    fn schedule(&mut self, state: Arc<Mutex<State>>) {
        self.remove_timer();

        if !self.callbacks.is_empty() {
            if self.source.is_none() {
                self.create_source(state);
            }
            unsafe {
                CFRunLoopSourceSignal(
                    self.source
                        .as_ref()
                        .expect("Failed to create source")
                        .as_concrete_TypeRef(),
                );
                CFRunLoopWakeUp(self.run_loop);
            }
        } else {
            let mutex = Arc::as_ptr(&state);
            let next = self.next_instant();
            let pending = next.saturating_duration_since(Instant::now());
            let fire_date = unsafe { CFAbsoluteTimeGetCurrent() } + pending.as_secs_f64();

            let mut context = CFRunLoopTimerContext {
                version: 0,
                info: mutex as *mut c_void,
                retain: Some(Self::retain),
                release: Some(Self::release),
                copyDescription: None,
            };

            let timer =
                CFRunLoopTimer::new(fire_date, 0.0, 0, 0, Self::on_timer, &mut context as *mut _);
            self.timer = Some(timer.clone());
            unsafe {
                CFRunLoopAddTimer(
                    self.run_loop,
                    timer.as_concrete_TypeRef(),
                    kCFRunLoopCommonModes,
                );
                CFRunLoopAddTimer(
                    self.run_loop,
                    timer.as_concrete_TypeRef(),
                    *self.run_loop_mode as CFStringRef,
                );
                CFRunLoopWakeUp(self.run_loop);
            };
        }
    }

    extern "C" fn retain(data: *const c_void) -> *const c_void {
        let state = data as *const Mutex<State>;
        unsafe { Arc::increment_strong_count(state) }
        data
    }

    extern "C" fn release(data: *const c_void) {
        let state = data as *const Mutex<State>;
        unsafe { Arc::decrement_strong_count(state) };
    }

    extern "C" fn on_timer(_timer: CFRunLoopTimerRef, data: *mut c_void) {
        let state = data as *const Mutex<State>;
        let state = unsafe { Arc::from_raw(state) };
        Self::poll(&state);
        let _ = ManuallyDrop::new(state);
    }

    extern "C" fn on_source(data: *const c_void) {
        let state = data as *const Mutex<State>;
        let state = unsafe { Arc::from_raw(state) };
        Self::poll(&state);
        let _ = ManuallyDrop::new(state);
    }

    fn drain(state: &Arc<Mutex<State>>) {
        autoreleasepool(|| {
            let execution = state.lock().unwrap().get_pending_execution();
            for c in execution.callbacks {
                c();
            }
            for t in execution.timers {
                (t.callback)();
            }
        });
    }

    fn poll(state: &Arc<Mutex<State>>) {
        Self::drain(state);
        if !state.lock().unwrap().timers.is_empty() {
            let state_clone = state.clone();
            state.lock().unwrap().schedule(state_clone);
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.run_loop as *mut _);
        }
    }
}

pub struct PlatformRunLoop {
    next_handle: Cell<HandleType>,
    state: Arc<Mutex<State>>,
    running: Cell<bool>,
}

impl Drop for PlatformRunLoop {
    fn drop(&mut self) {
        // This needs to be done to unref State
        self.state.lock().unwrap().remove_source();
        self.state.lock().unwrap().remove_timer();
    }
}

pub struct PollSession {
    start: Instant,
    timed_out: bool,
}

impl PollSession {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            timed_out: false,
        }
    }
}

impl PlatformRunLoop {
    pub fn new() -> Self {
        Self {
            next_handle: Cell::new(INVALID_HANDLE + 1),
            state: Arc::new(Mutex::new(State::new())),
            running: Cell::new(false),
        }
    }

    fn next_handle(&self) -> HandleType {
        let r = self.next_handle.get();
        self.next_handle.replace(r + 1);
        r
    }

    pub fn unschedule(&self, handle: HandleType) {
        let state_clone = self.state.clone();
        let mut state = self.state.lock().unwrap();
        state.timers.remove(&handle);
        state.schedule(state_clone);
    }

    pub fn schedule<F>(&self, in_time: Duration, callback: F) -> HandleType
    where
        F: FnOnce() + 'static,
    {
        let handle = self.next_handle();

        let state_clone = self.state.clone();
        let mut state = self.state.lock().unwrap();

        state.timers.insert(
            handle,
            Timer {
                scheduled: Instant::now() + in_time,
                callback: Box::new(callback),
            },
        );

        state.schedule(state_clone);

        handle
    }

    pub fn is_main_thread() -> bool {
        unsafe { CFRunLoopGetCurrent() == CFRunLoopGetMain() }
    }

    pub fn run(&self) {
        self.running.set(true);
        // Run-loop will exit immediately if it has no sources, but that's not what we
        // expect from run(). To workaround it schedule a very distant timer.
        let distant_duration = 1.0e10;
        let distant_timer = self.schedule(Duration::from_secs_f64(distant_duration), || {});
        while self.running.get() {
            let result = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, distant_duration, 0) };
            if result == kCFRunLoopRunStopped || result == kCFRunLoopRunFinished {
                State::drain(&self.state);
                self.running.set(false);
            }
        }
        self.unschedule(distant_timer);
    }

    #[cfg(target_os = "macos")]
    pub fn run_app(&self) {
        use self::sys::cocoa::NSApplication;
        use crate::platform::platform_impl::sys::cocoa::nil;
        use objc::runtime::YES;
        unsafe {
            let app = NSApplication::sharedApplication(nil);
            NSApplication::activateIgnoringOtherApps_(app, YES);
            NSApplication::run(app);
        }
    }

    pub fn stop(&self) {
        self.running.set(false);
        unsafe {
            let run_loop: CFRunLoopRef =
                CFRetain(self.state.lock().unwrap().run_loop as *mut _) as *mut _;
            CFRunLoopStop(run_loop);
            CFRelease(run_loop as *mut _);
        }
    }

    #[cfg(target_os = "macos")]
    pub fn stop_app(&self) {
        use self::sys::cocoa::NSApplication;
        use crate::platform::platform_impl::sys::cocoa::{id, nil, NSEventType, NSPoint};
        use objc::{class, msg_send, runtime::YES, sel, sel_impl};
        unsafe {
            let app = NSApplication::sharedApplication(nil);
            app.stop_(nil);

            let dummy_event: id = msg_send![class!(NSEvent),
                otherEventWithType: NSEventType::NSApplicationDefined
                location: NSPoint::new(0.0, 0.0)
                modifierFlags: 0
                timestamp: 0
                windowNumber: 0
                context: nil
                subtype: 0
                data1: 0
                data2: 0
            ];

            // To stop event loop immediately, we need to post event.
            let () = msg_send![app, postEvent: dummy_event atStart: YES];
        }
    }

    pub fn poll_once(&self, poll_session: &mut PollSession) {
        let mode = self.state.lock().unwrap().run_loop_mode.clone();
        if !poll_session.timed_out {
            // We try to drain only tasks scheduled by run_loop. However in some
            // circumstances the UI thread may be waiting for RasterThread (i.e. await toImage)
            // and raster thread might might be waiting to schedule things on UI thread
            // (i.e. ResizeSynchronizer) - in which case we must drain the run loop fully.
            unsafe { CFRunLoopRunInMode(*mode as CFStringRef, 0.006, 1) };
            poll_session.timed_out = poll_session.start.elapsed() >= Duration::from_millis(6);
        } else {
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, 1) };
        }
    }

    pub fn new_sender(&self) -> PlatformRunLoopSender {
        PlatformRunLoopSender::Regular {
            state: StateWrapper(Arc::downgrade(&self.state)),
        }
    }

    pub fn main_thread_fallback_sender() -> PlatformRunLoopSender {
        PlatformRunLoopSender::MainThreadFallback
    }
}

#[derive(Clone)]
pub struct StateWrapper(std::sync::Weak<Mutex<State>>);

#[derive(Clone)]
pub enum PlatformRunLoopSender {
    Regular { state: StateWrapper },
    MainThreadFallback,
}

impl PlatformRunLoopSender {
    pub fn send<F>(&self, callback: F) -> bool
    where
        F: FnOnce() + 'static + Send,
    {
        match self {
            PlatformRunLoopSender::Regular { state } => {
                if let Some(state) = state.0.upgrade() {
                    let state_clone = state.clone();
                    let mut state = state.lock().unwrap();
                    state.callbacks.push(Box::new(callback));
                    state.schedule(state_clone);
                    true
                } else {
                    false
                }
            }
            PlatformRunLoopSender::MainThreadFallback => {
                // This could be done through custom run loop source but it
                // is probably not worth the effort. Just use dispatch queue
                // for now.
                let callback: Box<dyn FnOnce()> = Box::new(callback);
                let callback = Box::new(callback);
                let callback = Box::into_raw(callback);

                unsafe {
                    dispatch_async_f(
                        dispatch_get_main_queue(),
                        callback as *mut c_void,
                        Self::dispatch_work,
                    );
                }
                true
            }
        }
    }

    extern "C" fn dispatch_work(data: *mut c_void) {
        let callback = data as *mut Box<dyn FnOnce()>;
        let callback = unsafe { Box::from_raw(callback) };
        callback();
    }
}
