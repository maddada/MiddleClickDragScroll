use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Sense, Stroke, Vec2, ViewportCommand, pos2, vec2};

use crate::platform::{Elevation, SystemServices};
use crate::shared::engine::simulate_glide;
use crate::shared::settings::{self, APP_NAME, NUMS, Section, Settings, TOGGLES};
use crate::shared::{icon, log};

const SAVE_DELAY: Duration = Duration::from_millis(600);

#[derive(Default)]
struct WindowState {
    sender: Option<Sender<()>>,
    open: bool,
    ctx: Option<egui::Context>,
}

/// Opens the settings window on demand. The window runs on its own thread
/// (the main thread belongs to the platform's tray/message loop) and that same
/// thread is reused for every opening, as winit requires.
pub struct SettingsWindow {
    services: Arc<dyn SystemServices>,
    state: Arc<Mutex<WindowState>>,
}

impl SettingsWindow {
    pub fn new(services: Arc<dyn SystemServices>) -> Arc<Self> {
        let state = Arc::new(Mutex::new(WindowState::default()));
        let repaint = state.clone();
        // Reflect changes made elsewhere (e.g. the tray toggle) while the window is open.
        settings::on_change(move || {
            if let Some(ctx) = repaint.lock().unwrap_or_else(|e| e.into_inner()).ctx.clone() {
                ctx.request_repaint();
            }
        });
        Arc::new(Self { services, state })
    }

    pub fn show(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.open {
            if let Some(ctx) = &st.ctx {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                ctx.request_repaint();
            }
            return;
        }
        st.open = true;
        let sender = st.sender.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let (state, services) = (self.state.clone(), self.services.clone());
            std::thread::Builder::new()
                .name("settings-ui".into())
                .spawn(move || ui_thread(rx, state, services))
                .expect("failed to spawn settings thread");
            tx
        });
        let _ = sender.send(());
    }
}

fn ui_thread(rx: Receiver<()>, state: Arc<Mutex<WindowState>>, services: Arc<dyn SystemServices>) {
    for () in rx {
        let creator_state = state.clone();
        let creator_services = services.clone();
        let result = eframe::run_native(
            APP_NAME,
            native_options(),
            Box::new(move |cc| {
                creator_state.lock().unwrap_or_else(|e| e.into_inner()).ctx = Some(cc.egui_ctx.clone());
                Ok(Box::new(SettingsApp::new(creator_services)))
            }),
        );
        if let Err(e) = result {
            log::write(&format!("settings window failed: {e}"));
        }
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        st.ctx = None;
        st.open = false;
        drop(st);
        settings::save();
    }
}

fn native_options() -> eframe::NativeOptions {
    let icon = egui::IconData { rgba: icon::render_rgba(64, true), width: 64, height: 64 };
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{APP_NAME} settings"))
            .with_inner_size([560.0, 860.0])
            .with_min_inner_size([440.0, 360.0])
            .with_icon(Arc::new(icon)),
        centered: true,
        run_and_return: true,
        ..Default::default()
    };
    #[cfg(windows)]
    {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        }));
    }
    // macOS: winit requires the main thread there, so that backend will need to
    // run this window on the main thread instead (see README).
    options
}

struct SettingsApp {
    services: Arc<dyn SystemServices>,
    defaults: Settings,
    excluded_text: String,
    autostart: bool,
    elevation: Elevation,
    preview_speed: f64,
    save_due: Option<Instant>,
    flash: Option<(String, Instant)>,
}

impl SettingsApp {
    fn new(services: Arc<dyn SystemServices>) -> Self {
        let s = settings::current();
        Self {
            autostart: services.autostart_enabled(),
            elevation: services.elevation(),
            services,
            defaults: Settings::default(),
            excluded_text: s.excluded_apps.join("\n"),
            preview_speed: 3000.0,
            save_due: None,
            flash: None,
        }
    }

