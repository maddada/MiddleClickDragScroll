//! MiddleClickDragScroll: hold the middle mouse button and drag to scroll,
//! with trackpad-style flick momentum.
//!
//! Layout:
//! - `shared/`   OS-independent: settings, gesture recognizer, scroll physics, icon art
//! - `platform/` one backend per OS: input hook, scroll output, frame pacing, tray, autostart
//! - `ui/`       the cross-platform settings window (egui)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod platform;
mod shared;
mod ui;

fn main() {
    shared::log::install_panic_hook();
    shared::settings::init(shared::settings::load());
    platform::run();
}
