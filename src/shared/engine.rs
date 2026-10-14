//! The scroll engine: turns gesture commands into paced scroll output,
//! including flick detection and momentum ("glide") physics.
//!
//! It runs on its own thread. Platform backends provide a [`ScrollOutput`]
//! (how to actually scroll) and a [`FrameClock`] (how to wait for the next
//! display refresh), so the physics feel identical on every OS.

use std::collections::VecDeque;
use std::ops::{Add, AddAssign, Mul, Sub};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::settings::{self, GLIDE_CANCEL_NEVER, Settings};

/// Commands sent from the gesture recognizer (input thread) to the engine.
#[derive(Clone, Copy, Debug)]
pub enum Command {
    /// The trigger button went down: stop any glide, get ready for a drag.
    Touch { at: Instant },
    /// Pointer motion while dragging, in logical pixels, in mouse direction.
    Drag { dx: f64, dy: f64, at: Instant },
    /// The trigger button was released after dragging.
    Release { at: Instant },
    /// The trigger button was tapped without dragging: replay a middle click.
    Click,
    /// Some other input (button, real wheel) that should cancel a glide.
    Stop,
}

/// Where scroll output goes. Implemented per platform.
pub trait ScrollOutput: Send {
    /// A gesture is starting: latch whatever the backend needs (target window, units…).
    fn begin(&mut self) {}

    /// Scrolls by logical pixels. `+x` reveals content to the right, `+y` reveals
    /// content below. `momentum` is true for glide output (after release).
    /// Return `false` to end a glide, e.g. because the pointer moved onto another
    /// window and the output would land in the wrong place.
    fn scroll(&mut self, x: f64, y: f64, momentum: bool) -> bool;

    /// Current pointer position in logical pixels, if the backend can tell.
    /// Used to stop a glide once the pointer moves away (`glide_cancel_distance`).
    fn pointer_position(&mut self) -> Option<Vec2> {
        None
    }

    /// Emits a normal middle click.
    fn middle_click(&mut self);
}

/// Paces output to the display. Implemented per platform.
pub trait FrameClock: Send {
    /// Runs once on the engine thread before anything else (e.g. to raise its priority).
    fn prepare_thread(&mut self) {}

    /// A gesture is starting: re-pick the display to sync with.
    fn retarget(&mut self) {}

    /// Blocks until the next display refresh (or an equivalent interval).
    fn wait(&mut self);
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn len(self) -> f64 {
        self.x.hypot(self.y)
    }