    fn flash(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let before = settings::current();
        let mut s = (*before).clone();
        let mut admin_pref_changed = false;

        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            self.actions(ui, &mut s);
            ui.add_space(6.0);
        });
        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                self.header(ui, &mut s);
                for section in Section::ALL {
                    self.section(ui, section, &mut s);
                }
                self.excluded_apps(ui, &mut s);
                admin_pref_changed = self.system(ui, &mut s);
                ui.add_space(12.0);
            });
        });

        if s != *before {
            settings::update(|current| *current = s);
            self.save_due = Some(Instant::now() + SAVE_DELAY);
            if admin_pref_changed {
                self.services.run_as_admin_changed();
                self.autostart = self.services.autostart_enabled();
            }
        }
        if let Some(due) = self.save_due {
            let now = Instant::now();
            if now >= due {
                settings::save();
                self.save_due = None;
            } else {
                ui.ctx().request_repaint_after(due - now);
            }
        }
    }
}

impl SettingsApp {
    fn header(&mut self, ui: &mut egui::Ui, s: &mut Settings) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(APP_NAME);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut s.enabled, "Enabled").on_hover_text(TOGGLES[0].help);
            });
        });
        ui.label(
            RichText::new("Hold the middle button and drag to scroll. Flick to glide. Tap for a normal middle click.")
                .weak(),
        );
        ui.label(
            RichText::new("Changes apply instantly. Hover a setting for details.")
                .weak()
                .small(),
        );
    }

    fn section(&mut self, ui: &mut egui::Ui, section: Section, s: &mut Settings) {
        section_heading(ui, section.title());

        if section == Section::Drag {
            ui.horizontal_wrapped(|ui| {
                for toggle in &TOGGLES[1..] {
                    let mut value = (toggle.get)(s);
                    if ui.checkbox(&mut value, toggle.label).on_hover_text(toggle.help).changed() {
                        (toggle.set)(s, value);
                    }
                }
            });
            ui.add_space(4.0);
        }

        let slider_width = (ui.available_width() - 210.0).clamp(140.0, 420.0);
        egui::Grid::new(section.title()).num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.spacing_mut().slider_width = slider_width;
            for spec in NUMS.iter().filter(|n| n.section == section) {
                ui.label(spec.label).on_hover_text(spec.help);

                let mut value = (spec.get)(s);
                let suffix = match spec.unit.chars().next() {
                    Some(c) if c.is_alphanumeric() => format!(" {}", spec.unit),
                    _ => spec.unit.to_string(),
                };
                let mut slider = egui::Slider::new(&mut value, spec.min..=spec.max)
                    .logarithmic(spec.logarithmic)
                    .fixed_decimals(spec.decimals);
                slider = match spec.max_label {
                    // Format the unit ourselves so the max can read "never" instead of "5000 px".
                    Some(label) => {
                        let (max, decimals) = (spec.max, spec.decimals);
                        slider
                            .custom_formatter(move |v, _| {
                                if v >= max { label.to_string() } else { format!("{v:.decimals$}{suffix}") }
                            })
                            .custom_parser(move |text| {
                                let text = text.trim();
                                if text.eq_ignore_ascii_case(label) {
                                    return Some(max);
                                }
                                text.trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace()).parse().ok()
                            })
                    }
                    None => slider.suffix(suffix),
                };
                ui.add(slider).on_hover_text(spec.help);

                let default = (spec.get)(&self.defaults);
                if (value - default).abs() > 1e-9 {
                    let tip = match spec.max_label {
                        Some(label) if default >= spec.max => format!("Reset to {label}"),
                        _ => format!("Reset to {:.*}", spec.decimals, default),
                    };
                    if ui.small_button("↺").on_hover_text(tip).clicked() {
                        value = default;
                    }
                } else {
                    ui.label("");
                }
                (spec.set)(s, value);
                ui.end_row();
            }
        });

        if section == Section::Glide {
            self.glide_preview(ui, s);
        }
    }

    fn glide_preview(&mut self, ui: &mut egui::Ui, s: &Settings) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Preview a flick at");
            ui.add(
                egui::Slider::new(&mut self.preview_speed, 500.0..=12000.0)
                    .logarithmic(true)
                    .fixed_decimals(0)
                    .suffix(" px/s"),
            );
        });

        let (distance, duration, points) = simulate_glide(self.preview_speed, s);
        ui.label(format!("→ glides {distance:.0} px over {duration:.2} s"));

        let (response, painter) = ui.allocate_painter(vec2(ui.available_width(), 90.0), Sense::hover());
        let rect = response.rect;
        let visuals = ui.visuals();
        painter.rect_filled(rect, 6.0, visuals.extreme_bg_color);

        let span = duration.max(3.0);
        let v0 = points.first().map(|p| p.1).unwrap_or(1.0).max(1.0);
        let plot = rect.shrink2(Vec2::new(8.0, 10.0));
        let line: Vec<_> = points
            .iter()
            .map(|&(t, v)| {
                pos2(
                    plot.left() + (t / span) as f32 * plot.width(),
                    plot.bottom() - (v / v0) as f32 * plot.height(),
                )
            })
            .collect();
        painter.line(line, Stroke::new(2.0, visuals.selection.bg_fill));

        let font = egui::FontId::proportional(11.0);
        let weak = visuals.weak_text_color();
        painter.text(rect.left_top() + vec2(8.0, 4.0), egui::Align2::LEFT_TOP, "speed", font.clone(), weak);
        painter.text(rect.right_bottom() - vec2(8.0, 4.0), egui::Align2::RIGHT_BOTTOM, format!("{span:.1} s"), font, weak);
    }

    fn excluded_apps(&mut self, ui: &mut egui::Ui, s: &mut Settings) {
        section_heading(ui, "Excluded apps");
        ui.label(
            RichText::new("The middle button works normally in these apps. One per line, e.g. blender.exe").weak(),
        );
        let edit = egui::TextEdit::multiline(&mut self.excluded_text)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("blender.exe");
        if ui.add(edit).changed() {
            s.excluded_apps = settings::parse_app_list(&self.excluded_text);
        }
    }

    /// Returns true if the "run as administrator" preference changed.
    fn system(&mut self, ui: &mut egui::Ui, s: &mut Settings) -> bool {
        section_heading(ui, "System");

        let mut autostart = self.autostart;
        if ui.checkbox(&mut autostart, self.services.autostart_label()).changed() {
            if let Err(e) = self.services.set_autostart(autostart) {
                self.flash(e);
            }
            self.autostart = self.services.autostart_enabled();
        }

        if self.elevation == Elevation::NotApplicable {
            return false;
        }

        let admin_changed = ui
            .checkbox(&mut s.run_as_admin, "Run as administrator")
            .on_hover_text(
                "Needed to scroll windows that run as administrator (Task Manager, admin terminals…). \
                 Starts elevated from now on.",
            )
            .changed();
        ui.checkbox(&mut s.ask_admin_at_launch, "Ask at launch when not running as administrator");

        ui.horizontal(|ui| {
            if self.elevation == Elevation::Elevated {
                ui.label(RichText::new("✔ Running as administrator").color(Color32::from_rgb(0x3C, 0xB3, 0x71)));
            } else {
                ui.label(RichText::new("Not running as administrator").weak());
                if ui.button("Restart as administrator").clicked() {
                    self.services.restart_elevated();
                }
            }
        });
        admin_changed
    }

    fn actions(&mut self, ui: &mut egui::Ui, s: &mut Settings) {
        ui.horizontal(|ui| {
            if ui
                .button("Reset to defaults")
                .on_hover_text("Resets every slider and toggle (keeps excluded apps and system options)")
                .clicked()
            {
                for spec in NUMS {
                    (spec.set)(s, (spec.get)(&self.defaults));
                }
                for toggle in TOGGLES {
                    (toggle.set)(s, (toggle.get)(&self.defaults));
                }
                self.flash("Reset to defaults");
            }
            if ui.button("Copy settings").on_hover_text("Copies all values as text").clicked() {
                ui.ctx().copy_text(settings::to_text(s, false));
                self.flash("Copied to clipboard");
            }
            if ui.button("Open config folder").clicked() {
                self.services.open_config_folder();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Quit app").clicked() {
                    settings::update(|current| *current = s.clone());
                    settings::save();
                    self.services.quit();
                }
            });
        });

        if let Some((message, at)) = &self.flash {
            let age = at.elapsed();
            if age < Duration::from_secs(4) {
                ui.label(RichText::new(message).small());
                ui.ctx().request_repaint_after(Duration::from_secs(4) - age);
            } else {
                self.flash = None;
            }
        }
    }
}

fn section_heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(title).strong().size(15.0));
    ui.separator();
}
