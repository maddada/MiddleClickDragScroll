//! Notification-area icon, its menu, and the hidden window that owns them.
//! Runs the main thread's message loop.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, ChangeWindowMessageFilterEx, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, KillTimer,
    MB_ICONWARNING, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, MSGFLT_ALLOW, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, SetMenuDefaultItem,
    SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_CONTEXTMENU, WM_DESTROY, WM_NULL, WM_TIMER, WNDCLASSEXW, WS_OVERLAPPED,
};

use super::elevation;
use super::instance::InstanceLock;
use super::util::{copy_wide, wide};
use crate::platform::SystemServices;
use crate::shared::{icon, settings};
use crate::ui::SettingsWindow;

pub const CLASS_NAME: &str = "MiddleClickDragScroll.Tray";

const WM_TRAY: u32 = WM_APP + 1;
pub const WM_SHOW_SETTINGS: u32 = WM_APP + 2;
const WM_REFRESH: u32 = WM_APP + 3;
pub const WM_RESTART_ELEVATED: u32 = WM_APP + 4;
pub const WM_QUIT_APP: u32 = WM_APP + 5;

const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const RETRY_TIMER: usize = 1;

const ID_ENABLED: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_AUTOSTART: usize = 3;
const ID_RESTART_ADMIN: usize = 4;
const ID_QUIT: usize = 5;

static TRAY_HWND: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static ICON_ON: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ICON_OFF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

