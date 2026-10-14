//! Scroll output via high-resolution wheel events.
//!
//! Normally output goes through `SendInput`, the same path as a real wheel, so
//! every app handles it. With "stick to the starting area", once the pointer
//! has moved off the spot where the drag began, wheel messages are posted
//! straight to the original window, carrying the original position. Apps
//! scroll whatever is at that position, so the same scroll area keeps scrolling,
//! like a trackpad gesture. (Apps re-pick the target for plain wheel input as
//! soon as the pointer moves a few pixels.)

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::HiDpi::PhysicalToLogicalPointForPerMonitorDPI;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VK_CONTROL, VK_LBUTTON, VK_RBUTTON, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, IsWindow, PostMessageW, SPI_GETWHEELSCROLLLINES,
    SystemParametersInfoW, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WindowFromPoint,
};

use super::util::dpi_scale;
use crate::shared::engine::{ScrollOutput, Vec2};
use crate::shared::settings;

/// Tag on every event we inject, so our own hook ignores them.
pub const MARKER: usize = 0x4D43_4453;

/// Largest wheel delta per event (the message field is a signed 16-bit value).
const MAX_DELTA: i32 = 30_000;

// Key-state flags carried in a wheel message's wParam.
const MK_LBUTTON: usize = 0x1;
const MK_RBUTTON: usize = 0x2;
const MK_SHIFT: usize = 0x4;
const MK_CONTROL: usize = 0x8;

pub struct WheelOutput {
    /// Wheel units per logical pixel.
    units_per_px: f64,
    remainder_x: f64,
    remainder_y: f64,
    /// Pointer position (physical pixels) when the gesture started.
    anchor: POINT,
    /// DPI scale at the anchor.
    scale: f64,
    /// Window under the pointer when the gesture started, if latching is on.
    target: Option<isize>,
    /// Whether the target scrolls whatever is at the position a posted wheel
    /// message carries (false for UWP/WinUI, which ignore posted wheel messages).
    target_takes_posted: bool,
}

impl WheelOutput {
    pub fn new() -> Self {
        Self {
            units_per_px: 1.2,
            remainder_x: 0.0,
            remainder_y: 0.0,
            anchor: POINT { x: 0, y: 0 },
            scale: 1.0,
            target: None,
            target_takes_posted: false,
        }
    }
}

impl ScrollOutput for WheelOutput {
    fn begin(&mut self) {
        // Browsers and most modern apps scroll `lines × 100/3` px per 120-unit notch.
        // Normalising by the system setting makes 1 px of drag ≈ 1 px of content.
        let mut lines: u32 = 3;
        unsafe { SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, (&raw mut lines).cast(), 0) };
        if lines == 0 || lines > 100 {
            lines = 3; // 0 = disabled, u32::MAX = "one screen at a time"
        }
        self.units_per_px = 120.0 / (lines as f64 * 100.0 / 3.0);
        self.remainder_x = 0.0;
        self.remainder_y = 0.0;

        self.anchor = cursor_pos();
        self.scale = dpi_scale(self.anchor);
        self.target = if settings::current().latch_target {
            Some(unsafe { WindowFromPoint(self.anchor) } as isize).filter(|&h| h != 0)
        } else {
            None
        };
        self.target_takes_posted = self.target.is_some_and(|t| !is_xaml_window(t as HWND));
    }

    fn scroll(&mut self, x: f64, y: f64, _momentum: bool) -> bool {
        // Positive WM_MOUSEWHEEL scrolls up; positive WM_MOUSEHWHEEL scrolls right.
        self.remainder_y -= y * self.units_per_px;
        self.remainder_x += x * self.units_per_px;
        let wheel_y = take_whole(&mut self.remainder_y);
        let wheel_x = take_whole(&mut self.remainder_x);
        if wheel_x == 0 && wheel_y == 0 {
            return true;
        }

        match self.target {
            Some(target) if self.aim_at_anchor(target) => {
                let hwnd = target as HWND;
                if unsafe { IsWindow(hwnd) } == 0 {
                    return false; // the window is gone; nothing left to scroll
                }
                post_wheel(hwnd, self.anchor, WM_MOUSEWHEEL, wheel_y);
                post_wheel(hwnd, self.anchor, WM_MOUSEHWHEEL, wheel_x);
            }
            _ => {
                let mut inputs = Vec::with_capacity(2);
                push_wheel(&mut inputs, MOUSEEVENTF_WHEEL, wheel_y);
                push_wheel(&mut inputs, MOUSEEVENTF_HWHEEL, wheel_x);
                send(&inputs);
            }
        }
        true
    }

    fn pointer_position(&mut self) -> Option<Vec2> {
        let pt = cursor_pos();
        Some(Vec2::new(pt.x as f64 / self.scale, pt.y as f64 / self.scale))
    }

    fn middle_click(&mut self) {
        send(&[mouse_input(MOUSEEVENTF_MIDDLEDOWN, 0), mouse_input(MOUSEEVENTF_MIDDLEUP, 0)]);
    }
}

