//! The engine: screen capture → zones → colour → one sender thread per light.
//!
//! Energy rules it lives by:
//! - nothing runs while sync is paused; the capture and the lights are let go entirely;
//! - the capture thread sleeps inside AcquireNextFrame until the screen changes, and the frame is
//!   averaged down on the GPU, so a still screen costs nothing and a busy one very little;
//! - each light gets "latest colour wins": a slow light skips frames instead of queueing them, and
//!   a colour that barely moved isn't sent at all, only an occasional keepalive.
//!
//! Settings changes are applied live: colours and layouts change on the next frame, and only a light
//! whose connection settings changed is reconnected.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use lightsync_core::color::{Pipeline, changed};
use lightsync_core::govee::Rgb;
use lightsync_core::settings::{Connection, Light as LightSettings, Settings};
use lightsync_core::zones::{FULL, Frame, Letterbox, Zone, sample};

use crate::capture::Capture;
use crate::control::{
    ControlRequest, ControlResponse, EngineStatus, Found, Handler, LightStatus, LinkState, SyncState,
};
use crate::lights::Light;
use crate::lights::ble::{self, BleLight};
use crate::lights::lan::{LanBus, LanLight};
use crate::log::log;

const KEEPALIVE: Duration = Duration::from_secs(2);
const IDENTIFY_FOR: Duration = Duration::from_secs(8);

#[derive(Default)]
struct Signals {
    stop: bool,
    reload: bool,
}

struct Shared {
    settings_path: PathBuf,
    settings: Mutex<Settings>,
    status: Mutex<EngineStatus>,
    signals: Mutex<Signals>,
    wake: Condvar,
    identify: Mutex<Option<(String, Instant)>>,
    bus: Mutex<Option<Arc<LanBus>>>,
}

impl Shared {
    fn signal(&self, change: impl FnOnce(&mut Signals)) {
        change(&mut self.signals.lock().unwrap());
        self.wake.notify_all();
    }

    fn pending(&self) -> bool {
        let signals = self.signals.lock().unwrap();
        signals.stop || signals.reload
    }

    fn stopping(&self) -> bool {
        self.signals.lock().unwrap().stop
    }

    /// Sleeps up to `timeout`, waking early for a stop or a reload.
    fn rest(&self, timeout: Duration) {
        let signals = self.signals.lock().unwrap();
        let _ = self.wake.wait_timeout_while(signals, timeout, |s| !s.stop && !s.reload);
    }

    fn lan_bus(&self) -> Result<Arc<LanBus>> {
        let mut bus = self.bus.lock().unwrap();
        if let Some(bus) = bus.as_ref() {
            return Ok(bus.clone());
        }
        let opened = LanBus::open()?;
        *bus = Some(opened.clone());
        Ok(opened)
    }

