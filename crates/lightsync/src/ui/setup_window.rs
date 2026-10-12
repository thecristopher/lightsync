//! The installer checklist and the uninstaller, one button per step.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Align, Color32, Layout, RichText, TextureHandle, TextureOptions, vec2};

use super::style;
use crate::panel::icon_rgba;
use crate::setup;

#[derive(Clone, PartialEq)]
enum Job {
    Idle,
    Working,
    Done,
    Failed(String),
}

fn run_job(job: &Arc<Mutex<Job>>, ctx: &egui::Context, work: impl FnOnce() -> anyhow::Result<()> + Send + 'static) {
    *job.lock().unwrap() = Job::Working;
    let (job, ctx) = (job.clone(), ctx.clone());
    std::thread::spawn(move || {
        let outcome = match work() {
            Ok(()) => Job::Done,
            Err(e) => Job::Failed(format!("{e:#}")),
        };
        *job.lock().unwrap() = outcome;
        ctx.request_repaint();
    });
}

pub fn badge(ctx: &egui::Context) -> TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([64, 64], &icon_rgba(64));
    ctx.load_texture("badge", image, TextureOptions::LINEAR)
}

fn header(ui: &mut egui::Ui, badge: &TextureHandle, title: &str, subtitle: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Image::new((badge.id(), vec2(52.0, 52.0))));
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.add_space(4.0);
            ui.label(RichText::new(title).font(style::semibold(22.0)).color(Color32::WHITE));
            ui.add(egui::Label::new(RichText::new(subtitle).color(style::SECONDARY)).wrap());
        });
    });
    ui.add_space(14.0);
}

/// A numbered step that turns into a green check once it's done.
fn step(ui: &mut egui::Ui, number: usize, done: bool, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    style::card(ui, |ui| {
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), egui::Sense::hover());
            let painter = ui.painter();
            if done {
                painter.circle_filled(rect.center(), 13.0, style::GOOD);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "✔",
                    egui::FontId::proportional(14.0),
                    Color32::WHITE,
                );
            } else {
                painter.circle_stroke(rect.center(), 12.0, egui::Stroke::new(1.5, style::SECONDARY));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    number.to_string(),
                    style::semibold(13.0),
                    style::SECONDARY,
                );
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(title).font(style::semibold(15.0)).color(Color32::WHITE));
                contents(ui);
            });
        });
    });
    ui.add_space(8.0);
}

fn note(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
}

pub struct SetupWindow {
    badge: TextureHandle,
    install: Arc<Mutex<Job>>,
    firewall: Arc<Mutex<Job>>,
    lights: Arc<Mutex<Option<(usize, usize)>>>,
    looking: bool,
}

/// Looks for lights in the background, so the last step can say what's out there. Only once the
/// firewall step is done: listening for replies before that makes Windows pop up its own prompt.
fn look_for_lights(lights: &Arc<Mutex<Option<(usize, usize)>>>, ctx: &egui::Context) {
    let (sink, repaint) = (lights.clone(), ctx.clone());
    std::thread::spawn(move || {
        let found = match crate::control::send(&crate::control::ControlRequest::new("discover")) {
            Ok(reply) => reply.found.unwrap_or_default(),
            Err(_) => {
                crate::lights::lan::LanBus::open().and_then(|bus| crate::engine::discover(&bus)).unwrap_or_default()
            }
        };
        let wifi = found.iter().filter(|f| f.link == "wifi").count();
        *sink.lock().unwrap() = Some((wifi, found.len() - wifi));
        repaint.request_repaint();
    });
}

impl SetupWindow {
    pub fn new(ctx: &egui::Context) -> Self {
        style::apply(ctx);
        Self {
            badge: badge(ctx),
            install: Arc::new(Mutex::new(if setup::is_installed() { Job::Done } else { Job::Idle })),
            firewall: Arc::new(Mutex::new(if setup::firewall_allowed() { Job::Done } else { Job::Idle })),
            lights: Arc::new(Mutex::new(None)),
            looking: false,
        }
    }
}

