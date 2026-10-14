//! Single-instance lock. A second launch asks the running copy to open its settings.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

use super::tray;
use super::util::wide;

const MUTEX_NAME: &str = "Local\\MiddleClickDragScroll.SingleInstance";

pub struct InstanceLock(HANDLE);

impl InstanceLock {
    /// Tries to become the running instance, retrying for up to `wait`
    /// (used by the elevated relaunch while the old copy shuts down).
    pub fn acquire(wait: Duration) -> Option<Self> {
        let name = wide(MUTEX_NAME);
        let deadline = Instant::now() + wait;
        loop {
            unsafe {
                let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
                // An elevated owner makes this fail with access denied instead of
                // ALREADY_EXISTS; both mean someone else holds it.
                if !handle.is_null() && GetLastError() != ERROR_ALREADY_EXISTS {
                    return Some(Self(handle));
                }
                if !handle.is_null() {
                    CloseHandle(handle);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Asks an already-running instance to show its settings window.
pub fn signal_existing() {
    let class = wide(tray::CLASS_NAME);
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), std::ptr::null());
        if !hwnd.is_null() {
            PostMessageW(hwnd, tray::WM_SHOW_SETTINGS, 0, 0);
        }
    }
}
