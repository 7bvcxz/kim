//! Hover detection on Windows: the window list and UI Automation element rectangles
//! (NOEUL-FR-040, NOEUL-SEC-004, NOEUL-SEC-007; Step 52, ADR 0035). Geometry only.
//!
//! - **Windows** ([`snapshot`]): `EnumWindows` lists the top-level windows in Z-order, topmost
//!   first (*documented*). For each one this module reads only what [`WindowFacts`] holds: its
//!   visibility (`IsWindowVisible`), whether it is minimized (`IsIconic`) or cloaked
//!   (`DWMWA_CLOAKED`), its extended style bits `WS_EX_LAYERED` and `WS_EX_TRANSPARENT`, the
//!   constant opacity of a layered window (`GetLayeredWindowAttributes`), its capture affinity
//!   (`GetWindowDisplayAffinity`) and its rectangle — the extended frame bounds
//!   (`DWMWA_EXTENDED_FRAME_BOUNDS`, the visible frame without the invisible resize border), else
//!   `GetWindowRect`. Both are device pixels for a per-monitor-aware process, whatever the
//!   window's own DPI awareness. No title, class name, process name or content is ever read.
//!   [`Scope::Processes`] restricts the list to some processes and checks the owner's process id
//!   **before** anything else, so tests and the debug-build test hook never look at the user's
//!   windows at all.
//! - **Elements** ([`ElementWorker`]): one worker thread per capture, started only when the user
//!   turns element detection on (`E`), in a COM multithreaded apartment with one
//!   `CUIAutomation` object. A request names a window from the list and a point; the worker
//!   skips windows of processes with higher rights (UIPI: an elevated program; checked from the
//!   process token, never by trying), then walks the control view from `ElementFromHandle` down:
//!   at each level one `FindAllBuildCache(TreeScope_Children)` call with only
//!   `UIA_BoundingRectanglePropertyId` cached, picking the smallest child that contains the point,
//!   within 300 ms, 32 levels and 512 children per level. It reads bounding rectangles and nothing
//!   else: no name, value, help text, automation id, class name or pattern (a test inspects this
//!   file for the calls). Only the newest request is answered; answers are queued for the UI
//!   thread with a waker. The thread ends when the overlay closes; a lookup that hangs in an
//!   unresponsive program is abandoned, never waited for.

use crate::detect::{
    Affinity, ElementAnswer, ElementRequest, Fallback, Snapshot, TargetRights, WindowFacts,
    uia_permission,
};
use crate::frame::Rect;
use crate::win::Handle;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GetLastError, HWND, LPARAM, RECT, S_OK};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetForegroundWindow, GetLayeredWindowAttributes, GetShellWindow,
    GetWindowDisplayAffinity, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, LWA_ALPHA, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WS_EX_LAYERED,
    WS_EX_TRANSPARENT,
};
use windows_sys::core::{GUID, HRESULT};

/// Whose windows a list may hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every top-level window of the desktop (a real capture).
    All,
    /// Only windows of these processes. Every other window is passed over after reading the
    /// owner's process id and nothing else (tests and the debug-build test hook).
    Processes(Vec<u32>),
}

/// The top-level windows over `picture`, topmost first, as [`crate::detect`] offers them;
/// `skip` (the open overlay) is never listed. An enumeration failure gives an unavailable list,
/// so the overlay falls back to the plain region selection.
pub fn snapshot(picture: Rect, scope: &Scope, skip: Option<HWND>) -> Snapshot {
    unsafe extern "system" fn collect(h: HWND, data: LPARAM) -> i32 {
        // SAFETY: `data` is the `&mut Vec<usize>` passed below, alive during EnumWindows.
        unsafe { (*(data as *mut Vec<usize>)).push(h as usize) };
        1
    }
    let mut handles: Vec<usize> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, passed by pointer for the call's duration.
    if unsafe { EnumWindows(Some(collect), (&raw mut handles) as LPARAM) } == 0 {
        return Snapshot::unavailable();
    }
    let facts = handles
        .into_iter()
        .map(|h| h as HWND)
        .filter(|h| Some(*h) != skip)
        .filter_map(|h| facts_of(h, scope));
    Snapshot::from_facts(facts, picture)
}

/// The process that owns `h`; `None` when the window is gone (a window destroyed between the
/// enumeration and this query has no owner — never "process 0").
/// The window that has the keyboard focus (`GetForegroundWindow`) as [`WindowFacts`], for
/// `capture.window` (Step 58): `None` when no window has focus, when the desktop itself has it
/// (`GetShellWindow`), or when `scope` leaves the window's process out. Geometry and flags only.
pub fn foreground(scope: &Scope) -> Option<WindowFacts> {
    // SAFETY: plain queries without arguments.
    let (h, shell) = unsafe { (GetForegroundWindow(), GetShellWindow()) };
    if h.is_null() || h == shell {
        return None;
    }
    facts_of(h, scope)
}

