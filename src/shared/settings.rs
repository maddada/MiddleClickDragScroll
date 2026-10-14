//! User-tunable settings, their slider metadata, and persistence.
//!
//! Everything here is OS-independent. The live settings are held in a global
//! `Arc<Settings>` that any thread can cheaply snapshot with [`current`].

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, RwLock};

pub const APP_NAME: &str = "MiddleClickDragScroll";

/// `glide_cancel_distance` at (or above) this means "never stop the glide".
pub const GLIDE_CANCEL_NEVER: f64 = 5000.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub natural_direction: bool,
    pub axis_lock: bool,
    pub freeze_cursor: bool,
    pub latch_target: bool,

    // Dragging
    pub speed: f64,
    pub drag_acceleration: f64,
    pub axis_lock_ratio: f64,
    pub click_slop: f64,

    // Flick detection
    pub flick_window_ms: f64,
    pub release_hold_ms: f64,
    pub min_flick_speed: f64,
    pub flick_gain: f64,
    pub flick_stacking: f64,

    // Glide physics
    pub friction: f64,
    pub tail_brake: f64,
    pub stop_speed: f64,
    pub max_speed: f64,
    pub glide_cancel_distance: f64,

    /// Executable names (e.g. `blender.exe`) where the middle button is left untouched.
    pub excluded_apps: Vec<String>,

    // Launch preferences. Their meaning is platform specific; backends that have
    // no such concept simply ignore them.
    pub run_as_admin: bool,
    pub ask_admin_at_launch: bool,
    pub welcomed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            natural_direction: true,
            axis_lock: true,
            freeze_cursor: false,
            latch_target: true,

            speed: 1.0,
            drag_acceleration: 0.3,
            axis_lock_ratio: 2.0,
            click_slop: 6.0,

            flick_window_ms: 60.0,
            release_hold_ms: 50.0,
            min_flick_speed: 200.0,
            flick_gain: 1.0,
            flick_stacking: 0.8,

            friction: 2.2,
            tail_brake: 250.0,
            stop_speed: 15.0,
            max_speed: 12000.0,
            glide_cancel_distance: GLIDE_CANCEL_NEVER,

            excluded_apps: Vec::new(),

            run_as_admin: false,
            ask_admin_at_launch: true,
            welcomed: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Slider / toggle metadata, so every platform's UI (and the config file) can be
// generated from one table.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Drag,
    Flick,
    Glide,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Drag, Section::Flick, Section::Glide];

    pub fn title(self) -> &'static str {
        match self {
            Section::Drag => "Dragging",
            Section::Flick => "Flick detection",
            Section::Glide => "Glide (momentum)",
        }
    }
}

pub struct NumSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub section: Section,
    pub min: f64,
    pub max: f64,
    pub decimals: usize,
    pub unit: &'static str,
    pub logarithmic: bool,
    /// Shown instead of the number when the slider is at its maximum (e.g. "never").
    pub max_label: Option<&'static str>,
    pub get: fn(&Settings) -> f64,
    pub set: fn(&mut Settings, f64),
}

pub struct BoolSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub get: fn(&Settings) -> bool,
    pub set: fn(&mut Settings, bool),
}

macro_rules! num {
    ($field:ident, $label:expr, $help:expr, $section:ident, $min:expr, $max:expr, $dec:expr, $unit:expr, $log:expr) => {
        num!($field, $label, $help, $section, $min, $max, $dec, $unit, $log, None)
    };
    ($field:ident, $label:expr, $help:expr, $section:ident, $min:expr, $max:expr, $dec:expr, $unit:expr, $log:expr, $max_label:expr) => {
        NumSpec {
            key: stringify!($field),
            label: $label,
            help: $help,
            section: Section::$section,
            min: $min,
            max: $max,
            decimals: $dec,
            unit: $unit,
            logarithmic: $log,
            max_label: $max_label,
            get: |s| s.$field,
            set: |s, v| s.$field = v,
        }
    };
}

macro_rules! toggle {
    ($field:ident, $label:expr, $help:expr) => {
        BoolSpec {
            key: stringify!($field),
            label: $label,
            help: $help,
            get: |s| s.$field,
            set: |s, v| s.$field = v,
        }
    };
}

