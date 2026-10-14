//! Frame pacing: waits for the vertical blank of the monitor under the pointer,
//! so each refresh gets exactly one scroll update (no judder).
//! Falls back to a high-resolution sleep at the monitor's refresh rate.

use std::time::{Duration, Instant};

use windows_sys::Wdk::Graphics::Direct3D::{
    D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMHDC, D3DKMT_WAITFORVERTICALBLANKEVENT, D3DKMTCloseAdapter,
    D3DKMTOpenAdapterFromHdc, D3DKMTWaitForVerticalBlankEvent,
};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{
    CreateDCW, DEVMODEW, DeleteDC, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
};
use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::shared::engine::FrameClock;

pub struct VsyncClock {
    /// (kernel adapter handle, video source id)
    adapter: Option<(u32, u32)>,
    device: [u16; 32],
    period: Duration,
    /// Consecutive waits that returned suspiciously fast (display asleep etc).
    instant_returns: u32,
}

impl VsyncClock {
    pub fn new() -> Self {
        Self { adapter: None, device: [0; 32], period: Duration::from_micros(16_667), instant_returns: 0 }
    }

    fn close(&mut self) {
        if let Some((adapter, _)) = self.adapter.take() {
            unsafe { D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: adapter }) };
        }
    }
}

impl FrameClock for VsyncClock {
    fn prepare_thread(&mut self) {
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    }

    fn retarget(&mut self) {
        unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFOEXW = std::mem::zeroed();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) == 0 {
                return;
            }
            if self.adapter.is_some() && info.szDevice == self.device {
                return;
            }
            self.close();
            self.device = info.szDevice;

            let mut mode: DEVMODEW = std::mem::zeroed();
            mode.dmSize = size_of::<DEVMODEW>() as u16;
            if EnumDisplaySettingsW(self.device.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) != 0
                && mode.dmDisplayFrequency > 1
            {
                self.period = Duration::from_secs_f64(1.0 / mode.dmDisplayFrequency as f64);
            }

            let hdc = CreateDCW(self.device.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null());
            if hdc.is_null() {
                return;
            }
            let mut open = D3DKMT_OPENADAPTERFROMHDC { hDc: hdc, ..std::mem::zeroed() };
            let status = D3DKMTOpenAdapterFromHdc(&mut open);
            DeleteDC(hdc);
            if status >= 0 {
                self.adapter = Some((open.hAdapter, open.VidPnSourceId));
                self.instant_returns = 0;
            }
        }
    }

    fn wait(&mut self) {
        if let Some((adapter, source)) = self.adapter {
            let start = Instant::now();
            let event = D3DKMT_WAITFORVERTICALBLANKEVENT { hAdapter: adapter, hDevice: 0, VidPnSourceId: source };
            if unsafe { D3DKMTWaitForVerticalBlankEvent(&event) } >= 0 {
                if start.elapsed() > Duration::from_micros(500) {
                    self.instant_returns = 0;
                    return;
                }
                // Landing right on a vblank happens occasionally; doing it every time
                // means the display isn't really pacing us.
                self.instant_returns += 1;
                if self.instant_returns < 3 {
                    return;
                }
            }
            self.close();
        }
        std::thread::sleep(self.period);
    }
}

impl Drop for VsyncClock {
    fn drop(&mut self) {
        self.close();
    }
}