/// One window's facts when `scope` allows its process (the debug-build test hook names a test's
/// own window this way; Step 58).
pub fn window(h: HWND, scope: &Scope) -> Option<WindowFacts> {
    if h.is_null() {
        return None;
    }
    facts_of(h, scope)
}

fn owner(h: HWND) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: a window handle from the enumeration (a stale one makes this fail harmlessly).
    let thread = unsafe { GetWindowThreadProcessId(h, &raw mut pid) };
    (thread != 0).then_some(pid)
}

/// Geometry and flags of one window (never text).
fn facts_of(h: HWND, scope: &Scope) -> Option<WindowFacts> {
    if let Scope::Processes(pids) = scope
        && !owner(h).is_some_and(|pid| pids.contains(&pid))
    {
        return None;
    }
    let token = h as usize as u64;
    // SAFETY: plain queries on a window handle (a stale one makes them fail harmlessly).
    let visible = unsafe { IsWindowVisible(h) } != 0;
    if !visible {
        let mut f = WindowFacts::plain(token, Rect::new(0, 0, 0, 0));
        f.visible = false;
        return Some(f);
    }
    // SAFETY: as above.
    let minimized = unsafe { IsIconic(h) } != 0;
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is a writable u32, the attribute's documented size.
    let cloaked = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED as u32,
            (&raw mut cloaked).cast::<c_void>(),
            4,
        )
    } == S_OK
        && cloaked != 0;
    // SAFETY: as above.
    let ex = unsafe { GetWindowLongW(h, GWL_EXSTYLE) } as u32;
    let layered = ex & WS_EX_LAYERED != 0;
    let transparent = ex & WS_EX_TRANSPARENT != 0;
    let alpha = if layered {
        let (mut a, mut flags) = (0u8, 0u32);
        // SAFETY: writable out-parameters; the colour key is not asked for.
        let ok = unsafe {
            GetLayeredWindowAttributes(h, std::ptr::null_mut(), &raw mut a, &raw mut flags)
        } != 0;
        (ok && flags & LWA_ALPHA != 0).then_some(a)
    } else {
        None
    };
    let mut aff = 0u32;
    // SAFETY: a writable out-parameter.
    let affinity = if unsafe { GetWindowDisplayAffinity(h, &raw mut aff) } != 0 {
        match aff {
            WDA_EXCLUDEFROMCAPTURE => Affinity::Excluded,
            WDA_MONITOR => Affinity::Monitor,
            _ => Affinity::None,
        }
    } else {
        Affinity::None
    };
    Some(WindowFacts {
        token,
        rect: frame_bounds(h)?,
        visible,
        minimized,
        cloaked,
        layered,
        transparent,
        alpha,
        affinity,
    })
}

fn rect_of(r: RECT) -> Rect {
    Rect::new(
        r.left,
        r.top,
        u32::try_from(i64::from(r.right) - i64::from(r.left)).unwrap_or(0),
        u32::try_from(i64::from(r.bottom) - i64::from(r.top)).unwrap_or(0),
    )
}

/// The extended frame bounds of `h`, else its window rectangle (device pixels).
fn frame_bounds(h: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    // SAFETY: `r` is a writable RECT, the attribute's documented type and size.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&raw mut r).cast::<c_void>(),
            u32::try_from(size_of::<RECT>()).unwrap_or(16),
        )
    };
    if dwm == S_OK {
        return Some(rect_of(r));
    }
    // SAFETY: as above.
    (unsafe { GetWindowRect(h, &raw mut r) } != 0).then(|| rect_of(r))
}

/// Whether a process token is elevated (`None`: it could not be read).
fn token_elevated(token: &Handle) -> Option<bool> {
    let mut e = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: a token handle with TOKEN_QUERY; `e` is the documented output structure.
    let ok = unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            (&raw mut e).cast::<c_void>(),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(4),
            &raw mut len,
        )
    } != 0;
    ok.then_some(e.TokenIsElevated != 0)
}

/// Whether Noeul itself runs elevated.
pub fn own_elevated() -> bool {
    let mut t = std::ptr::null_mut();
    // SAFETY: this process's own token, for query; closed by `Handle`.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut t) } == 0 {
        return false;
    }
    Handle::new(t)
        .ok()
        .and_then(|t| token_elevated(&t))
        .unwrap_or(false)
}