pub static TOGGLES: &[BoolSpec] = &[
    toggle!(enabled, "Enabled", "Master switch. Also toggled by left-clicking the tray icon."),
    toggle!(
        natural_direction,
        "Natural direction",
        "On: content follows your hand, like a trackpad. Off: dragging down scrolls down."
    ),
    toggle!(
        axis_lock,
        "Lock to one axis",
        "Each drag decides early whether it's vertical, horizontal, or free, and sticks to it."
    ),
    toggle!(
        freeze_cursor,
        "Freeze cursor while scrolling",
        "Keep the pointer in place while you drag. Off: the pointer moves with your hand, like on a Mac."
    ),
    toggle!(
        latch_target,
        "Stick to the starting area",
        "Keep scrolling the area where the drag began, during the drag and the glide, even after the \
         pointer moves to another area or window, like a trackpad. Off: scrolling goes to whatever is \
         under the pointer."
    ),
];

pub static NUMS: &[NumSpec] = &[
    num!(speed, "Scroll speed", "Content pixels moved per pixel of mouse movement.", Drag, 0.25, 5.0, 2, "×", true),
    num!(
        drag_acceleration,
        "Drag acceleration",
        "Extra gain for fast drags: speed × (1 + this × mouse speed / 1000 px/s).",
        Drag, 0.0, 2.0, 2, "", false
    ),
    num!(
        axis_lock_ratio,
        "Axis lock strength",
        "How dominant one direction must be (ratio) for a drag to lock to it.",
        Drag, 1.0, 5.0, 1, ":1", false
    ),
    num!(
        click_slop,
        "Click dead zone",
        "Movement below this still counts as a plain middle click.",
        Drag, 1.0, 30.0, 0, "px", false
    ),
    num!(
        flick_window_ms,
        "Velocity window",
        "How much of the end of a drag is averaged to measure the flick speed.",
        Flick, 15.0, 200.0, 0, "ms", false
    ),
    num!(
        release_hold_ms,
        "Stop-before-release",
        "If the mouse was still for this long before you let go, there's no glide.",
        Flick, 10.0, 300.0, 0, "ms", false
    ),
    num!(
        min_flick_speed,
        "Minimum flick speed",
        "Releases slower than this stop dead instead of gliding.",
        Flick, 0.0, 2000.0, 0, "px/s", false
    ),
    num!(
        flick_gain,
        "Flick gain",
        "Multiplier on release speed. 1.0 = the glide starts exactly as fast as you were dragging.",
        Flick, 0.25, 3.0, 2, "×", false
    ),
    num!(
        flick_stacking,
        "Flick stacking",
        "Fraction of the current glide speed added when you flick again in the same direction.",
        Flick, 0.0, 1.5, 2, "", false
    ),
    num!(
        friction,
        "Friction",
        "Exponential slow-down of the glide. Lower = longer, floatier glides.",
        Glide, 0.3, 12.0, 2, "/s", true
    ),
    num!(
        tail_brake,
        "Tail brake",
        "Constant deceleration on top of friction, so slow glides end crisply instead of crawling.",
        Glide, 0.0, 3000.0, 0, "px/s²", false
    ),
    num!(
        stop_speed,
        "Stop speed",
        "The glide ends once it's slower than this.",
        Glide, 1.0, 200.0, 0, "px/s", true
    ),
    num!(
        max_speed,
        "Max glide speed",
        "Upper limit for glide speed, including stacked flicks.",
        Glide, 1000.0, 40000.0, 0, "px/s", true
    ),
    num!(
        glide_cancel_distance,
        "Stop glide on pointer move",
        "Moving the pointer this far from where you let go stops the glide. \
         All the way right = never, like a trackpad.",
        Glide, 10.0, GLIDE_CANCEL_NEVER, 0, "px", true, Some("never")
    ),
];

// ---------------------------------------------------------------------------
// Live, shared settings
// ---------------------------------------------------------------------------

static CURRENT: LazyLock<RwLock<Arc<Settings>>> = LazyLock::new(|| RwLock::new(Arc::new(Settings::default())));

type Listener = Box<dyn Fn() + Send + Sync>;
static LISTENERS: Mutex<Vec<Listener>> = Mutex::new(Vec::new());

/// Cheap snapshot of the live settings.
pub fn current() -> Arc<Settings> {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Replaces the live settings without saving or notifying (used at startup).
pub fn init(settings: Settings) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(settings);
}

