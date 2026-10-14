//! Windows backend.

mod autostart;
mod elevation;
mod hook;
mod inject;
mod instance;
mod tray;
mod util;
mod vsync;

use std::sync::Arc;
use std::time::Duration;

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::{Elevation, SystemServices};
use crate::shared::{engine, settings};
use crate::ui::SettingsWindow;
use instance::InstanceLock;
use util::wide;

pub fn run() {
    let elevated = elevation::is_elevated();
    let relaunched = std::env::args().any(|a| a == elevation::ELEVATED_ARG);

    // After an elevated relaunch, the old copy needs a moment to exit.
    let wait = if relaunched { Duration::from_secs(5) } else { Duration::ZERO };
    let Some(mut lock) = InstanceLock::acquire(wait) else {
        instance::signal_existing();
        return;
    };

    if !elevated && !relaunched {
        match maybe_elevate(lock) {
            Some(l) => lock = l,
            None => return, // the elevated copy takes over
        }
    }

    autostart::sync(settings::current().run_as_admin, elevated);

    let engine = engine::spawn(Box::new(inject::WheelOutput::new()), Box::new(vsync::VsyncClock::new()));
    hook::start(engine);

    let services: Arc<dyn SystemServices> = Arc::new(WindowsServices { elevated });
    let ui = SettingsWindow::new(services.clone());
    tray::run(ui, services, lock, elevated);
}

/// Restarts elevated if the user prefers it (or says so in the launch prompt).
/// Returns the lock back if we keep running unelevated.
fn maybe_elevate(lock: InstanceLock) -> Option<InstanceLock> {
    let s = settings::current();
    let remembered = s.run_as_admin;
    let wants_admin = remembered
        || (s.ask_admin_at_launch
            && match elevation::ask() {
                elevation::Choice::Restart => true,
                elevation::Choice::Continue { dont_ask_again } => {
                    if dont_ask_again {
                        settings::update_and_save(|s| s.ask_admin_at_launch = false);
                    }
                    false
                }
            });
    if !wants_admin {
        return Some(lock);
    }

    settings::update_and_save(|s| s.run_as_admin = true);
    drop(lock);
    if elevation::launch_elevated() {
        return None;
    }
    // UAC declined. If this was a fresh choice, don't remember it.
    if !remembered {
        settings::update_and_save(|s| s.run_as_admin = false);
    }
    InstanceLock::acquire(Duration::from_secs(1))
}

struct WindowsServices {
    elevated: bool,
}

impl SystemServices for WindowsServices {
    fn autostart_label(&self) -> &'static str {
        "Start with Windows"
    }

    fn autostart_enabled(&self) -> bool {
        autostart::is_enabled()
    }

    fn set_autostart(&self, enabled: bool) -> Result<(), String> {
        if enabled {
            autostart::enable(settings::current().run_as_admin && self.elevated)
        } else {
            autostart::disable()
        }
    }

    fn elevation(&self) -> Elevation {
        if self.elevated { Elevation::Elevated } else { Elevation::NotElevated }
    }

    fn run_as_admin_changed(&self) {
        autostart::sync(settings::current().run_as_admin, self.elevated);
    }

    fn restart_elevated(&self) {
        tray::post(tray::WM_RESTART_ELEVATED);
    }

    fn open_config_folder(&self) {
        let dir = settings::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let (verb, path) = (wide("open"), wide(&dir.to_string_lossy()));
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
    }

    fn quit(&self) {
        tray::post(tray::WM_QUIT_APP);
    }
}
