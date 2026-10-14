//! Recognizes the middle-button gesture from raw input and decides, per event,
//! whether the platform should swallow it or let it through.
//!
//! This runs synchronously inside the platform's input hook, so it must stay
//! cheap and never block. Heavier work is handed to the engine as [`Command`]s.

use std::time::Instant;

use super::engine::Command;
use super::settings::Settings;

#[derive(Clone, Copy, Debug)]
pub enum Input {
    /// The trigger (middle) button went down. `excluded` is true when the app
    /// under the pointer is on the exclusion list.
    TriggerDown { excluded: bool },
    TriggerUp,
    /// Pointer motion since the previous event, in logical pixels.
    /// Only reported while [`Gesture::is_capturing`].
    Motion { dx: f64, dy: f64 },
    OtherButtonDown,
    Wheel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Pass,
    Swallow,
}

#[derive(Clone, Copy, Debug)]
enum State {
    Idle,
    /// Button held, still within the click dead zone.
    Pending { moved_x: f64, moved_y: f64 },
    Dragging,
    /// Button held but we're not handling it (disabled or excluded app).
    Bypass,
}

pub struct Gesture {
    state: State,
}

impl Default for Gesture {
    fn default() -> Self {
        Self::new()
    }
}

impl Gesture {
    pub const fn new() -> Self {
        Self { state: State::Idle }
    }

    /// While true, the platform reports pointer motion as [`Input::Motion`].
    /// Motion events come back as `Swallow` when `freeze_cursor` is on, which
    /// keeps the pointer still; otherwise they pass and the pointer moves too.
    pub fn is_capturing(&self) -> bool {
        matches!(self.state, State::Pending { .. } | State::Dragging)
    }

    pub fn handle(&mut self, input: Input, s: &Settings, now: Instant, emit: &mut dyn FnMut(Command)) -> Verdict {
        match input {
            Input::TriggerDown { excluded } => {
                // Any press stops a glide, just like touching a trackpad.
                emit(Command::Touch { at: now });
                if !s.enabled || excluded {
                    self.state = State::Bypass;
                    return Verdict::Pass;
                }
                self.state = State::Pending { moved_x: 0.0, moved_y: 0.0 };
                Verdict::Swallow
            }

            Input::Motion { dx, dy } => match &mut self.state {
                State::Pending { moved_x, moved_y } => {
                    *moved_x += dx;
                    *moved_y += dy;
                    if moved_x.hypot(*moved_y) >= s.click_slop {
                        // Hand over everything moved so far, so the drag doesn't lose the dead zone.
                        emit(Command::Drag { dx: *moved_x, dy: *moved_y, at: now });
                        self.state = State::Dragging;
                    }
                    motion_verdict(s)
                }
                State::Dragging => {
                    emit(Command::Drag { dx, dy, at: now });
                    motion_verdict(s)
                }
                State::Idle | State::Bypass => Verdict::Pass,
            },

            Input::TriggerUp => {
                let previous = std::mem::replace(&mut self.state, State::Idle);
                match previous {
                    State::Pending { .. } => {
                        emit(Command::Click);
                        Verdict::Swallow
                    }
                    State::Dragging => {
                        emit(Command::Release { at: now });
                        Verdict::Swallow
                    }
                    State::Bypass | State::Idle => Verdict::Pass,
                }
            }

            Input::OtherButtonDown | Input::Wheel => {
                if !self.is_capturing() {
                    emit(Command::Stop);
                }
                Verdict::Pass
            }
        }
    }
}

/// What to do with pointer motion during a gesture.
pub fn motion_verdict(s: &Settings) -> Verdict {
    if s.freeze_cursor { Verdict::Swallow } else { Verdict::Pass }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(g: &mut Gesture, s: &Settings, inputs: &[Input]) -> (Vec<Verdict>, Vec<Command>) {
        let mut commands = Vec::new();
        let verdicts = inputs
            .iter()
            .map(|&i| g.handle(i, s, Instant::now(), &mut |c| commands.push(c)))
            .collect();
        (verdicts, commands)
    }

    #[test]
    fn small_wiggle_is_a_click() {
        let inputs = [Input::TriggerDown { excluded: false }, Input::Motion { dx: 2.0, dy: 1.0 }, Input::TriggerUp];

        let moving = Settings { freeze_cursor: false, ..Settings::default() };
        let (v, c) = run(&mut Gesture::new(), &moving, &inputs);
        assert_eq!(v, [Verdict::Swallow, Verdict::Pass, Verdict::Swallow]);
        assert!(matches!(c.as_slice(), [Command::Touch { .. }, Command::Click]));

        let frozen = Settings { freeze_cursor: true, ..Settings::default() };
        let (v, _) = run(&mut Gesture::new(), &frozen, &inputs);
        assert!(v.iter().all(|&v| v == Verdict::Swallow));
    }

    #[test]
    fn drag_hands_over_dead_zone_movement() {
        let mut g = Gesture::new();
        let (_, c) = run(
            &mut g,
            &Settings::default(),
            &[
                Input::TriggerDown { excluded: false },
                Input::Motion { dx: 0.0, dy: 4.0 },
                Input::Motion { dx: 0.0, dy: 4.0 },
                Input::Motion { dx: 0.0, dy: 1.0 },
                Input::TriggerUp,
            ],
        );
        assert!(matches!(
            c.as_slice(),
            [Command::Touch { .. }, Command::Drag { dy: 8.0, .. }, Command::Drag { dy: 1.0, .. }, Command::Release { .. }]
        ));
    }

    #[test]
    fn excluded_apps_pass_through() {
        let mut g = Gesture::new();
        let (v, _) = run(
            &mut g,
            &Settings::default(),
            &[Input::TriggerDown { excluded: true }, Input::Motion { dx: 50.0, dy: 0.0 }, Input::TriggerUp],
        );
        assert!(v.iter().all(|&v| v == Verdict::Pass));
        assert!(!g.is_capturing());
    }
}
