//! "Start with Windows".
//!
//! Normally a value under `HKCU\...\Run`. When the user prefers running as
//! administrator, a Run entry would trigger a UAC prompt at every sign-in, so
//! a scheduled task with "highest privileges" is used instead (no prompt).

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use super::util::wide;
use crate::shared::settings::APP_NAME;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 0 = unknown, 1 = off, 2 = on. Checking the task means spawning schtasks, so cache it.
static CACHED: AtomicU8 = AtomicU8::new(0);

pub fn is_enabled() -> bool {
    match CACHED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = run_value_exists() || task_exists();
            CACHED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// Turns autostart on, as an elevated scheduled task or a plain Run entry.
pub fn enable(as_admin_task: bool) -> Result<(), String> {
    let result = if as_admin_task {
        create_task().map(|_| {
            let _ = delete_run_value();
        })
    } else {
        set_run_value().map(|_| {
            if task_exists() {
                let _ = delete_task(); // needs admin; harmless if it fails
            }
        })
    };
    CACHED.store(0, Ordering::Relaxed);
    result
}

pub fn disable() -> Result<(), String> {
    let _ = delete_run_value();
    let result = if task_exists() {
        delete_task().map_err(|_| {
            "Couldn't remove the administrator startup task. Restart as administrator and try again.".to_string()
        })
    } else {
        Ok(())
    };
    CACHED.store(0, Ordering::Relaxed);
    result
}

/// Startup reconciliation: keeps the entry pointing at this exe and uses the
/// mechanism that matches the "run as administrator" preference.
pub fn sync(run_as_admin: bool, elevated: bool) {
    if !is_enabled() {
        return;
    }
    if run_as_admin && elevated {
        let _ = enable(true);
    } else if !run_as_admin && (elevated || !task_exists()) {
        let _ = enable(false);
    } else if run_value_exists() {
        let _ = set_run_value(); // refresh the path in case the exe moved
    }
}

fn exe_path() -> Result<String, String> {
    std::env::current_exe().map(|p| p.display().to_string()).map_err(|e| e.to_string())
}

fn run_value_exists() -> bool {
    let (key, name) = (wide(RUN_KEY), wide(APP_NAME));
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    status == ERROR_SUCCESS
}

fn set_run_value() -> Result<(), String> {
    let (key, name) = (wide(RUN_KEY), wide(APP_NAME));
    let data = wide(&format!("\"{}\"", exe_path()?));
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("Couldn't write the startup entry (error {status}).")) }
}

fn delete_run_value() -> Result<(), String> {
    let (key, name) = (wide(RUN_KEY), wide(APP_NAME));
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("error {status}")) }
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let output = Command::new("schtasks.exe")
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn task_exists() -> bool {
    schtasks(&["/Query", "/TN", APP_NAME]).is_ok()
}

fn delete_task() -> Result<(), String> {
    schtasks(&["/Delete", "/TN", APP_NAME, "/F"])
}

fn create_task() -> Result<(), String> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (_, Ok(name)) => name,
        _ => return Err("Couldn't determine the current user.".into()),
    };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts {app} as administrator when you sign in.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
    </Exec>
  </Actions>
</Task>
"#,
        app = APP_NAME,
        user = xml_escape(&user),
        exe = xml_escape(&exe_path()?),
    );

    // schtasks wants the file's encoding to match the declaration: UTF-16 LE with BOM.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    let path = std::env::temp_dir().join(format!("{APP_NAME}-task.xml"));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let result = schtasks(&["/Create", "/TN", APP_NAME, "/XML", &path.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&path);
    result.map_err(|e| format!("Couldn't create the startup task: {e}"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