/// The rights of the process that owns `h`, read from its token (UIPI).
fn target_rights(h: HWND) -> TargetRights {
    let Some(pid) = owner(h).filter(|pid| *pid != 0) else {
        return TargetRights::Unknown;
    };
    // SAFETY: a limited query handle, closed by `Handle`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    let Ok(process) = Handle::new(process) else {
        // SAFETY: reads this thread's last error right after the failed call.
        return if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
            TargetRights::Denied
        } else {
            TargetRights::Unknown
        };
    };
    let mut t = std::ptr::null_mut();
    // SAFETY: the process handle above; `t` receives a token handle closed by `Handle`.
    if unsafe { OpenProcessToken(process.raw(), TOKEN_QUERY, &raw mut t) } == 0 {
        // SAFETY: as above.
        return if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
            TargetRights::Denied
        } else {
            TargetRights::Unknown
        };
    }
    match Handle::new(t).ok().and_then(|t| token_elevated(&t)) {
        Some(e) => TargetRights::Elevated(e),
        None => TargetRights::Unknown,
    }
}

/// The time budget of one element lookup.
pub const BUDGET: Duration = Duration::from_millis(300);
const MAX_DEPTH: usize = 32;
const MAX_CHILDREN: i32 = 512;

/// `CUIAutomation`, `IUIAutomation` and `IUIAutomation2` (UIAutomationClient.h, *documented*).
static CLSID_CUIAUTOMATION: GUID = GUID::from_u128(0xff48dba4_60ef_4201_aa87_54103eef594e);
static IID_IUIAUTOMATION: GUID = GUID::from_u128(0x30cbe57d_d9d0_452a_ab13_7ac5ac4825ee);
static IID_IUIAUTOMATION2: GUID = GUID::from_u128(0x34723aff_0c9d_49d0_9896_7ab52df8cd8a);
const UIA_BOUNDING_RECTANGLE_PROPERTY_ID: i32 = 30001;
const TREE_SCOPE_CHILDREN: i32 = 2;
const E_ACCESSDENIED: HRESULT = 0x8007_0005_u32 as HRESULT;
const UIA_E_TIMEOUT: HRESULT = 0x8013_1505_u32 as HRESULT;

/// Vtable slots of the interfaces used (IUnknown has 0–2). Only bounding rectangles are read.
mod slot {
    pub const QUERY_INTERFACE: usize = 0;
    pub const RELEASE: usize = 2;
    // IUIAutomation
    pub const ELEMENT_FROM_HANDLE: usize = 6;
    pub const CONTROL_VIEW_CONDITION: usize = 18;
    pub const CREATE_CACHE_REQUEST: usize = 20;
    // IUIAutomation2
    pub const SET_CONNECTION_TIMEOUT: usize = 61;
    pub const SET_TRANSACTION_TIMEOUT: usize = 63;
    // IUIAutomationElement
    pub const FIND_ALL_BUILD_CACHE: usize = 8;
    pub const CURRENT_BOUNDING_RECTANGLE: usize = 43;
    pub const CACHED_BOUNDING_RECTANGLE: usize = 75;
    // IUIAutomationCacheRequest
    pub const ADD_PROPERTY: usize = 3;
    // IUIAutomationElementArray
    pub const LENGTH: usize = 3;
    pub const GET_ELEMENT: usize = 4;
}

type FnOut = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
type FnQi = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
type FnRelease = unsafe extern "system" fn(*mut c_void) -> u32;
type FnHwnd = unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT;
type FnU32 = unsafe extern "system" fn(*mut c_void, u32) -> HRESULT;
type FnI32 = unsafe extern "system" fn(*mut c_void, i32) -> HRESULT;
type FnRect = unsafe extern "system" fn(*mut c_void, *mut RECT) -> HRESULT;
type FnLen = unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT;
type FnIndex = unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT;
type FnFindAll = unsafe extern "system" fn(
    *mut c_void,
    i32,
    *mut c_void,
    *mut c_void,
    *mut *mut c_void,
) -> HRESULT;

/// One COM interface pointer, released once on drop.
#[derive(Debug)]
struct Com(NonNull<c_void>);

impl Com {
    fn from_raw(p: *mut c_void) -> Option<Com> {
        NonNull::new(p).map(Com)
    }

    fn raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The function in vtable slot `i`.
    ///
    /// # Safety
    /// `i` must be a slot of this interface's vtable.
    unsafe fn slot(&self, i: usize) -> *const c_void {
        // SAFETY: a live COM object starts with a pointer to its vtable; the caller names a slot
        // inside it.
        unsafe { *(*(self.raw() as *const *const *const c_void)).add(i) }
    }

    /// Calls a method that returns an interface pointer through its last argument.
    fn out(&self, f: impl FnOnce(*mut *mut c_void) -> HRESULT) -> Result<Com, HRESULT> {
        let mut p = std::ptr::null_mut();
        let hr = f(&raw mut p);
        if hr < 0 {
            return Err(hr);
        }
        Com::from_raw(p).ok_or(hr)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: slot 2 of every COM vtable is IUnknown::Release; this value owns one reference.
        unsafe {
            let release = std::mem::transmute::<*const c_void, FnRelease>(self.slot(slot::RELEASE));
            release(self.raw());
        }
    }
}