    fn set_status(&self, state: SyncState, detail: &str) {
        let mut status = self.status.lock().unwrap();
        status.state = state;
        status.detail = detail.into();
        if state != SyncState::Running {
            status.fps = 0.0;
        }
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start() -> Self {
        let settings_path = crate::log::settings_path();
        let settings = Settings::load(&settings_path).unwrap_or_else(|e| {
            log!("couldn't read the settings, starting fresh: {e}");
            Settings::default()
        });
        let shared = Arc::new(Shared {
            settings_path,
            settings: Mutex::new(settings),
            status: Mutex::new(EngineStatus::default()),
            signals: Mutex::new(Signals::default()),
            wake: Condvar::new(),
            identify: Mutex::new(None),
            bus: Mutex::new(None),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new().name("engine".into()).spawn(move || run(&worker)).ok();
        Self { shared, thread }
    }

    pub fn status(&self) -> EngineStatus {
        self.shared.status.lock().unwrap().clone()
    }

    pub fn enabled(&self) -> bool {
        self.shared.settings.lock().unwrap().enabled
    }

    /// Pauses or resumes sync, and remembers it.
    pub fn set_enabled(&self, enabled: bool) {
        let mut settings =
            Settings::load(&self.shared.settings_path).unwrap_or_else(|_| self.shared.settings.lock().unwrap().clone());
        settings.enabled = enabled;
        if let Err(e) = settings.save(&self.shared.settings_path) {
            log!("couldn't save the settings: {e}");
        }
        *self.shared.settings.lock().unwrap() = settings;
        self.shared.signal(|s| s.reload = true);
    }

    /// Picks up settings the window saved.
    pub fn reload(&self) -> Result<(), String> {
        let settings = Settings::load(&self.shared.settings_path)?;
        *self.shared.settings.lock().unwrap() = settings;
        self.shared.signal(|s| s.reload = true);
        Ok(())
    }

    pub fn identify(&self, id: &str) {
        *self.shared.identify.lock().unwrap() = Some((id.to_string(), Instant::now()));
        self.shared.signal(|s| s.reload = true);
    }

    pub fn discover(&self) -> Result<Vec<Found>> {
        let bus = self.shared.lan_bus()?;
        discover(&bus)
    }

    pub fn handle(&self, request: ControlRequest) -> ControlResponse {
        match request.command.as_str() {
            "status" => ControlResponse { status: Some(self.status()), ..ControlResponse::success() },
            "reload" => match self.reload() {
                Ok(()) => ControlResponse::success(),
                Err(e) => ControlResponse::failure(e),
            },
            "pause" => {
                self.set_enabled(false);
                ControlResponse::success()
            }
            "resume" => {
                self.set_enabled(true);
                ControlResponse::success()
            }
            "toggle" => {
                self.set_enabled(!self.enabled());
                ControlResponse::success()
            }
            "identify" => match request.id {
                Some(id) => {
                    self.identify(&id);
                    ControlResponse::success()
                }
                None => ControlResponse::failure("identify needs a light id"),
            },
            "discover" => match self.discover() {
                Ok(found) => ControlResponse { found: Some(found), ..ControlResponse::success() },
                Err(e) => ControlResponse::failure(format!("{e:#}")),
            },
            other => ControlResponse::failure(format!("I don't know the command \"{other}\"")),
        }
    }

    /// Holds the engine weakly, so quitting still drops it and hands every light back.
    pub fn handler(self: &Arc<Self>) -> Handler {
        let engine = Arc::downgrade(self);
        Arc::new(move |request| match engine.upgrade() {
            Some(engine) => engine.handle(request),
            None => ControlResponse::failure("LightSync is quitting"),
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.signal(|s| s.stop = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Looks for lights over Wi-Fi and Bluetooth at the same time.
pub fn discover(bus: &LanBus) -> Result<Vec<Found>> {
    let bluetooth = std::thread::spawn(|| ble::scan(Duration::from_secs(5)));
    let mut found: Vec<Found> = bus
        .scan(Duration::from_secs(4))
        .into_iter()
        .map(|d| Found { link: "wifi".into(), id: d.device_id, sku: d.sku, address: d.ip.to_string(), rssi: None })
        .collect();
    match bluetooth.join() {
        Ok(Ok(lights)) => found.extend(lights.into_iter().map(|b| Found {
            link: "bluetooth".into(),
            id: b.address.clone(),
            sku: b.sku,
            address: b.address,
            rssi: Some(b.rssi),
        })),
        Ok(Err(e)) => log!("bluetooth scan: {e:#}"),
        Err(_) => {}
    }
    Ok(found)
}

fn run(shared: &Arc<Shared>) {
    let mut session: Option<Session> = None;
    loop {
        if shared.stopping() {
            break;
        }
        shared.signals.lock().unwrap().reload = false;
        let settings = shared.settings.lock().unwrap().clone();
        let active = settings.lights.iter().any(|l| l.enabled);
        if !settings.enabled || !active {
            // Letting go of the session restores every light and frees the capture
            session = None;
            if settings.enabled {
                shared.set_status(SyncState::NoLights, "Add a light to start");
            } else {
                shared.set_status(SyncState::Paused, "Sync is paused");
            }
            shared.status.lock().unwrap().lights.clear();
            shared.rest(Duration::from_secs(3600));
            continue;
        }
        let session = session.get_or_insert_with(|| Session::new(shared.clone()));
        session.apply(&settings);
        session.run_until_signal(&settings);
    }
    drop(session);
    log!("engine stopped");
}

/// Everything that lives while sync is on.
struct Session {
    shared: Arc<Shared>,
    capture: Option<Capture>,
    capture_key: (u32, u32),
    workers: Vec<Worker>,
    letterbox: Letterbox,
    last_tick: Instant,
    frames: u32,
    fps_since: Instant,
}

impl Session {
    fn new(shared: Arc<Shared>) -> Self {
        shared.set_status(SyncState::Starting, "Starting");
        Self {
            shared,
            capture: None,
            capture_key: (u32::MAX, 0),
            workers: Vec::new(),
            letterbox: Letterbox::default(),
            last_tick: Instant::now(),
            frames: 0,
            fps_since: Instant::now(),
        }
    }

    /// Brings workers in line with the settings, reconnecting only what has to be.
    fn apply(&mut self, settings: &Settings) {
        let key = (settings.capture.monitor, settings.capture.detail);
        if key != self.capture_key {
            self.capture = None;
            self.capture_key = key;
        }
        let aspect = self.capture.as_ref().map_or(16.0 / 9.0, |c| c.size.0 as f32 / c.size.1.max(1) as f32);
        let wanted: Vec<&LightSettings> = settings.lights.iter().filter(|l| l.enabled).collect();
        self.workers.retain(|w| {
            wanted.iter().any(|l| l.id == w.settings.id && Worker::same_link(&w.settings, l, &settings.color))
        });
        for light in wanted {
            match self.workers.iter_mut().find(|w| w.settings.id == light.id) {
                Some(worker) => worker.update(light, &settings.color, aspect),
                None => match Worker::spawn(&self.shared, light, &settings.color, aspect) {
                    Ok(worker) => self.workers.push(worker),
                    Err(e) => log!("{}: {e:#}", light.name),
                },
            }
        }
    }

    fn run_until_signal(&mut self, settings: &Settings) {
        let interval = Duration::from_secs_f32(1.0 / settings.capture.max_fps.clamp(1.0, 120.0));
        while !self.shared.pending() {
            if self.capture.is_none() {
                match Capture::open(settings.capture.monitor, settings.capture.detail) {
                    Ok(capture) => {
                        log!("capturing {}", capture.monitor_name);
                        {
                            let mut status = self.shared.status.lock().unwrap();
                            status.monitor = capture.monitor_name.clone();
                            status.screen = capture.size;
                        }
                        let aspect = capture.size.0 as f32 / capture.size.1.max(1) as f32;
                        self.capture = Some(capture);
                        for worker in &mut self.workers {
                            worker.rezone(aspect);
                        }
                    }
                    Err(e) => {
                        self.shared.set_status(SyncState::Trouble, &format!("{e:#}"));
                        self.publish();
                        self.shared.rest(Duration::from_secs(2));
                        continue;
                    }
                }
            }
            let identifying = self.identify_target();
            let settled = identifying.is_none() && self.workers.iter().all(|w| w.pipeline.settled);
            let wait = if settled { 500 } else { interval.as_millis() as u32 };
            let started = Instant::now();
            match self.capture.as_mut().map(|c| c.next(wait)) {
                Some(Ok(frame)) => {
                    let got_frame = frame.is_some();
                    if got_frame || !settled {
                        self.tick(frame.as_ref(), settings, identifying.as_deref());
                    }
                    if got_frame {
                        self.frames += 1;
                        // Cap the analysis rate; the screen keeps its frames for us meanwhile
                        if let Some(left) = interval.checked_sub(started.elapsed()) {
                            std::thread::sleep(left);
                        }
                    }
                }
                Some(Err(e)) => {
                    log!("{e:#}; reopening the capture");
                    self.capture = None;
                    self.shared.rest(Duration::from_secs(1));
                }
                None => {}
            }
            self.publish();
        }
    }

    fn identify_target(&self) -> Option<String> {
        let mut identify = self.shared.identify.lock().unwrap();
        match identify.as_ref() {
            Some((id, since)) if since.elapsed() < IDENTIFY_FOR => Some(id.clone()),
            Some(_) => {
                *identify = None;
                None
            }
            None => None,
        }
    }

    fn tick(&mut self, frame: Option<&Frame>, settings: &Settings, identifying: Option<&str>) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.5);
        self.last_tick = now;
        let content = match frame {
            Some(frame) if settings.capture.letterbox => self.letterbox.update(frame),
            _ if settings.capture.letterbox => self.letterbox.content,
            _ => FULL,
        };
        for worker in &mut self.workers {
            let colors = if identifying == Some(worker.settings.id.as_str()) {
                identify_pattern(
                    worker.zones.len(),
                    self.shared.identify.lock().unwrap().as_ref().map_or(0.0, |(_, t)| t.elapsed().as_secs_f32()),
                )
            } else {
                if let Some(frame) = frame {
                    worker.raw = Some(sample(frame, &worker.zones, content, worker.color.sampling));
                }
                let Some(raw) = &worker.raw else { continue };
                worker.pipeline.process(raw, &worker.color, dt)
            };
            worker.slot.offer(colors);
        }
    }

    /// Refreshes the status the tray and window show, twice a second.
    fn publish(&mut self) {
        let elapsed = self.fps_since.elapsed();
        if elapsed < Duration::from_millis(500) {
            return;
        }
        let fps = self.frames as f32 / elapsed.as_secs_f32();
        let lights: Vec<LightStatus> = self.workers.iter_mut().map(|w| w.status(elapsed)).collect();
        (self.frames, self.fps_since) = (0, Instant::now());
        let mut status = self.shared.status.lock().unwrap();
        if self.capture.is_some() {
            status.state = SyncState::Running;
            let live = lights.iter().filter(|l| l.state == LinkState::Live).count();
            status.detail = format!("{live} of {} lights live", lights.len());
            status.fps = fps;
        }
        status.lights = lights;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Tell every light at once, so they restore in parallel instead of one after another
        for worker in &self.workers {
            worker.slot.stop.store(true, Ordering::Relaxed);
            worker.slot.wake.notify_all();
        }
        self.workers.clear();
    }
}

/// Red marks the first segment, blue the last, and a white dot walks from first to last.
fn identify_pattern(n: usize, seconds: f32) -> Vec<Rgb> {
    let mut colors = vec![[12, 12, 12]; n.max(1)];
    let n = colors.len();
    colors[0] = [255, 0, 0];
    colors[n - 1] = [0, 0, 255];
    if n > 2 {
        colors[1 + (seconds * 3.0) as usize % (n - 2)] = [255, 255, 255];
    }
    colors
}

/// The newest colours for a light, handed from the capture thread to the light's own thread.
#[derive(Default)]
struct Slot {
    latest: Mutex<Option<Vec<Rgb>>>,
    wake: Condvar,
    stop: AtomicBool,
    sends: AtomicU64,
    shown: Mutex<Vec<Rgb>>,
    link: Mutex<(LinkState, String)>,
}

impl Slot {
    fn offer(&self, colors: Vec<Rgb>) {
        *self.latest.lock().unwrap() = Some(colors);
        self.wake.notify_one();
    }

    /// The next colours, or `None` once `timeout` passes (or when stopping).
    fn take(&self, timeout: Duration) -> Option<Vec<Rgb>> {
        let latest = self.latest.lock().unwrap();
        let (mut latest, _) = self
            .wake
            .wait_timeout_while(latest, timeout, |l| l.is_none() && !self.stop.load(Ordering::Relaxed))
            .unwrap();
        latest.take()
    }

    fn set_link(&self, state: LinkState, detail: impl Into<String>) {
        *self.link.lock().unwrap() = (state, detail.into());
    }

    /// Sleeps, waking early when told to stop.
    fn pause(&self, duration: Duration) {
        let latest = self.latest.lock().unwrap();
        let _ = self.wake.wait_timeout_while(latest, duration, |_| !self.stop.load(Ordering::Relaxed));
    }
}

struct Worker {
    settings: LightSettings,
    color: lightsync_core::color::ColorSettings,
    zones: Vec<Zone>,
    pipeline: Pipeline,
    raw: Option<Vec<[f32; 3]>>,
    slot: Arc<Slot>,
    thread: Option<JoinHandle<()>>,
    last_sends: u64,
}

impl Worker {
    /// True when switching between `a` and `b` needs no reconnect.
    fn same_link(a: &LightSettings, b: &LightSettings, defaults: &lightsync_core::color::ColorSettings) -> bool {
        a.connection == b.connection
            && a.on_exit == b.on_exit
            && a.max_fps == b.max_fps
            && a.color(defaults).min_delta == b.color(defaults).min_delta
    }

    fn spawn(
        shared: &Arc<Shared>,
        light: &LightSettings,
        defaults: &lightsync_core::color::ColorSettings,
        aspect: f32,
    ) -> Result<Self> {
        let color = light.color(defaults).clone();
        let mut sender: Box<dyn Light> = match &light.connection {
            Connection::Lan { device_id, ip, mode, gradient, .. } => {
                Box::new(LanLight::new(shared.lan_bus()?, &light.name, device_id, ip, *mode, *gradient, light.on_exit))
            }
            Connection::Ble { address, mode, command, .. } => {
                Box::new(BleLight::new(address, *mode, *command, light.on_exit, color.min_delta)?)
            }
        };
        let slot = Arc::new(Slot::default());
        let (thread_slot, name, min_delta) = (slot.clone(), light.name.clone(), color.min_delta);
        let min_interval = Duration::from_secs_f32(1.0 / light.max_fps.clamp(1.0, 60.0));
        let thread = std::thread::Builder::new()
            .name(format!("light {}", light.name))
            .spawn(move || stream(sender.as_mut(), &thread_slot, &name, min_delta, min_interval))?;
        Ok(Self {
            zones: light.layout.zones(aspect, light.reverse, light.offset),
            settings: light.clone(),
            color,
            pipeline: Pipeline::default(),
            raw: None,
            slot,
            thread: Some(thread),
            last_sends: 0,
        })
    }

    fn update(&mut self, light: &LightSettings, defaults: &lightsync_core::color::ColorSettings, aspect: f32) {
        self.color = light.color(defaults).clone();
        if light.layout != self.settings.layout
            || light.reverse != self.settings.reverse
            || light.offset != self.settings.offset
        {
            self.settings = light.clone();
            self.rezone(aspect);
        } else {
            self.settings = light.clone();
        }
        // Force a fresh pass so the change shows even on a still screen
        self.pipeline.settled = false;
    }

    fn rezone(&mut self, aspect: f32) {
        self.zones = self.settings.layout.zones(aspect, self.settings.reverse, self.settings.offset);
        self.raw = None;
    }

    fn status(&mut self, elapsed: Duration) -> LightStatus {
        let sends = self.slot.sends.load(Ordering::Relaxed);
        let rate = (sends - self.last_sends) as f32 / elapsed.as_secs_f32();
        self.last_sends = sends;
        let (state, detail) = self.slot.link.lock().unwrap().clone();
        LightStatus {
            id: self.settings.id.clone(),
            name: self.settings.name.clone(),
            state,
            detail,
            colors: self.slot.shown.lock().unwrap().clone(),
            sends_per_second: rate,
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.slot.stop.store(true, Ordering::Relaxed);
        self.slot.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A light's own thread: connect, then send whatever is newest, reconnecting with backoff on trouble.
fn stream(light: &mut dyn Light, slot: &Slot, name: &str, min_delta: u8, min_interval: Duration) {
    let mut backoff = Duration::from_secs(1);
    'connect: while !slot.stop.load(Ordering::Relaxed) {
        slot.set_link(LinkState::Connecting, "Connecting");
        if let Err(e) = light.connect() {
            let reason = format!("{e:#}");
            log!("{name}: {reason}");
            slot.set_link(LinkState::Trouble, reason);
            light.close(false);
            slot.pause(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
            continue;
        }
        log!("{name}: connected");
        slot.set_link(LinkState::Live, "Live");
        backoff = Duration::from_secs(1);
        let mut sent: Option<Vec<Rgb>> = None;
        let mut last_io = Instant::now();
        while !slot.stop.load(Ordering::Relaxed) {
            let wait = KEEPALIVE.saturating_sub(last_io.elapsed());
            let result = match slot.take(wait) {
                Some(colors) if changed(sent.as_deref(), &colors, min_delta) => {
                    let result = light.send(&colors);
                    if result.is_ok() {
                        slot.sends.fetch_add(1, Ordering::Relaxed);
                        *slot.shown.lock().unwrap() = colors.clone();
                        sent = Some(colors);
                        last_io = Instant::now();
                        std::thread::sleep(min_interval);
                    }
                    result
                }
                Some(_) => Ok(()),
                None if last_io.elapsed() >= KEEPALIVE => {
                    last_io = Instant::now();
                    light.keepalive()
                }
                None => Ok(()),
            };
            if let Err(e) = result {
                let reason = format!("{e:#}");
                log!("{name}: {reason}; reconnecting");
                slot.set_link(LinkState::Trouble, reason);
                light.close(false);
                continue 'connect;
            }
        }
    }
    light.close(true);
    log!("{name}: let go");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_marks_both_ends() {
        let colors = identify_pattern(6, 0.0);
        assert_eq!(colors[0], [255, 0, 0]);
        assert_eq!(colors[5], [0, 0, 255]);
        assert_eq!(colors.iter().filter(|c| **c == [255, 255, 255]).count(), 1);
        assert_eq!(identify_pattern(1, 3.0).len(), 1);
    }
}
