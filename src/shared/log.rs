//! Minimal append-only log file next to the settings (the app has no console).

use std::io::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use super::settings::config_dir;

pub fn write(message: &str) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("log.txt");

    // Keep the log from growing forever.
    if std::fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }

    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{secs}] {message}");
    }
    #[cfg(debug_assertions)]
    eprintln!("{message}");
}

/// Logs panics to the file; a tray app has nowhere else to show them.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        write(&format!("panic on thread {:?}: {info}", thread.name().unwrap_or("?")));
        default(info);
    }));
}