// SAFETY: UI Automation's objects are free-threaded and only used by the worker thread that
// created them (a multithreaded apartment); `Com` is moved there, never shared.
unsafe impl Send for Com {}

/// UI Automation for one worker: the automation object, the control view condition, a cache
/// request for bounding rectangles only, and the time budget of one lookup.
#[derive(Debug)]
struct Uia {
    automation: Com,
    condition: Com,
    cache: Com,
    budget: Duration,
}

impl Uia {
    fn new(budget: Duration) -> Option<Uia> {
        let mut p = std::ptr::null_mut();
        // SAFETY: documented class and interface ids; `p` receives an owned interface pointer.
        let hr = unsafe {
            CoCreateInstance(
                &raw const CLSID_CUIAUTOMATION,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &raw const IID_IUIAUTOMATION,
                &raw mut p,
            )
        };
        if hr < 0 {
            return None;
        }
        let automation = Com::from_raw(p)?;
        // SAFETY: slots of IUIAutomation (UIAutomationClient.h order) with their signatures.
        let (condition, cache) = unsafe {
            let cond = std::mem::transmute::<*const c_void, FnOut>(
                automation.slot(slot::CONTROL_VIEW_CONDITION),
            );
            let condition = automation.out(|o| cond(automation.raw(), o)).ok()?;
            let create = std::mem::transmute::<*const c_void, FnOut>(
                automation.slot(slot::CREATE_CACHE_REQUEST),
            );
            let cache = automation.out(|o| create(automation.raw(), o)).ok()?;
            let add = std::mem::transmute::<*const c_void, FnI32>(cache.slot(slot::ADD_PROPERTY));
            if add(cache.raw(), UIA_BOUNDING_RECTANGLE_PROPERTY_ID) < 0 {
                return None;
            }
            (condition, cache)
        };
        // Shorter timeouts where available (Windows 8 and later): a hung program costs at most
        // the budget, never the 20 s default.
        // SAFETY: QueryInterface on our object; IUIAutomation2 slots with their signatures.
        unsafe {
            let qi =
                std::mem::transmute::<*const c_void, FnQi>(automation.slot(slot::QUERY_INTERFACE));
            if let Ok(two) =
                automation.out(|o| qi(automation.raw(), &raw const IID_IUIAUTOMATION2, o))
            {
                let ms = u32::try_from(budget.as_millis()).unwrap_or(300);
                let conn = std::mem::transmute::<*const c_void, FnU32>(
                    two.slot(slot::SET_CONNECTION_TIMEOUT),
                );
                let trans = std::mem::transmute::<*const c_void, FnU32>(
                    two.slot(slot::SET_TRANSACTION_TIMEOUT),
                );
                conn(two.raw(), ms);
                trans(two.raw(), ms);
            }
        }
        Some(Uia {
            automation,
            condition,
            cache,
            budget,
        })
    }

    /// The element of a window (`ElementFromHandle`).
    fn element_of(&self, h: HWND) -> Result<Com, HRESULT> {
        // SAFETY: IUIAutomation::ElementFromHandle with its signature.
        unsafe {
            let f = std::mem::transmute::<*const c_void, FnHwnd>(
                self.automation.slot(slot::ELEMENT_FROM_HANDLE),
            );
            self.automation.out(|o| f(self.automation.raw(), h, o))
        }
    }

    /// The control-view children of `e` with their bounding rectangles cached (one call).
    fn children(&self, e: &Com) -> Result<Vec<(Com, Rect)>, HRESULT> {
        // SAFETY: IUIAutomationElement::FindAllBuildCache and IUIAutomationElementArray's
        // `Length` and `GetElement`, IUIAutomationElement::CachedBoundingRectangle, with their
        // signatures; every returned pointer is owned by a `Com`.
        unsafe {
            let find =
                std::mem::transmute::<*const c_void, FnFindAll>(e.slot(slot::FIND_ALL_BUILD_CACHE));
            let all = e.out(|o| {
                find(
                    e.raw(),
                    TREE_SCOPE_CHILDREN,
                    self.condition.raw(),
                    self.cache.raw(),
                    o,
                )
            })?;
            let len = std::mem::transmute::<*const c_void, FnLen>(all.slot(slot::LENGTH));
            let mut n = 0i32;
            let hr = len(all.raw(), &raw mut n);
            if hr < 0 {
                return Err(hr);
            }
            let get = std::mem::transmute::<*const c_void, FnIndex>(all.slot(slot::GET_ELEMENT));
            let mut out = Vec::new();
            for i in 0..n.clamp(0, MAX_CHILDREN) {
                let Ok(child) = all.out(|o| get(all.raw(), i, o)) else {
                    continue;
                };
                let rect = std::mem::transmute::<*const c_void, FnRect>(
                    child.slot(slot::CACHED_BOUNDING_RECTANGLE),
                );
                let mut r = RECT::default();
                if rect(child.raw(), &raw mut r) >= 0 {
                    out.push((child, rect_of(r)));
                }
            }
            Ok(out)
        }
    }