    pub fn dot(self, o: Vec2) -> f64 {
        self.x * o.x + self.y * o.y
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}

impl AddAssign for Vec2 {
    fn add_assign(&mut self, o: Vec2) {
        *self = *self + o;
    }
}

impl Mul<f64> for Vec2 {
    type Output = Vec2;
    fn mul(self, k: f64) -> Vec2 {
        Vec2::new(self.x * k, self.y * k)
    }
}

// ---------------------------------------------------------------------------
// Glide physics, shared by the engine and the settings preview.
// ---------------------------------------------------------------------------

/// Advances a glide by `dt` seconds. Returns `(new_speed, distance_travelled)`;
/// a `new_speed` of 0 means the glide is over.
pub fn glide_step(speed: f64, dt: f64, s: &Settings) -> (f64, f64) {
    let mut next = speed * (-s.friction * dt).exp() - s.tail_brake * dt;
    if next < s.stop_speed {
        next = 0.0;
    }
    (next, (speed + next) * 0.5 * dt)
}

/// Simulates a glide from `v0` px/s. Returns total distance (px), duration (s)
/// and `(time, speed)` samples for plotting.
pub fn simulate_glide(v0: f64, s: &Settings) -> (f64, f64, Vec<(f64, f64)>) {
    const DT: f64 = 1.0 / 240.0;
    let mut speed = v0.min(s.max_speed);
    let (mut t, mut dist) = (0.0, 0.0);
    let mut points = vec![(0.0, speed)];
    while speed > 0.0 && t < 30.0 {
        let (next, travel) = glide_step(speed, DT, s);
        speed = next;
        dist += travel;
        t += DT;
        points.push((t, speed));
    }
    (dist, t, points)
}

// ---------------------------------------------------------------------------
// Engine thread
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Queue {
    commands: Mutex<Vec<Command>>,
    ready: Condvar,
}

/// Cheap, thread-safe handle for sending commands to the engine.
#[derive(Clone)]
pub struct EngineHandle {
    queue: Arc<Queue>,
}

impl EngineHandle {
    pub fn send(&self, command: Command) {
        self.queue.commands.lock().unwrap_or_else(|e| e.into_inner()).push(command);
        self.queue.ready.notify_one();
    }
}

pub fn spawn(output: Box<dyn ScrollOutput>, clock: Box<dyn FrameClock>) -> EngineHandle {
    let queue = Arc::new(Queue::default());
    let handle = EngineHandle { queue: queue.clone() };
    std::thread::Builder::new()
        .name("scroll-engine".into())
        .spawn(move || Engine::new(output, clock).run(&queue))
        .expect("failed to spawn scroll engine thread");
    handle
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Idle,
    /// Button down, not dragging yet.
    Held,
    Dragging,
    Gliding,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Axis {
    Undecided,
    Free,
    X,
    Y,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    at: Instant,
    /// Output-space delta (after direction, speed and axis lock).
    d: Vec2,
}

/// Flicking again within this long after interrupting a glide stacks speed.
const STACK_WINDOW: Duration = Duration::from_millis(600);
/// Drag samples older than this are dropped (well beyond the largest velocity window).
const SAMPLE_HISTORY: Duration = Duration::from_millis(500);
/// Never integrate more than this in one step (e.g. after the machine stalls).
const MAX_STEP: f64 = 0.05;

struct Engine {
    output: Box<dyn ScrollOutput>,
    clock: Box<dyn FrameClock>,
    phase: Phase,
    axis: Axis,
    pending: Vec2,
    samples: VecDeque<Sample>,
    velocity: Vec2,
    smooth_speed: f64,
    last_input: Instant,
    last_step: Instant,
    /// Glide velocity that the current touch interrupted, for flick stacking.
    interrupted: Option<(Vec2, Instant)>,
    /// Pointer position when the current glide started.
    glide_origin: Option<Vec2>,
}

impl Engine {
    fn new(output: Box<dyn ScrollOutput>, clock: Box<dyn FrameClock>) -> Self {
        let now = Instant::now();
        Self {
            output,
            clock,
            phase: Phase::Idle,
            axis: Axis::Undecided,
            pending: Vec2::ZERO,
            samples: VecDeque::new(),
            velocity: Vec2::ZERO,
            smooth_speed: 0.0,
            last_input: now,
            last_step: now,
            interrupted: None,
            glide_origin: None,
        }
    }

    fn run(mut self, queue: &Queue) {
        self.clock.prepare_thread();
        let mut active = false;
        loop {
            let commands = if active {
                self.clock.wait();
                std::mem::take(&mut *queue.commands.lock().unwrap_or_else(|e| e.into_inner()))
            } else {
                let mut guard = queue.commands.lock().unwrap_or_else(|e| e.into_inner());
                while guard.is_empty() {
                    guard = queue.ready.wait(guard).unwrap_or_else(|e| e.into_inner());
                }
                std::mem::take(&mut *guard)
            };

            let s = settings::current();
            for command in commands {
                self.handle(command, &s);
            }
            let emitted = self.step(Instant::now(), &s);

            // Keep ticking once per frame while gliding or while drag output is flowing.
            // When a drag pauses we go back to sleep, so the next movement is sent
            // immediately instead of waiting for a frame.
            active = self.phase == Phase::Gliding || emitted;
        }
    }

    fn handle(&mut self, command: Command, s: &Settings) {
        match command {
            Command::Touch { at } => {
                self.interrupted = (self.phase == Phase::Gliding).then_some((self.velocity, at));
                self.phase = Phase::Held;
                self.velocity = Vec2::ZERO;
                self.pending = Vec2::ZERO;
                self.axis = Axis::Undecided;
                self.smooth_speed = 0.0;
                self.last_input = at;
                self.samples.clear();
                self.samples.push_back(Sample { at, d: Vec2::ZERO });
                self.output.begin();
                self.clock.retarget();
            }
            Command::Drag { dx, dy, at } => self.drag(dx, dy, at, s),
            Command::Release { at } => self.release(at, s),
            Command::Click => {
                // A tap that stopped a glide is just "stop", like touching a trackpad.
                if self.interrupted.is_none() {
                    self.output.middle_click();
                }
                self.phase = Phase::Idle;
            }
            Command::Stop => {
                if self.phase == Phase::Gliding {
                    self.phase = Phase::Idle;
                    self.velocity = Vec2::ZERO;
                }
            }
        }
    }

    fn drag(&mut self, dx: f64, dy: f64, at: Instant, s: &Settings) {
        if self.phase == Phase::Idle || self.phase == Phase::Gliding {
            // Shouldn't happen (Touch always comes first) but recover gracefully.
            self.handle(Command::Touch { at }, s);
        }
        self.phase = Phase::Dragging;

        if self.axis == Axis::Undecided {
            self.axis = if !s.axis_lock {
                Axis::Free
            } else if dy.abs() >= dx.abs() * s.axis_lock_ratio {
                Axis::Y
            } else if dx.abs() >= dy.abs() * s.axis_lock_ratio {
                Axis::X
            } else {
                Axis::Free
            };
        }

        // Smoothed pointer speed drives drag acceleration.
        let dt = at.saturating_duration_since(self.last_input).as_secs_f64().max(0.001);
        let instant_speed = dx.hypot(dy) / dt;
        let alpha = 1.0 - (-dt / 0.03).exp();
        self.smooth_speed += (instant_speed - self.smooth_speed) * alpha;
        self.last_input = at;

        let gain = s.speed * (1.0 + s.drag_acceleration * (self.smooth_speed / 1000.0).min(4.0));
        let direction = if s.natural_direction { -1.0 } else { 1.0 };
        let mut d = Vec2::new(dx, dy) * (gain * direction);
        match self.axis {
            Axis::X => d.y = 0.0,
            Axis::Y => d.x = 0.0,
            _ => {}
        }

        self.pending += d;
        self.samples.push_back(Sample { at, d });
        while self.samples.len() > 2 && at.saturating_duration_since(self.samples[1].at) > SAMPLE_HISTORY {
            self.samples.pop_front();
        }
    }

    fn release(&mut self, at: Instant, s: &Settings) {
        self.phase = Phase::Idle;
        let mut v = self.release_velocity(at, s) * s.flick_gain;
        if v.len() < s.min_flick_speed {
            return;
        }

        if let Some((previous, when)) = self.interrupted
            && at.saturating_duration_since(when) < STACK_WINDOW
            && v.dot(previous) > 0.0
        {
            v += previous * s.flick_stacking;
        }

        let speed = v.len();
        if speed > s.max_speed {
            v = v * (s.max_speed / speed);
        }
        self.velocity = v;
        self.phase = Phase::Gliding;
        self.glide_origin = self.output.pointer_position();
        // The glide starts at the moment of release.
        if self.last_step < at {
            self.last_step = at;
        }
    }

    /// Average output velocity over the last `flick_window_ms` before release.
    fn release_velocity(&self, at: Instant, s: &Settings) -> Vec2 {
        let Some(last_motion) = self.samples.iter().rev().find(|x| x.d != Vec2::ZERO) else {
            return Vec2::ZERO;
        };
        if at.saturating_duration_since(last_motion.at).as_secs_f64() * 1000.0 > s.release_hold_ms {
            return Vec2::ZERO;
        }

        let window = Duration::from_secs_f64(s.flick_window_ms / 1000.0);
        let window_start = at.checked_sub(window).unwrap_or(at);
        let Some(first) = self.samples.iter().position(|x| x.at >= window_start) else {
            return Vec2::ZERO;
        };
        // Each sample's delta accumulated since the previous sample, so the
        // span starts at the sample before the first one in the window.
        let span_start = if first == 0 { self.samples[0].at } else { self.samples[first - 1].at };
        let total = self.samples.iter().skip(first).fold(Vec2::ZERO, |acc, x| acc + x.d);
        let span = at.saturating_duration_since(span_start).as_secs_f64().max(0.008);
        total * (1.0 / span)
    }

    /// Emits pending drag movement and advances any glide. Returns whether anything was emitted.
    fn step(&mut self, now: Instant, s: &Settings) -> bool {
        let dt = now.saturating_duration_since(self.last_step).as_secs_f64().min(MAX_STEP);
        self.last_step = now;

        let mut out = std::mem::take(&mut self.pending);
        if self.phase == Phase::Gliding && self.pointer_moved_away(s) {
            self.phase = Phase::Idle;
            self.velocity = Vec2::ZERO;
        }
        let momentum = self.phase == Phase::Gliding;
        if momentum {
            let speed = self.velocity.len();
            let (next, travel) = glide_step(speed, dt, s);
            if speed > 0.0 {
                let dir = self.velocity * (1.0 / speed);
                out += dir * travel;
                self.velocity = dir * next;
            }
            if next == 0.0 {
                self.phase = Phase::Idle;
            }
        }

        if out == Vec2::ZERO {
            return false;
        }
        if !self.output.scroll(out.x, out.y, momentum) && momentum {
            self.phase = Phase::Idle;
            self.velocity = Vec2::ZERO;
        }
        true
    }

    fn pointer_moved_away(&mut self, s: &Settings) -> bool {
        if s.glide_cancel_distance >= GLIDE_CANCEL_NEVER {
            return false;
        }
        match (self.glide_origin, self.output.pointer_position()) {
            (Some(origin), Some(now)) => (now - origin).len() > s.glide_cancel_distance,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Clone)]
    struct Recorder(Arc<Mutex<(Vec2, u32)>>);

    impl ScrollOutput for Recorder {
        fn scroll(&mut self, x: f64, y: f64, _momentum: bool) -> bool {
            self.0.lock().unwrap().0 += Vec2::new(x, y);
            true
        }
        fn middle_click(&mut self) {
            self.0.lock().unwrap().1 += 1;
        }
    }

    struct NoClock;
    impl FrameClock for NoClock {
        fn wait(&mut self) {}
    }

    fn engine(rec: &Recorder) -> Engine {
        Engine::new(Box::new(rec.clone()), Box::new(NoClock))
    }

    /// Drags at `speed` px/s (mouse moving up) for `ms`, sampling at 1 kHz.
    fn drag_up(e: &mut Engine, s: &Settings, start: Instant, speed: f64, ms: u64) -> Instant {
        e.handle(Command::Touch { at: start }, s);
        let mut t = start;
        for _ in 0..ms {
            t += Duration::from_millis(1);
            e.handle(Command::Drag { dx: 0.0, dy: -speed / 1000.0, at: t }, s);
        }
        t
    }

    #[test]
    fn natural_drag_up_scrolls_down() {
        let rec = Recorder::default();
        let mut e = engine(&rec);
        let s = Settings { drag_acceleration: 0.0, ..Settings::default() };
        let t = drag_up(&mut e, &s, Instant::now(), 1000.0, 100);
        e.step(t, &s);
        let total = rec.0.lock().unwrap().0;
        assert!((total.y - 100.0).abs() < 1e-6, "{total:?}");
        assert_eq!(total.x, 0.0);
    }

    #[test]
    fn flick_glides_at_release_speed() {
        let rec = Recorder::default();
        let mut e = engine(&rec);
        let s = Settings { drag_acceleration: 0.0, ..Settings::default() };
        let t = drag_up(&mut e, &s, Instant::now(), 2000.0, 100);
        e.handle(Command::Release { at: t }, &s);
        assert_eq!(e.phase, Phase::Gliding);
        assert!((e.velocity.y - 2000.0).abs() < 50.0, "{:?}", e.velocity);
    }

    #[test]
    fn pausing_before_release_prevents_glide() {
        let rec = Recorder::default();
        let mut e = engine(&rec);
        let s = Settings::default();
        let t = drag_up(&mut e, &s, Instant::now(), 2000.0, 100);
        e.handle(Command::Release { at: t + Duration::from_millis(200) }, &s);
        assert_eq!(e.phase, Phase::Idle);
    }

    #[test]
    fn tap_clicks_but_tap_during_glide_only_stops() {
        let rec = Recorder::default();
        let mut e = engine(&rec);
        let s = Settings::default();
        let now = Instant::now();
        e.handle(Command::Touch { at: now }, &s);
        e.handle(Command::Click, &s);
        assert_eq!(rec.0.lock().unwrap().1, 1);

        let t = drag_up(&mut e, &s, now, 3000.0, 80);
        e.handle(Command::Release { at: t }, &s);
        assert_eq!(e.phase, Phase::Gliding);
        e.handle(Command::Touch { at: t + Duration::from_millis(50) }, &s);
        e.handle(Command::Click, &s);
        assert_eq!(rec.0.lock().unwrap().1, 1);
    }

    #[derive(Clone)]
    struct MovingPointer(Arc<Mutex<Vec2>>);

    impl ScrollOutput for MovingPointer {
        fn scroll(&mut self, _: f64, _: f64, _: bool) -> bool {
            true
        }
        fn middle_click(&mut self) {}
        fn pointer_position(&mut self) -> Option<Vec2> {
            Some(*self.0.lock().unwrap())
        }
    }

    #[test]
    fn moving_the_pointer_away_stops_the_glide() {
        let pointer = MovingPointer(Arc::new(Mutex::new(Vec2::ZERO)));
        let mut e = Engine::new(Box::new(pointer.clone()), Box::new(NoClock));
        let s = Settings { glide_cancel_distance: 300.0, ..Settings::default() };
        let t = drag_up(&mut e, &s, Instant::now(), 3000.0, 80);
        e.handle(Command::Release { at: t }, &s);

        *pointer.0.lock().unwrap() = Vec2::new(150.0, 0.0);
        e.step(t + Duration::from_millis(16), &s);
        assert_eq!(e.phase, Phase::Gliding);

        *pointer.0.lock().unwrap() = Vec2::new(301.0, 0.0);
        e.step(t + Duration::from_millis(32), &s);
        assert_eq!(e.phase, Phase::Idle);
    }

    #[test]
    fn by_default_the_glide_ignores_pointer_movement() {
        let pointer = MovingPointer(Arc::new(Mutex::new(Vec2::ZERO)));
        let mut e = Engine::new(Box::new(pointer.clone()), Box::new(NoClock));
        let s = Settings::default();
        let t = drag_up(&mut e, &s, Instant::now(), 3000.0, 80);
        e.handle(Command::Release { at: t }, &s);

        *pointer.0.lock().unwrap() = Vec2::new(4000.0, 2000.0);
        e.step(t + Duration::from_millis(16), &s);
        assert_eq!(e.phase, Phase::Gliding);
    }

    #[test]
    fn glide_simulation_terminates() {
        let (dist, secs, _) = simulate_glide(3000.0, &Settings::default());
        assert!(dist > 100.0 && secs > 0.2 && secs < 30.0, "{dist} {secs}");
    }
}
