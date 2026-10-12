//! The main window's state: the settings being edited, the engine's live status, and scans.
//! Edits are saved a moment after they stop and the engine picks them up live.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use lightsync_core::settings::Settings;

use crate::control::{ControlRequest, EngineStatus, Found, send};

const SAVE_AFTER: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Scan {
    #[default]
    Idle,
    Scanning,
    Done(Vec<Found>),
    Failed(String),
}

pub struct Model {
    pub settings: Settings,
    pub problem: Option<String>,
    edited_at: Option<Instant>,
    status: Arc<Mutex<Option<EngineStatus>>>,
    scan: Arc<Mutex<Scan>>,
    ctx: egui::Context,
}

impl Model {
    pub fn new(ctx: egui::Context) -> Self {
        let (settings, problem) = match Settings::load(&crate::log::settings_path()) {
            Ok(settings) => (settings, None),
            Err(e) => (Settings::default(), Some(format!("Couldn't read your settings, so these are fresh: {e}"))),
        };
        let status = Arc::new(Mutex::new(None));
        // Poll the engine while the window is open; the thread ends with the window's process
        let (sink, repaint) = (status.clone(), ctx.clone());
        std::thread::spawn(move || {
            loop {
                let latest = send(&ControlRequest::new("status")).ok().and_then(|r| r.status);
                let changed = *sink.lock().unwrap() != latest;
                *sink.lock().unwrap() = latest;
                if changed {
                    repaint.request_repaint();
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        Self { settings, problem, edited_at: None, status, scan: Arc::default(), ctx }
    }

    pub fn status(&self) -> Option<EngineStatus> {
        self.status.lock().unwrap().clone()
    }

    pub fn running(&self) -> bool {
        self.status.lock().unwrap().is_some()
    }

    /// Call after changing `settings`; it's saved and applied once edits pause.
    pub fn edited(&mut self) {
        self.edited_at = Some(Instant::now());
    }

    pub fn tick(&mut self) {
        if let Some(at) = self.edited_at {
            if at.elapsed() >= SAVE_AFTER {
                self.edited_at = None;
                self.save();
            } else {
                self.ctx.request_repaint_after(SAVE_AFTER);
            }
        }
    }

    pub fn save(&mut self) {
        // The tray can pause or resume while this window is open, so keep whatever it last saved
        if let Ok(on_disk) = Settings::load(&crate::log::settings_path()) {
            self.settings.enabled = on_disk.enabled;
        }
        match self.settings.save(&crate::log::settings_path()) {
            Ok(()) => {
                self.problem = None;
                std::thread::spawn(|| send(&ControlRequest::new("reload")));
            }
            Err(e) => self.problem = Some(format!("Couldn't save your settings: {e}")),
        }
    }

    pub fn flush(&mut self) {
        if self.edited_at.take().is_some() {
            self.save();
        }
    }

    pub fn sync_on(&self) -> bool {
        match self.status() {
            Some(status) => status.state != crate::control::SyncState::Paused,
            None => self.settings.enabled,
        }
    }

    pub fn set_sync(&mut self, on: bool) {
        self.flush();
        self.settings.enabled = on;
        if self.running() {
            std::thread::spawn(move || send(&ControlRequest::new(if on { "resume" } else { "pause" })));
        } else {
            self.save();
        }
    }

    pub fn identify(&self, id: &str) {
        let id = id.to_string();
        std::thread::spawn(move || send(&ControlRequest::with_id("identify", &id)));
    }

    pub fn scan(&self) -> Scan {
        self.scan.lock().unwrap().clone()
    }

    pub fn start_scan(&self) {
        *self.scan.lock().unwrap() = Scan::Scanning;
        let (sink, repaint) = (self.scan.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = match send(&ControlRequest::new("discover")) {
                Ok(reply) if reply.ok => Scan::Done(reply.found.unwrap_or_default()),
                Ok(reply) => Scan::Failed(reply.message.unwrap_or_default()),
                // Not running: scan from here instead
                Err(_) => match crate::lights::lan::LanBus::open().and_then(|bus| crate::engine::discover(&bus)) {
                    Ok(found) => Scan::Done(found),
                    Err(e) => Scan::Failed(format!("{e:#}")),
                },
            };
            *sink.lock().unwrap() = result;
            repaint.request_repaint();
        });
    }
}