    /// The window's own bounding rectangle as UI Automation reports it.
    fn bounds(e: &Com) -> Option<Rect> {
        // SAFETY: IUIAutomationElement::CurrentBoundingRectangle with its signature.
        unsafe {
            let f = std::mem::transmute::<*const c_void, FnRect>(
                e.slot(slot::CURRENT_BOUNDING_RECTANGLE),
            );
            let mut r = RECT::default();
            (f(e.raw(), &raw mut r) >= 0).then(|| rect_of(r))
        }
    }

    /// The deepest element under `req.at` in `req.token`'s window, within the budget
    /// ([`BUDGET`] in Noeul).
    fn element_at(&self, req: &ElementRequest) -> ElementAnswer {
        let deadline = Instant::now() + self.budget;
        let at = Rect::new(req.at.0, req.at.1, 1, 1);
        let mut current = match self.element_of(req.token as usize as HWND) {
            Ok(e) => e,
            Err(hr) => return ElementAnswer::Failed(fallback_of(hr)),
        };
        if Uia::bounds(&current).is_none() {
            return ElementAnswer::Failed(Fallback::Unavailable);
        }
        let mut best: Option<Rect> = None;
        for _ in 0..MAX_DEPTH {
            if Instant::now() > deadline {
                return best.map_or(
                    ElementAnswer::Failed(Fallback::Timeout),
                    ElementAnswer::Found,
                );
            }
            let children = match self.children(&current) {
                Ok(c) => c,
                Err(hr) => {
                    return best
                        .map_or(ElementAnswer::Failed(fallback_of(hr)), ElementAnswer::Found);
                }
            };
            let area = |r: &Rect| u64::from(r.width) * u64::from(r.height);
            let Some((child, r)) = children
                .into_iter()
                .filter(|(_, r)| !r.is_empty() && r.contains(&at))
                .min_by_key(|(_, r)| area(r))
            else {
                break;
            };
            best = Some(r);
            current = child;
        }
        best.map_or(ElementAnswer::WindowOnly, ElementAnswer::Found)
    }
}

/// What a failed UI Automation call means for the fallback.
fn fallback_of(hr: HRESULT) -> Fallback {
    match hr {
        E_ACCESSDENIED => Fallback::Denied,
        UIA_E_TIMEOUT => Fallback::Timeout,
        _ => Fallback::Unavailable,
    }
}

/// Answers queued for the UI thread.
type Answers = Arc<Mutex<VecDeque<(ElementRequest, ElementAnswer)>>>;

/// The element lookups of one capture: a worker thread, its requests and its answers.
pub struct ElementWorker {
    tx: Option<Sender<ElementRequest>>,
    answers: Answers,
}

impl std::fmt::Debug for ElementWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElementWorker").finish_non_exhaustive()
    }
}

impl ElementWorker {
    /// Starts the worker with Noeul's per-lookup [`BUDGET`]; `waker` runs (on the worker
    /// thread) after each answer is queued.
    pub fn start(waker: Box<dyn Fn() + Send>) -> std::io::Result<ElementWorker> {
        ElementWorker::start_with_budget(waker, BUDGET)
    }

    /// The same with another time budget per lookup (the UI Automation connection and
    /// transaction timeouts too). Only tests use a longer one: a lookup of their own window must
    /// not depend on how fast a cold machine loads UI Automation.
    pub fn start_with_budget(
        waker: Box<dyn Fn() + Send>,
        budget: Duration,
    ) -> std::io::Result<ElementWorker> {
        let (tx, rx) = channel();
        let answers: Answers = Arc::default();
        let out = Arc::clone(&answers);
        std::thread::Builder::new()
            .name("noeul-uia".into())
            .spawn(move || worker(&rx, &out, &*waker, budget))?;
        Ok(ElementWorker {
            tx: Some(tx),
            answers,
        })
    }

    /// Asks for an element (only the newest pending request is answered).
    pub fn ask(&self, req: ElementRequest) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(req);
        }
    }

    /// The answers that arrived, in order.
    pub fn take_answers(&self) -> Vec<(ElementRequest, ElementAnswer)> {
        self.answers
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }
}

impl Drop for ElementWorker {
    fn drop(&mut self) {
        // Closing the channel ends the worker after its current lookup; it is never joined, so
        // a lookup stuck in an unresponsive program cannot hold up the UI thread.
        self.tx.take();
    }
}