struct Tray {
    ui: Arc<SettingsWindow>,
    services: Arc<dyn SystemServices>,
    instance: Option<InstanceLock>,
    elevated: bool,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// Posts a message to the tray window from any thread.
pub fn post(msg: u32) {
    let hwnd = TRAY_HWND.load(Ordering::Acquire);
    if !hwnd.is_null() {
        unsafe { PostMessageW(hwnd, msg, 0, 0) };
    }
}

/// Creates the tray icon and runs the message loop until the app quits.
pub fn run(ui: Arc<SettingsWindow>, services: Arc<dyn SystemServices>, instance: InstanceLock, elevated: bool) {
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let class = wide(CLASS_NAME);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        TRAY_HWND.store(hwnd, Ordering::Release);

        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        TASKBAR_CREATED.store(taskbar_created, Ordering::Relaxed);
        // When elevated, UIPI would otherwise block these from non-elevated senders
        // (Explorer restarting, or a second launch asking us to show settings).
        ChangeWindowMessageFilterEx(hwnd, taskbar_created, MSGFLT_ALLOW, std::ptr::null_mut());
        ChangeWindowMessageFilterEx(hwnd, WM_SHOW_SETTINGS, MSGFLT_ALLOW, std::ptr::null_mut());

        let size = GetSystemMetrics(SM_CXSMICON).max(16) as u32;
        ICON_ON.store(make_icon(size, true), Ordering::Relaxed);
        ICON_OFF.store(make_icon(size, false), Ordering::Relaxed);

        TRAY.set(Some(Tray { ui, services, instance: Some(instance), elevated }));

        if !add_icon(hwnd) {
            // Explorer may not be ready yet right after sign-in.
            SetTimer(hwnd, RETRY_TIMER, 2000, None);
        }
        settings::on_change(|| post(WM_REFRESH));

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    settings::save();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 & 0xFFFF {
                NIN_SELECT | NIN_KEYSELECT => settings::update_and_save(|s| s.enabled = !s.enabled),
                WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_SHOW_SETTINGS => {
            if let Some(ui) = TRAY.with_borrow(|t| t.as_ref().map(|t| t.ui.clone())) {
                ui.show();
            }
            0
        }
        WM_REFRESH => {
            let nid = notify_data(hwnd);
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
            0
        }
        WM_RESTART_ELEVATED => {
            restart_elevated(hwnd);
            0
        }
        WM_QUIT_APP => {
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_TIMER if wparam == RETRY_TIMER => {
            if add_icon(hwnd) {
                unsafe { KillTimer(hwnd, RETRY_TIMER) };
            }
            0
        }
        WM_DESTROY => {
            let nid = notify_data(hwnd);
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &nid);
                PostQuitMessage(0);
            }
            0
        }
        _ if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            add_icon(hwnd);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let s = settings::current();
    let elevated = TRAY.with_borrow(|t| t.as_ref().is_some_and(|t| t.elevated));
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = if s.enabled { ICON_ON.load(Ordering::Relaxed) } else { ICON_OFF.load(Ordering::Relaxed) };
    let state = if s.enabled { "on" } else { "off" };
    let admin = if elevated { " (admin)" } else { "" };
    copy_wide(&mut nid.szTip, &format!("{}: {state}{admin}", settings::APP_NAME));
    nid
}

fn add_icon(hwnd: HWND) -> bool {
    let mut nid = notify_data(hwnd);
    unsafe {
        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
            return false;
        }
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
    if !settings::current().welcomed {
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        copy_wide(&mut nid.szInfoTitle, &format!("{} is running", settings::APP_NAME));
        copy_wide(
            &mut nid.szInfo,
            "Hold the middle mouse button and drag to scroll, flick to glide. \
             Click this icon to turn it on or off, right-click for settings.",
        );
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
        settings::update_and_save(|s| s.welcomed = true);
    }
    true
}

fn show_menu(hwnd: HWND) {
    let Some((services, elevated)) = TRAY.with_borrow(|t| t.as_ref().map(|t| (t.services.clone(), t.elevated))) else {
        return;
    };
    let enabled = settings::current().enabled;
    let autostart = services.autostart_enabled();

    let command = unsafe {
        let menu = CreatePopupMenu();
        let add = |id: usize, text: &str, checked: bool| {
            let text = wide(text);
            AppendMenuW(menu, MF_STRING | if checked { MF_CHECKED } else { 0 }, id, text.as_ptr());
        };
        add(ID_ENABLED, "Enabled", enabled);
        add(ID_SETTINGS, "Settings…", false);
        SetMenuDefaultItem(menu, ID_SETTINGS as u32, 0);
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        add(ID_AUTOSTART, services.autostart_label(), autostart);
        if !elevated {
            add(ID_RESTART_ADMIN, "Restart as administrator", false);
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        add(ID_QUIT, "Quit", false);

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required so the menu closes when clicking elsewhere.
        SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, std::ptr::null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        command as usize
    };

    match command {
        ID_ENABLED => settings::update_and_save(|s| s.enabled = !s.enabled),
        ID_SETTINGS => unsafe {
            PostMessageW(hwnd, WM_SHOW_SETTINGS, 0, 0);
        },
        ID_AUTOSTART => {
            if let Err(e) = services.set_autostart(!autostart) {
                let (text, caption) = (wide(&e), wide(settings::APP_NAME));
                unsafe { MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), MB_ICONWARNING) };
            }
        }
        ID_RESTART_ADMIN => restart_elevated(hwnd),
        ID_QUIT => unsafe {
            DestroyWindow(hwnd);
        },
        _ => {}
    }
}

/// Remembers the preference, hands the single-instance lock to an elevated copy and exits.
fn restart_elevated(hwnd: HWND) {
    let previous = settings::current().run_as_admin;
    settings::update_and_save(|s| s.run_as_admin = true);
    drop(TRAY.with_borrow_mut(|t| t.as_mut().and_then(|t| t.instance.take())));

    if elevation::launch_elevated() {
        unsafe { DestroyWindow(hwnd) };
        return;
    }
    // UAC was declined: keep running as we were.
    settings::update_and_save(|s| s.run_as_admin = previous);
    let lock = InstanceLock::acquire(Duration::ZERO);
    TRAY.with_borrow_mut(|t| {
        if let Some(t) = t.as_mut() {
            t.instance = lock;
        }
    });
}

fn make_icon(size: u32, active: bool) -> HICON {
    let rgba = icon::render_rgba(size, active);
    let bgra: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    // The 32-bit colour data carries alpha, so the AND mask is all zeros.
    let mask = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
    unsafe {
        CreateIcon(
            GetModuleHandleW(std::ptr::null()),
            size as i32,
            size as i32,
            1,
            32,
            mask.as_ptr(),
            bgra.as_ptr(),
        )
    }
}
