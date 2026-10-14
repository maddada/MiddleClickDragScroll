//! Running as administrator: detection, the launch prompt, and relaunching elevated.
//!
//! Without elevation, Windows (UIPI) blocks hooks and injected input for
//! windows that run as administrator, so drag-scrolling wouldn't work there.

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOGCONFIG_1,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_USE_COMMAND_LINKS, TDM_SET_BUTTON_ELEVATION_REQUIRED_STATE, TDN_CREATED,
    TD_SHIELD_ICON, TaskDialogIndirect,
};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWNORMAL, SendMessageW};
use windows_sys::core::{BOOL, HRESULT};

use super::util::wide;
use crate::shared::settings::APP_NAME;

/// Passed to the elevated copy so it waits for this instance to exit.
pub const ELEVATED_ARG: &str = "--elevated-restart";

const ID_RESTART: i32 = 100;
const ID_CONTINUE: i32 = 101;

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Starts an elevated copy of this exe. Returns false if the UAC prompt was declined.
pub fn launch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = wide(&exe.to_string_lossy());
    let verb = wide("runas");
    let params = wide(ELEVATED_ARG);
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = exe.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        ShellExecuteExW(&mut info) != 0
    }
}

pub enum Choice {
    Restart,
    Continue { dont_ask_again: bool },
}

/// The launch-time "Run as administrator?" dialog.
pub fn ask() -> Choice {
    let title = wide(APP_NAME);
    let instruction = wide("Run as administrator?");
    let content = wide(
        "Windows doesn't let regular apps scroll windows that are running as administrator, \
         like Task Manager, Registry Editor or admin terminals.\n\n\
         Restart as administrator so drag-scrolling works everywhere.",
    );
    let restart = wide(
        "Restart as administrator\n\
         Windows will ask for permission. This is remembered, so it starts as admin from now on.",
    );
    let skip = wide("Continue without admin\nDrag-scrolling works everywhere except in admin windows.");
    let verification = wide("Don't ask again");
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: ID_RESTART, pszButtonText: restart.as_ptr() },
        TASKDIALOG_BUTTON { nButtonID: ID_CONTINUE, pszButtonText: skip.as_ptr() },
    ];

    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: std::ptr::null_mut(),
        hInstance: std::ptr::null_mut(),
        dwFlags: TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: 0,
        pszWindowTitle: title.as_ptr(),
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: TD_SHIELD_ICON },
        pszMainInstruction: instruction.as_ptr(),
        pszContent: content.as_ptr(),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: ID_RESTART,
        cRadioButtons: 0,
        pRadioButtons: std::ptr::null(),
        nDefaultRadioButton: 0,
        pszVerificationText: verification.as_ptr(),
        pszExpandedInformation: std::ptr::null(),
        pszExpandedControlText: std::ptr::null(),
        pszCollapsedControlText: std::ptr::null(),
        Anonymous2: TASKDIALOGCONFIG_1 { pszFooterIcon: std::ptr::null() },
        pszFooter: std::ptr::null(),
        pfCallback: Some(dialog_callback),
        lpCallbackData: 0,
        cxWidth: 0,
    };

    let mut button = 0;
    let mut checked: BOOL = 0;
    let hr = unsafe { TaskDialogIndirect(&config, &mut button, std::ptr::null_mut(), &mut checked) };
    if hr >= 0 && button == ID_RESTART {
        Choice::Restart
    } else {
        Choice::Continue { dont_ask_again: hr >= 0 && checked != 0 }
    }
}

unsafe extern "system" fn dialog_callback(hwnd: HWND, msg: u32, _: WPARAM, _: LPARAM, _: isize) -> HRESULT {
    if msg == TDN_CREATED as u32 {
        // Show the UAC shield on the restart button.
        unsafe { SendMessageW(hwnd, TDM_SET_BUTTON_ELEVATION_REQUIRED_STATE as u32, ID_RESTART as usize, 1) };
    }
    0
}
