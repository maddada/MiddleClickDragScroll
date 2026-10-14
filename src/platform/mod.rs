//! OS-specific backends.
//!
//! Each backend provides:
//! - `run()`: owns the main thread (tray icon, input hook, engine, settings window)
//!   and returns when the app quits.
//! - an input hook that feeds [`crate::shared::gesture::Gesture`] and can swallow events,
//! - a [`crate::shared::engine::ScrollOutput`] and [`crate::shared::engine::FrameClock`],
//! - a [`SystemServices`] implementation for the settings window.
//!
//! See `README.md` for notes on the macOS and Linux ports.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::run;

#[cfg(not(windows))]
pub fn run() {
    eprintln!("{} doesn't support this OS yet (Windows only for now).", crate::shared::settings::APP_NAME);
    std::process::exit(1);
}

/// Whether the app runs with elevated rights (admin/root), where that matters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Elevation {
    /// The platform has no such concept (or it doesn't affect the app).
    NotApplicable,
    Elevated,
    NotElevated,
}

/// OS integration the shared settings window needs.
pub trait SystemServices: Send + Sync {
    /// Label for the launch-at-login toggle, e.g. "Start with Windows".
    fn autostart_label(&self) -> &'static str;
    fn autostart_enabled(&self) -> bool;
    fn set_autostart(&self, enabled: bool) -> Result<(), String>;

    fn elevation(&self) -> Elevation;
    /// Called after the "run as administrator" preference changes.
    fn run_as_admin_changed(&self) {}
    /// Restarts the app with elevated rights (shows the OS prompt).
    fn restart_elevated(&self) {}

    fn open_config_folder(&self);
    fn quit(&self);
}