impl WheelOutput {
    /// Whether this update has to go straight to the latched window: the pointer
    /// is over another window, or it has drifted off the starting spot in an app
    /// that would otherwise re-target to whatever is under the pointer now.
    fn aim_at_anchor(&self, target: isize) -> bool {
        let cursor = cursor_pos();
        let drifted = cursor.x != self.anchor.x || cursor.y != self.anchor.y;
        (drifted && self.target_takes_posted) || unsafe { WindowFromPoint(cursor) } as isize != target
    }
}

/// UWP, WinUI and XAML-island windows only take wheel input from the real input
/// stack and ignore posted wheel messages.
fn is_xaml_window(hwnd: HWND) -> bool {
    const MARKERS: [&str; 5] =
        ["Windows.UI.", "Microsoft.UI.", "ApplicationFrameWindow", "WinUIDesktopWin32WindowClass", "Xaml"];
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    [hwnd, root].into_iter().any(|h| {
        let mut buf = [0u16; 256];
        let len = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
        let class = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
        MARKERS.iter().any(|m| class.contains(m))
    })
}

fn take_whole(remainder: &mut f64) -> i32 {
    let whole = remainder.trunc();
    *remainder -= whole;
    whole as i32
}

fn chunks(mut delta: i32) -> impl Iterator<Item = i32> {
    std::iter::from_fn(move || {
        (delta != 0).then(|| {
            let part = delta.clamp(-MAX_DELTA, MAX_DELTA);
            delta -= part;
            part
        })
    })
}

fn push_wheel(inputs: &mut Vec<INPUT>, flag: MOUSE_EVENT_FLAGS, delta: i32) {
    inputs.extend(chunks(delta).map(|part| mouse_input(flag, part)));
}

/// Posts a wheel message as if the pointer were still at `anchor`, so the
/// app scrolls the same element the drag started on.
fn post_wheel(hwnd: HWND, anchor: POINT, msg: u32, delta: i32) {
    let mut pt = anchor;
    // Posted messages aren't DPI-virtualised for the receiver, so convert ourselves.
    unsafe { PhysicalToLogicalPointForPerMonitorDPI(hwnd, &mut pt) };
    let lparam = ((pt.y as u16 as u32) << 16 | pt.x as u16 as u32) as isize;
    let keys = key_state();
    for part in chunks(delta) {
        let wparam = ((part as i16 as u16 as usize) << 16) | keys;
        unsafe { PostMessageW(hwnd, msg, wparam, lparam) };
    }
}

fn key_state() -> usize {
    let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } < 0;
    let mut keys = 0;
    for (vk, flag) in [(VK_CONTROL, MK_CONTROL), (VK_SHIFT, MK_SHIFT), (VK_LBUTTON, MK_LBUTTON), (VK_RBUTTON, MK_RBUTTON)] {
        if down(vk) {
            keys |= flag;
        }
    }
    keys
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: MARKER },
        },
    }
}

fn send(inputs: &[INPUT]) {
    if !inputs.is_empty() {
        unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    }
}

fn cursor_pos() -> POINT {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pt) };
    pt
}

