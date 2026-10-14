//! Low-level mouse hook: feeds the shared gesture recognizer and swallows the
//! middle button (and, with "freeze cursor", pointer motion) during a drag-scroll.
//!
//! Motion deltas are measured against the last position the cursor actually
//! reached: when a move is swallowed the cursor stays put, so the reference
//! point only advances for moves that pass through.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;

use windows_sys::Win32::Foundation::{CloseHandle, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GA_ROOT, GetAncestor, GetMessageW, GetWindowThreadProcessId, HC_ACTION, MSG,
    MSLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_XBUTTONDOWN, WindowFromPoint,
};

use super::inject;
use super::util::dpi_scale;
use crate::shared::engine::EngineHandle;
use crate::shared::gesture::{Gesture, Input, Verdict, motion_verdict};
use crate::shared::{log, settings};

static ENGINE: OnceLock<EngineHandle> = OnceLock::new();

struct HookState {
    gesture: Gesture,
    /// Where the cursor actually is (motion is measured from here).
    last: POINT,
    /// Physical pixels per logical pixel on the anchor's monitor.
    scale: f64,
    /// Last process looked up for the exclusion list.
    cached_pid: u32,
    cached_exe: String,
}

thread_local! {
    static STATE: RefCell<HookState> = const {
        RefCell::new(HookState {
            gesture: Gesture::new(),
            last: POINT { x: 0, y: 0 },
            scale: 1.0,
            cached_pid: 0,
            cached_exe: String::new(),
        })
    };
}

/// Installs the hook on a dedicated high-priority thread.
pub fn start(engine: EngineHandle) {
    let _ = ENGINE.set(engine);
    std::thread::Builder::new()
        .name("mouse-hook".into())
        .spawn(|| unsafe {
            // Windows silently drops low-level hooks that respond too slowly, so
            // this thread does nothing else and runs at the highest priority.
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), GetModuleHandleW(std::ptr::null()), 0);
            if hook.is_null() {
                log::write("SetWindowsHookExW(WH_MOUSE_LL) failed");
                return;
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
            UnhookWindowsHookEx(hook);
        })
        .expect("failed to spawn mouse hook thread");
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        // Ignore the wheel/click events we inject ourselves.
        if info.dwExtraInfo != inject::MARKER && on_event(wparam as u32, info) == Verdict::Swallow {
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn on_event(msg: u32, info: &MSLLHOOKSTRUCT) -> Verdict {
    STATE.with_borrow_mut(|st| {
        let input = match msg {
            WM_MOUSEMOVE => {
                if !st.gesture.is_capturing() {
                    return Verdict::Pass;
                }
                let dx = (info.pt.x - st.last.x) as f64 / st.scale;
                let dy = (info.pt.y - st.last.y) as f64 / st.scale;
                if dx == 0.0 && dy == 0.0 {
                    return motion_verdict(&settings::current());
                }
                Input::Motion { dx, dy }
            }
            WM_MBUTTONDOWN => {
                st.last = info.pt;
                st.scale = dpi_scale(info.pt);
                let s = settings::current();
                let excluded = s.enabled && !s.excluded_apps.is_empty() && st.is_excluded(info.pt, &s.excluded_apps);
                Input::TriggerDown { excluded }
            }
            WM_MBUTTONUP => Input::TriggerUp,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_XBUTTONDOWN => Input::OtherButtonDown,
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => Input::Wheel,
            _ => return Verdict::Pass,
        };

        let Some(engine) = ENGINE.get() else { return Verdict::Pass };
        let s = settings::current();
        let verdict = st.gesture.handle(input, &s, Instant::now(), &mut |command| engine.send(command));
        if msg == WM_MOUSEMOVE && verdict == Verdict::Pass {
            st.last = info.pt;
        }
        verdict
    })
}

impl HookState {
    fn is_excluded(&mut self, pt: POINT, excluded: &[String]) -> bool {
        let Some(exe) = self.exe_under(pt) else { return false };
        excluded.iter().any(|e| e == exe || exe.strip_suffix(".exe") == Some(e.as_str()))
    }

    /// Lower-case executable name of the top-level window under `pt`.
    fn exe_under(&mut self, pt: POINT) -> Option<&str> {
        unsafe {
            let hwnd = WindowFromPoint(pt);
            if hwnd.is_null() {
                return None;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(GetAncestor(hwnd, GA_ROOT), &mut pid);
            if pid == 0 {
                return None;
            }
            if pid != self.cached_pid {
                let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if process.is_null() {
                    return None;
                }
                let mut buf = [0u16; 1024];
                let mut len = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
                CloseHandle(process);
                if ok == 0 {
                    return None;
                }
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                self.cached_exe = path.rsplit(['\\', '/']).next().unwrap_or_default().to_ascii_lowercase();
                self.cached_pid = pid;
            }
            Some(&self.cached_exe)
        }
    }
}