fn worker(rx: &Receiver<ElementRequest>, out: &Answers, waker: &dyn Fn(), budget: Duration) {
    // SAFETY: joins this new thread to the multithreaded apartment; balanced below on success.
    let com = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) } >= 0;
    let uia = if com { Uia::new(budget) } else { None };
    let own = own_elevated();
    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let answer = match &uia {
            None => ElementAnswer::Failed(Fallback::Unavailable),
            Some(u) => match uia_permission(own, target_rights(req.token as usize as HWND)) {
                Some(f) => ElementAnswer::Failed(f),
                None => u.element_at(&req),
            },
        };
        if let Ok(mut q) = out.lock() {
            q.push_back((req, answer));
        }
        waker();
    }
    drop(uia);
    if com {
        // SAFETY: balances the successful CoInitializeEx of this thread.
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::wide;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_UNAWARE, SetThreadDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
        HWND_TOP, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
        RegisterClassExW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
        TranslateMessage, WNDCLASSEXW, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
        WS_VISIBLE,
    };

    /// Far off-screen: no real monitor is there, so nothing a test creates is visible.
    const X: i32 = -21_000;
    const Y: i32 = -21_000;
    const PICTURE: Rect = Rect::new(X, Y, 3000, 1500);

    fn class() -> *const u16 {
        static NAME: OnceLock<usize> = OnceLock::new();
        *NAME.get_or_init(|| {
            let name = wide("NoeulDetectTest");
            let wc = WNDCLASSEXW {
                cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap(),
                lpfnWndProc: Some(DefWindowProcW),
                // SAFETY: this executable's module handle.
                hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
                lpszClassName: name.as_ptr(),
                ..Default::default()
            };
            // SAFETY: a fully initialized class description.
            unsafe { RegisterClassExW(&raw const wc) };
            Box::leak(name.into_boxed_slice()).as_ptr() as usize
        }) as *const u16
    }

    /// One of our own off-screen windows: never activated, no taskbar entry, shown unless
    /// `hidden`. Destroyed on drop.
    struct Own(HWND);

    impl Own {
        fn new(r: Rect, ex: u32, hidden: bool) -> Own {
            let title = wide("NOEUL-SECRET-TITLE");
            // SAFETY: our registered class; plain values; no parent.
            let h = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | ex,
                    class(),
                    title.as_ptr(),
                    WS_POPUP,
                    r.x,
                    r.y,
                    i32::try_from(r.width).unwrap(),
                    i32::try_from(r.height).unwrap(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                )
            };
            assert!(!h.is_null(), "{}", std::io::Error::last_os_error());
            if !hidden {
                // SAFETY: shows our own window without activating it.
                unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
            }
            Own(h)
        }

        fn token(&self) -> u64 {
            self.0 as usize as u64
        }

        /// Puts the window at the top of the Z-order, without activating it.
        fn raise(&self) {
            // SAFETY: our own window.
            unsafe {
                SetWindowPos(
                    self.0,
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
        }
    }

    impl Drop for Own {
        fn drop(&mut self) {
            // SAFETY: our own window, destroyed once.
            unsafe { DestroyWindow(self.0) };
        }
    }

    fn pump() {
        // SAFETY: MSG is plain data; this thread's own messages are dispatched.
        unsafe {
            let mut m: MSG = std::mem::zeroed();
            while PeekMessageW(&raw mut m, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&raw const m);
                DispatchMessageW(&raw const m);
            }
        }
    }

    fn ours() -> Scope {
        Scope::Processes(vec![std::process::id()])
    }

    /// Our own window types as detection targets: a plain window, one above it, a layered
    /// window, a fully transparent one, a click-through one, one excluded from capture, a
    /// protected one, a hidden one and one created by a DPI-unaware thread. The list holds only
    /// this process's windows (nothing of the user's is read) and classifies each as the table
    /// of `crate::detect` says.
    #[test]
    fn own_windows_of_every_kind_are_classified_and_the_topmost_wins() {
        crate::win::per_monitor_dpi_awareness();
        // SAFETY: a plain query.
        let before = unsafe { GetForegroundWindow() };
        let r = |dx: i32, dy: i32, w: u32, h: u32| Rect::new(X + dx, Y + dy, w, h);
        let below = Own::new(r(0, 0, 600, 400), 0, false);
        let above = Own::new(r(100, 100, 200, 150), 0, false);
        above.raise();
        let layered = Own::new(r(700, 0, 200, 200), WS_EX_LAYERED, false);
        let invisible = Own::new(r(950, 0, 200, 200), WS_EX_LAYERED, false);
        let through = Own::new(
            r(1200, 0, 200, 200),
            WS_EX_LAYERED | WS_EX_TRANSPARENT,
            false,
        );
        let excluded = Own::new(r(1450, 0, 200, 200), 0, false);
        let protected = Own::new(r(1700, 0, 200, 200), 0, false);
        let hidden = Own::new(r(1950, 0, 200, 200), 0, true);
        // SAFETY: attributes of our own windows.
        unsafe {
            assert_ne!(SetLayeredWindowAttributes(layered.0, 0, 200, LWA_ALPHA), 0);
            assert_ne!(SetLayeredWindowAttributes(invisible.0, 0, 0, LWA_ALPHA), 0);
            assert_ne!(SetLayeredWindowAttributes(through.0, 0, 255, LWA_ALPHA), 0);
            assert_ne!(
                SetWindowDisplayAffinity(excluded.0, WDA_EXCLUDEFROMCAPTURE),
                0,
                "WDA_EXCLUDEFROMCAPTURE needs Windows 10 2004 or later: {}",
                std::io::Error::last_os_error()
            );
            assert_ne!(SetWindowDisplayAffinity(protected.0, WDA_MONITOR), 0);
        }
        // A window created by a DPI-unaware thread: the list still reads device pixels.
        // SAFETY: switches this thread's DPI context for one window creation and restores it.
        let unaware = unsafe {
            let old = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);
            let w = Own::new(r(2200, 0, 300, 200), 0, false);
            SetThreadDpiAwarenessContext(old);
            w
        };
        pump();
        let s = snapshot(PICTURE, &ours(), None);
        assert!(s.taken);
        let find = |o: &Own| s.windows().iter().find(|w| w.token == o.token()).copied();
        assert_eq!(find(&below).map(|w| w.rect), Some(r(0, 0, 600, 400)));
        assert_eq!(find(&above).map(|w| w.rect), Some(r(100, 100, 200, 150)));
        assert!(find(&layered).is_some_and(|w| w.layered && !w.protected));
        assert_eq!(find(&invisible), None, "alpha 0: invisible");
        assert_eq!(find(&through), None, "click-through");
        assert_eq!(find(&excluded), None, "absent from captures");
        assert!(
            find(&protected).is_some_and(|w| w.protected),
            "captured black"
        );
        assert_eq!(find(&hidden), None);
        assert_eq!(
            find(&unaware).map(|w| w.rect),
            Some(r(2200, 0, 300, 200)),
            "device pixels (this machine's monitor is at 100 %, so the unaware window is not scaled)"
        );
        // Z-order: the raised window wins where both are; the one below elsewhere.
        assert_eq!(
            s.window_at((X + 150, Y + 150)).map(|w| w.token),
            Some(above.token())
        );
        assert_eq!(
            s.window_at((X + 20, Y + 20)).map(|w| w.token),
            Some(below.token())
        );
        assert!(s.window_at((X + 1750, Y + 50)).is_some_and(|w| w.protected));
        // The overlay window is never listed.
        let without = snapshot(PICTURE, &ours(), Some(below.0));
        assert!(without.windows().iter().all(|w| w.token != below.token()));
        // A process list without us: nothing is read, nothing is listed.
        let none = snapshot(PICTURE, &Scope::Processes(vec![0]), None);
        assert!(none.taken && none.is_empty() && none.skipped() == 0);
        // No text anywhere in what detection keeps.
        assert!(!format!("{s:?}").contains("NOEUL-SECRET"));
        // SAFETY: a plain query.
        let after = unsafe { GetForegroundWindow() };
        assert_eq!(after, before, "focus never moved");
    }

    /// UI Automation against our own window only: the deepest control under a point is found
    /// as a rectangle (the second button), the answer holds no text, and a point on the
    /// window's empty area answers "the window itself".
    ///
    /// Deterministic on a slow machine (it failed twice on the CI runner with `Failed(Timeout)`).
    /// The cause: a lookup sends this thread — the windows' thread — dozens of messages
    /// (`WM_GETOBJECT` and the Win32 proxies' queries), and the test answered them only between
    /// fixed 5 ms sleeps. Here a sleep is 5.5 ms (some program raised the timer resolution) and a
    /// lookup took about 200 ms; with Windows' default 15.6 ms resolution, as on the runner, it
    /// exceeded the 300 ms production [`BUDGET`]. The test now waits for messages
    /// (`MsgWaitForMultipleObjects`, as the overlay's message loop does; a lookup takes about
    /// 15 ms), waits until its windows are realized, warms UI Automation up with one lookup (the
    /// first loads its client side and the window's provider), and gives every lookup a generous
    /// budget of its own. The production budget is unchanged (a timeout there falls back to the
    /// window, Step 52).
    #[test]
    fn ui_automation_finds_a_child_control_of_our_own_window_and_reads_no_text() {
        crate::win::per_monitor_dpi_awareness();
        let top = Own::new(Rect::new(X, Y + 600, 400, 300), 0, false);
        let button = wide("BUTTON");
        let mut kids = Vec::new();
        for (i, y) in [20, 120].into_iter().enumerate() {
            let text = wide(format!("NOEUL-SECRET-BUTTON-{i}"));
            // SAFETY: a standard child control of our own window.
            let h = unsafe {
                CreateWindowExW(
                    0,
                    button.as_ptr(),
                    text.as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    20,
                    y,
                    160,
                    60,
                    top.0,
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                )
            };
            assert!(!h.is_null());
            kids.push(h);
        }
        // Realized: every window is visible and has its frame before UI Automation is asked.
        let realized = |h: HWND| {
            // SAFETY: a plain query of our own window.
            let shown = unsafe { IsWindowVisible(h) } != 0;
            shown && frame_bounds(h).is_some()
        };
        let ready = Instant::now() + Duration::from_secs(10);
        loop {
            pump();
            if std::iter::once(top.0)
                .chain(kids.iter().copied())
                .all(realized)
            {
                break;
            }
            assert!(
                Instant::now() < ready,
                "the test windows never became visible"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let second = frame_bounds(kids[1]).unwrap();
        let window = frame_bounds(top.0).unwrap();
        let worker =
            ElementWorker::start_with_budget(Box::new(|| {}), Duration::from_secs(10)).unwrap();
        let ask = |seq: u64, at: (i32, i32)| {
            worker.ask(ElementRequest {
                seq,
                token: top.token(),
                window,
                at,
            });
            // The window's thread (this one) must answer UI Automation's messages at once, as
            // the overlay's message loop does: it waits for a message, never sleeps a fixed time.
            // (Sleeping 5 ms per poll made every lookup wait for this thread dozens of times;
            // with Windows' default 15.6 ms timer resolution that alone exceeded the budget.)
            let until = Instant::now() + Duration::from_secs(30);
            loop {
                pump();
                let got = worker.take_answers();
                if let Some(a) = got.into_iter().find(|(r, _)| r.seq == seq) {
                    return a.1;
                }
                assert!(Instant::now() < until, "no answer from UI Automation");
                // SAFETY: waits for this thread's own input or sent messages, no handles.
                unsafe {
                    MsgWaitForMultipleObjects(0, std::ptr::null(), 0, 2, QS_ALLINPUT);
                }
            }
        };
        // Warm-up: the first lookup loads UI Automation's client side and the window's provider.
        let warm = ask(1, (window.x + 350, window.y + 250));
        assert!(
            !matches!(
                warm,
                ElementAnswer::Failed(Fallback::Unavailable | Fallback::Denied)
            ),
            "UI Automation is available for our own window: {warm:?}"
        );
        let centre = (second.x + 80, second.y + 30);
        let a = ask(2, centre);
        assert_eq!(
            a,
            ElementAnswer::Found(second),
            "the button under the point"
        );
        assert!(!format!("{a:?}").contains("NOEUL-SECRET"), "no text");
        let empty = ask(3, (window.x + 350, window.y + 250));
        assert_eq!(empty, ElementAnswer::WindowOnly, "no control there");
        drop(worker);
        for k in kids {
            // SAFETY: our own child windows.
            unsafe { DestroyWindow(k) };
        }
    }

    /// A window destroyed between the enumeration and the owner query has no owner: it is never
    /// taken for "process 0" (a stale handle made `GetWindowThreadProcessId` leave the pid at 0,
    /// so a scope of `[0]` listed it as skipped — this test's sibling failed once that way while
    /// other tests' windows came and went).
    #[test]
    fn windows_that_vanish_during_the_enumeration_belong_to_no_process() {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let churn = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut n = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) || n < 50 {
                    drop(Own::new(Rect::new(X + 3000, Y, 50, 50), 0, false));
                    n += 1;
                }
                n
            })
        };
        for _ in 0..100 {
            let none = snapshot(PICTURE, &Scope::Processes(vec![0]), None);
            assert!(none.taken);
            assert_eq!(
                (none.len(), none.skipped()),
                (0, 0),
                "nothing belongs to process 0"
            );
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let made = churn.join().unwrap();
        assert!(made >= 50, "windows came and went meanwhile: {made}");
    }

    /// Inspection (NOEUL-SEC-007): this file asks UI Automation and the window manager for
    /// geometry only — no call that returns text exists in it.
    #[test]
    fn detection_never_asks_for_text() {
        let src = include_str!("detect.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "GetWindowText",
            "GetClassName",
            "InternalGetWindowText",
            "WM_GETTEXT",
            "CurrentName",
            "CachedName",
            "30005", // UIA_NamePropertyId
            "30045", // UIA_ValueValuePropertyId
            "LegacyIAccessible",
            "ValuePattern",
            "TextPattern",
            "HelpText",
            "AutomationId",
            "QueryFullProcessImageName",
        ] {
            assert!(!code.contains(forbidden), "{forbidden} must not be used");
        }
    }
}