/// Applies `f` to the live settings and notifies listeners if anything changed.
/// Does not save; call [`save`] when appropriate.
pub fn update(f: impl FnOnce(&mut Settings)) {
    let changed = {
        let mut guard = CURRENT.write().unwrap_or_else(|e| e.into_inner());
        let mut next = (**guard).clone();
        f(&mut next);
        let changed = next != **guard;
        if changed {
            *guard = Arc::new(next);
        }
        changed
    };
    if changed {
        for listener in LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            listener();
        }
    }
}

/// Like [`update`], then saves to disk.
pub fn update_and_save(f: impl FnOnce(&mut Settings)) {
    update(f);
    save();
}

/// Registers a callback that runs (on the changing thread) after every effective change.
pub fn on_change(f: impl Fn() + Send + Sync + 'static) {
    LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).push(Box::new(f));
}

// ---------------------------------------------------------------------------
// Persistence: a plain `key = value` file so it's easy to read, edit and share.
// ---------------------------------------------------------------------------

pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));

    base.unwrap_or_else(std::env::temp_dir).join(APP_NAME)
}

pub fn config_path() -> PathBuf {
    config_dir().join("settings.ini")
}

pub fn load() -> Settings {
    match std::fs::read_to_string(config_path()) {
        Ok(text) => from_text(&text),
        Err(_) => Settings::default(),
    }
}

pub fn save() {
    let text = to_text(&current(), true);
    let dir = config_dir();
    let result = std::fs::create_dir_all(&dir).and_then(|_| {
        // Write-then-rename so a crash mid-write can't leave a truncated file.
        let tmp = dir.join("settings.ini.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, config_path())
    });
    if let Err(e) = result {
        super::log::write(&format!("failed to save settings: {e}"));
    }
}

/// Serializes settings. With `with_prefs`, launch preferences are included too
/// (the "Copy settings" button leaves them out so only the feel is shared).
pub fn to_text(s: &Settings, with_prefs: bool) -> String {
    let mut out = String::new();
    for t in TOGGLES {
        let _ = writeln!(out, "{} = {}", t.key, (t.get)(s));
    }
    for n in NUMS {
        let _ = writeln!(out, "{} = {:.*}", n.key, n.decimals.max(2), (n.get)(s));
    }
    let _ = writeln!(out, "excluded_apps = {}", s.excluded_apps.join(", "));
    if with_prefs {
        let _ = writeln!(out, "run_as_admin = {}", s.run_as_admin);
        let _ = writeln!(out, "ask_admin_at_launch = {}", s.ask_admin_at_launch);
        let _ = writeln!(out, "welcomed = {}", s.welcomed);
    }
    out
}

pub fn from_text(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());

        if let Some(t) = TOGGLES.iter().find(|t| t.key == key) {
            if let Ok(v) = value.parse() {
                (t.set)(&mut s, v);
            }
        } else if let Some(n) = NUMS.iter().find(|n| n.key == key) {
            if let Ok(v) = value.parse::<f64>()
                && v.is_finite()
            {
                (n.set)(&mut s, v.clamp(n.min, n.max));
            }
        } else {
            match key {
                "excluded_apps" => s.excluded_apps = parse_app_list(value),
                "run_as_admin" => s.run_as_admin = value.parse().unwrap_or(s.run_as_admin),
                "ask_admin_at_launch" => s.ask_admin_at_launch = value.parse().unwrap_or(s.ask_admin_at_launch),
                "welcomed" => s.welcomed = value.parse().unwrap_or(s.welcomed),
                _ => {}
            }
        }
    }
    s
}

/// Splits a comma- or newline-separated list of executable names.
pub fn parse_app_list(text: &str) -> Vec<String> {
    text.split([',', '\n', ';'])
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| !a.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_text() {
        let mut s = Settings::default();
        s.speed = 1.75;
        s.natural_direction = false;
        s.excluded_apps = vec!["blender.exe".into(), "game.exe".into()];
        s.run_as_admin = true;
        assert_eq!(from_text(&to_text(&s, true)), s);
    }

    #[test]
    fn clamps_and_ignores_garbage() {
        let s = from_text("speed = 999\nfriction = nope\nunknown = 3\n# comment");
        assert_eq!(s.speed, 5.0);
        assert_eq!(s.friction, Settings::default().friction);
    }
}