impl eframe::App for SetupWindow {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let install = self.install.lock().unwrap().clone();
        let firewall = self.firewall.lock().unwrap().clone();
        let installed = install == Job::Done;
        if firewall == Job::Done && !self.looking {
            self.looking = true;
            look_for_lights(&self.lights, &ctx);
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(style::PANEL).inner_margin(24)).show(ui, |ui| {
            header(ui, &self.badge, "Set Up LightSync", "Your Govee lights follow what's on your screen, over Wi-Fi or Bluetooth. Nothing goes through the cloud.");

            step(ui, 1, installed, "LightSync", |ui| {
                let mut text = String::from("Installs for you only, starts quietly in the tray when you sign in, and adds it to the Start menu. No administrator needed.");
                if installed && setup::installed_version().as_deref() != Some(env!("CARGO_PKG_VERSION")) {
                    text = format!("Version {} is installed. This is {}.", setup::installed_version().unwrap_or_default(), env!("CARGO_PKG_VERSION"));
                }
                note(ui, &text, style::SECONDARY);
                ui.add_space(4.0);
                ui.horizontal(|ui| match &install {
                    Job::Working => {
                        ui.spinner();
                        ui.label("Installing…");
                    }
                    Job::Done => {
                        ui.label(RichText::new("Installed.").color(style::GOOD));
                        if !setup::running_installed_copy() && ui.small_button("Reinstall").clicked() {
                            run_job(&self.install, &ctx, setup::install);
                        }
                    }
                    Job::Idle | Job::Failed(_) => {
                        if ui.button("Install").clicked() {
                            run_job(&self.install, &ctx, setup::install);
                        }
                    }
                });
                if let Job::Failed(reason) = &install {
                    note(ui, reason, style::WARNING);
                }
            });

            step(ui, 2, firewall == Job::Done, "Local network", |ui| {
                note(ui, "Lets your lights' replies through Windows Firewall, only for LightSync and only on the port Govee uses. Windows asks for permission once.", style::SECONDARY);
                ui.add_space(4.0);
                ui.horizontal(|ui| match &firewall {
                    Job::Working => {
                        ui.spinner();
                        ui.label("Waiting for Windows…");
                    }
                    Job::Done => {
                        ui.label(RichText::new("Allowed.").color(style::GOOD));
                    }
                    Job::Idle | Job::Failed(_) => {
                        if ui.button("Allow").clicked() {
                            run_job(&self.firewall, &ctx, setup::request_firewall);
                        }
                    }
                });
                if let Job::Failed(reason) = &firewall {
                    note(ui, reason, style::WARNING);
                }
            });

            let lights = *self.lights.lock().unwrap();
            step(ui, 3, lights.is_some_and(|(w, b)| w + b > 0), "Your lights", |ui| match lights {
                None if firewall != Job::Done => note(ui, "Once the local network is allowed, LightSync looks for your lights here.", style::FAINT),
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Looking for Govee lights…").color(style::SECONDARY));
                    });
                }
                Some((0, 0)) => note(ui, "None found yet. For Wi-Fi lights, open the Govee app, pick the light, tap the gear and switch on LAN Control. Bluetooth lights need to be in range and not connected to a phone.", style::WARNING),
                Some((wifi, bluetooth)) => {
                    note(ui, &format!("Found {wifi} over Wi-Fi and {bluetooth} over Bluetooth. You'll add the ones you want in the app."), style::SECONDARY);
                    if wifi == 0 {
                        note(ui, "Wi-Fi lights are much faster: switch on LAN Control for them in the Govee app.", style::FAINT);
                    }
                }
            });

            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let open = ui.add_enabled(installed, egui::Button::new(RichText::new("Open LightSync").strong()).min_size(vec2(160.0, 30.0)));
                if open.clicked() && setup::start_app(true).is_ok() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Close").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

pub struct UninstallWindow {
    badge: TextureHandle,
    job: Arc<Mutex<Job>>,
    keep_settings: bool,
}

impl UninstallWindow {
    pub fn new(ctx: &egui::Context) -> Self {
        style::apply(ctx);
        Self { badge: badge(ctx), job: Arc::new(Mutex::new(Job::Idle)), keep_settings: false }
    }
}

impl eframe::App for UninstallWindow {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let job = self.job.lock().unwrap().clone();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(style::PANEL).inner_margin(24)).show(ui, |ui| {
            header(
                ui,
                &self.badge,
                "Uninstall LightSync",
                "Your lights go back to how they were before LightSync took them.",
            );
            if job == Job::Done {
                style::card(ui, |ui| {
                    ui.label(RichText::new("LightSync is gone.").font(style::semibold(15.0)).color(style::GOOD));
                    let text = if self.keep_settings {
                        "Your settings are kept in case you install it again."
                    } else {
                        "Everything it put on this PC has been removed."
                    };
                    ui.label(RichText::new(text).color(style::SECONDARY));
                });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                return;
            }

            style::card(ui, |ui| {
                ui.label(RichText::new("This removes").font(style::semibold(15.0)).color(Color32::WHITE));
                for item in setup::removal_list() {
                    ui.add(egui::Label::new(RichText::new(format!("•  {item}")).color(style::SECONDARY)).wrap());
                }
            });
            ui.add_space(8.0);
            ui.checkbox(&mut self.keep_settings, "Keep my lights and settings for next time");
            if let Job::Failed(reason) = &job {
                note(ui, reason, style::WARNING);
            }
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if job == Job::Working {
                    ui.label("Removing…");
                    ui.spinner();
                } else {
                    let remove = egui::Button::new(RichText::new("Uninstall").strong().color(Color32::WHITE))
                        .fill(Color32::from_rgb(196, 43, 28))
                        .min_size(vec2(120.0, 30.0));
                    if ui.add(remove).clicked() {
                        let keep = self.keep_settings;
                        run_job(&self.job, &ctx, move || setup::uninstall(keep));
                    }
                    if ui.button("Cancel").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            });
        });
    }
}
