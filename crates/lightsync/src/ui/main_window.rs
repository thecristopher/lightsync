//! The main window: your lights down the side, and everything about the selected one on the right,
//! with a live picture of which part of the screen feeds which segment.

use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Label, Layout, Margin, Rect, RichText, Sense, Stroke, TextureHandle,
    pos2, vec2,
};
use lightsync_core::color::ColorSettings;
use lightsync_core::govee::{BleColorCommand, Rgb};
use lightsync_core::settings::{BleMode, Connection, LanMode, Light, LightsOff, OnExit, short_id};
use lightsync_core::zones::{Layout as ZoneLayout, Sampling, Side, split_counts};

use super::model::{Model, Scan};
use super::setup_window::badge;
use super::style::{self, QuitSignal};
use crate::control::{EngineStatus, LightStatus, LinkState, SyncState};
use crate::panel::{CREDITS, subtitle, title};

#[derive(Clone, Debug, PartialEq)]
enum Page {
    Light(String),
    Add,
    General,
}

pub struct MainWindow {
    model: Model,
    page: Page,
    badge: TextureHandle,
    monitors: Vec<String>,
    quit: QuitSignal,
}

impl MainWindow {
    pub fn new(ctx: &egui::Context) -> Self {
        style::apply(ctx);
        let model = Model::new(ctx.clone());
        let page = match model.settings.lights.first() {
            Some(light) => Page::Light(light.id.clone()),
            None => Page::Add,
        };
        Self { model, page, badge: badge(ctx), monitors: crate::capture::monitors(), quit: QuitSignal::open() }
    }
}

impl eframe::App for MainWindow {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // The uninstaller asks every LightSync window to leave so it can remove the files
        if self.quit.raised() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            self.model.flush();
        }
        self.model.tick();
        let status = self.model.status();

        egui::Panel::left("lights")
            .exact_size(270.0)
            .resizable(false)
            .frame(Frame::new().fill(style::STAGE).inner_margin(Margin::same(0)))
            .show(ui, |ui| self.sidebar(ui, status.as_ref()));
        egui::CentralPanel::default().frame(Frame::new().fill(style::PANEL)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                Frame::new().inner_margin(Margin::symmetric(28, 22)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(problem) = &self.model.problem {
                        style::card(ui, |ui| {
                            ui.add(Label::new(RichText::new(problem).color(style::WARNING)).wrap());
                        });
                        ui.add_space(8.0);
                    }
                    match self.page.clone() {
                        Page::Light(id) => self.light_page(ui, &id, status.as_ref()),
                        Page::Add => self.add_page(ui),
                        Page::General => self.general_page(ui, status.as_ref()),
                    }
                });
            });
        });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

// Sidebar

impl MainWindow {
    fn sidebar(&mut self, ui: &mut egui::Ui, status: Option<&EngineStatus>) {
        Frame::new().inner_margin(Margin::symmetric(16, 18)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Image::new((self.badge.id(), vec2(36.0, 36.0))));
                ui.vertical(|ui| {
                    ui.label(RichText::new("LightSync").font(style::semibold(18.0)).color(Color32::WHITE));
                    ui.label(RichText::new(title(status)).size(12.0).color(style::SECONDARY));
                });
            });
            ui.add_space(12.0);

            // The one switch that matters most
            style::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Sync with screen").font(style::semibold(14.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let mut on = self.model.sync_on();
                        if toggle(ui, &mut on).clicked() {
                            self.model.set_sync(on);
                        }
                    });
                });
                ui.add(Label::new(RichText::new(subtitle(status)).size(12.0).color(style::FAINT)).wrap());
                if status.is_none() && crate::setup::is_installed() && ui.small_button("Start LightSync").clicked() {
                    let _ = crate::setup::start_app(false);
                }
            });

            ui.add_space(14.0);
            ui.label(RichText::new("LIGHTS").size(11.0).color(style::FAINT));
            ui.add_space(2.0);
            let lights: Vec<(String, String, bool)> =
                self.model.settings.lights.iter().map(|l| (l.id.clone(), l.name.clone(), l.enabled)).collect();
            for (id, name, enabled) in lights {
                let live = status.and_then(|s| s.lights.iter().find(|l| l.id == id));
                let selected = self.page == Page::Light(id.clone());
                if light_row(ui, &name, enabled, live, selected).clicked() {
                    self.page = Page::Light(id);
                }
            }
            ui.add_space(6.0);
            if nav_row(ui, "+  Add lights", self.page == Page::Add).clicked() {
                self.page = Page::Add;
            }

            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                ui.label(
                    RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).size(11.0).color(style::FAINT),
                );
                ui.add_space(4.0);
                if nav_row(ui, "⚙  Screen & colour", self.page == Page::General).clicked() {
                    self.page = Page::General;
                }
            });
        });
    }
}

fn light_row(
    ui: &mut egui::Ui,
    name: &str,
    enabled: bool,
    live: Option<&LightStatus>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    let painter = ui.painter();
    if selected || response.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::same(8),
            if selected { style::CARD } else { Color32::from_white_alpha(6) },
        );
    }
    let dot = match (enabled, live.map(|l| l.state)) {
        (false, _) => Color32::from_gray(90),
        (true, Some(LinkState::Live)) => style::GOOD,
        (true, Some(LinkState::Trouble)) => style::WARNING,
        (true, _) => Color32::from_gray(150),
    };
    painter.circle_filled(pos2(rect.left() + 14.0, rect.top() + 16.0), 4.0, dot);
    let text_color = if enabled { Color32::WHITE } else { style::FAINT };
    painter.text(
        pos2(rect.left() + 26.0, rect.top() + 16.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(14.0),
        text_color,
    );
    // What the light is showing right now, as a little strip
    let strip =
        Rect::from_min_max(pos2(rect.left() + 26.0, rect.top() + 29.0), pos2(rect.right() - 12.0, rect.top() + 36.0));
    match live.map(|l| l.colors.as_slice()).filter(|c| !c.is_empty()) {
        Some(colors) => {
            let w = strip.width() / colors.len() as f32;
            for (i, c) in colors.iter().enumerate() {
                let cell =
                    Rect::from_min_size(pos2(strip.left() + i as f32 * w, strip.top()), vec2(w + 0.5, strip.height()));
                painter.rect_filled(cell, CornerRadius::ZERO, rgb(*c));
            }
        }
        None => {
            painter.rect_filled(strip, CornerRadius::same(2), Color32::from_white_alpha(10));
        }
    }
    response
}

fn nav_row(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(8),
            if selected { style::CARD } else { Color32::from_white_alpha(6) },
        );
    }
    ui.painter().text(
        pos2(rect.left() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(14.0),
        style::SECONDARY,
    );
    response
}

/// An iOS-style switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let painter = ui.painter();
    let track = Color32::from_gray(80).lerp_to_gamma(style::GOOD, t);
    painter.rect_filled(rect, CornerRadius::same(11), track);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    painter.circle_filled(pos2(x, rect.center().y), 8.5, Color32::WHITE);
    response
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

// Light page

impl MainWindow {
    fn aspect(status: Option<&EngineStatus>) -> f32 {
        match status.map(|s| s.screen) {
            Some((w, h)) if w > 0 && h > 0 => w as f32 / h as f32,
            _ => 16.0 / 9.0,
        }
    }

    fn light_page(&mut self, ui: &mut egui::Ui, id: &str, status: Option<&EngineStatus>) {
        let aspect = Self::aspect(status);
        let defaults = self.model.settings.color.clone();
        let live = status.and_then(|s| s.lights.iter().find(|l| l.id == id)).cloned();
        let Some(index) = self.model.settings.lights.iter().position(|l| l.id == id) else {
            self.page = Page::Add;
            return;
        };
        let mut edited = false;
        let remove;
        let mut identify = false;
        {
            let light = &mut self.model.settings.lights[index];

            // Title row
            ui.horizontal(|ui| {
                let name = egui::TextEdit::singleline(&mut light.name)
                    .font(style::semibold(22.0))
                    .frame(Frame::NONE)
                    .desired_width(320.0);
                edited |= ui.add(name).changed();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    edited |= toggle(ui, &mut light.enabled).on_hover_text("Sync this light").clicked();
                    ui.add_space(8.0);
                    identify = ui
                        .add_enabled(light.enabled && live.is_some(), egui::Button::new("Identify"))
                        .on_hover_text(
                            "Red on the first segment, blue on the last, and a white dot walking from first to last",
                        )
                        .clicked();
                });
            });
            let link = match &light.connection {
                Connection::Lan { ip, sku, .. } => format!("{sku} · Wi-Fi {ip}"),
                Connection::Ble { address, sku, .. } => format!("{sku} · Bluetooth {address}"),
            };
            let (state, color) = match (&live, light.enabled) {
                (_, false) => ("Not syncing".to_string(), style::FAINT),
                (Some(l), _) if l.state == LinkState::Live => {
                    (format!("Live · {:.0} updates a second", l.sends_per_second), style::GOOD)
                }
                (Some(l), _) if l.state == LinkState::Trouble => (l.detail.clone(), style::WARNING),
                (Some(l), _) if l.state == LinkState::Off => ("Off while the screen is off".to_string(), style::FAINT),
                (Some(_), _) => ("Connecting…".to_string(), style::SECONDARY),
                (None, _) => ("Waiting for sync to start".to_string(), style::FAINT),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(link).color(style::SECONDARY));
                ui.label(RichText::new("·").color(style::FAINT));
                ui.label(RichText::new(state).color(color));
            });
            ui.add_space(10.0);

            // Live preview of the zones
            let zones = light.layout.zones(aspect, light.reverse, light.offset);
            preview(ui, aspect, &zones, live.as_ref().map(|l| l.colors.as_slice()).unwrap_or(&[]));
            ui.add_space(6.0);

            section(ui, "Layout");
            style::card(ui, |ui| edited |= layout_editor(ui, light, aspect));
            footnote(
                ui,
                "Segments must match the light. Use Identify: red should sit where the strip starts and blue where it ends.",
            );

            section(ui, "Connection");
            style::card(ui, |ui| edited |= connection_editor(ui, light));

            section(ui, "Colour");
            style::card(ui, |ui| {
                let mut own = light.color.is_some();
                if ui.checkbox(&mut own, "Tune this light on its own").changed() {
                    light.color = own.then(|| defaults.clone());
                    edited = true;
                }
                match &mut light.color {
                    Some(color) => {
                        ui.add_space(6.0);
                        edited |= color_editor(ui, color);
                    }
                    None => {
                        ui.label(
                            RichText::new("Following the shared colour settings in Screen & colour.")
                                .color(style::FAINT),
                        );
                    }
                }
            });

            ui.add_space(18.0);
            remove = ui
                .add(
                    egui::Button::new(RichText::new("Remove this light").color(Color32::from_rgb(255, 105, 97)))
                        .frame(false),
                )
                .clicked();
        }
        if identify {
            self.model.identify(id);
        }
        if remove {
            self.model.settings.lights.remove(index);
            self.page = match self.model.settings.lights.first() {
                Some(l) => Page::Light(l.id.clone()),
                None => Page::Add,
            };
            edited = true;
        }
        if edited {
            self.model.edited();
        }
    }
}

/// The screen with each zone drawn in the colour its segment is showing, numbered where the strip starts and ends.
fn preview(ui: &mut egui::Ui, aspect: f32, zones: &[[f32; 4]], colors: &[Rgb]) {
    let width = ui.available_width().min(560.0);
    let size = vec2(width, (width / aspect).clamp(120.0, 300.0));
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), size.y), Sense::hover());
    let screen = Rect::from_center_size(outer.center(), vec2(size.y * aspect, size.y).min(size));
    let painter = ui.painter();
    painter.rect(
        screen,
        CornerRadius::same(8),
        Color32::from_rgb(14, 15, 18),
        Stroke::new(1.0, Color32::from_white_alpha(30)),
        egui::StrokeKind::Outside,
    );
    for (i, z) in zones.iter().enumerate() {
        let r = Rect::from_min_max(
            pos2(screen.left() + z[0] * screen.width(), screen.top() + z[1] * screen.height()),
            pos2(screen.left() + z[2] * screen.width(), screen.top() + z[3] * screen.height()),
        )
        .shrink(1.0);
        match colors.get(i) {
            Some(c) => {
                painter.rect_filled(r, CornerRadius::same(2), rgb(*c));
            }
            None => {
                painter.rect_stroke(
                    r,
                    CornerRadius::same(2),
                    Stroke::new(1.0, Color32::from_white_alpha(50)),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if i == 0 || i + 1 == zones.len() {
            let label = if i == 0 { "1".to_string() } else { zones.len().to_string() };
            let marker = if i == 0 { Color32::from_rgb(255, 80, 80) } else { Color32::from_rgb(90, 140, 255) };
            painter.circle_filled(r.center(), 9.0, Color32::from_black_alpha(170));
            painter.text(r.center(), egui::Align2::CENTER_CENTER, label, style::semibold(11.0), marker);
        }
    }
}

fn layout_editor(ui: &mut egui::Ui, light: &mut Light, aspect: f32) -> bool {
    let mut edited = false;
    let kinds = [
        (
            "Screen edges",
            ZoneLayout::edges(&[Side::Left, Side::Top, Side::Right], light.layout.segments().max(3) as u32, aspect),
        ),
        ("Whole screen", ZoneLayout::default()),
        ("Grid", ZoneLayout::Grid { rows: 2, cols: 3, snake: false }),
        ("Bands", ZoneLayout::Columns { count: light.layout.segments().max(2) as u32, vertical: false }),
    ];
    ui.horizontal(|ui| {
        ui.label("Follows");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for (name, layout) in kinds.into_iter().rev() {
                let current = light.layout.kind() == name;
                if ui.selectable_label(current, name).clicked() && !current {
                    light.layout = layout;
                    edited = true;
                }
            }
        });
    });
    ui.add_space(4.0);
    match &mut light.layout {
        ZoneLayout::Single { region } => {
            let mut center_only = *region != [0.0, 0.0, 1.0, 1.0];
            if ui.checkbox(&mut center_only, "Only the middle of the screen").changed() {
                *region = if center_only { [0.25, 0.25, 0.75, 0.75] } else { [0.0, 0.0, 1.0, 1.0] };
                edited = true;
            }
        }
        ZoneLayout::Edges { sides, counts, depth, clockwise } => {
            let mut chosen: Vec<Side> = sides.clone();
            ui.horizontal(|ui| {
                ui.label("Sides");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for side in Side::ALL.iter().rev() {
                        let mut on = chosen.contains(side);
                        if ui.checkbox(&mut on, side.label()).changed() {
                            if on {
                                chosen.push(*side);
                            } else {
                                chosen.retain(|s| s != side);
                            }
                        }
                    }
                });
            });
            let mut direction = *clockwise;
            ui.horizontal(|ui| {
                ui.label("Runs");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.selectable_value(&mut direction, false, "Counter-clockwise");
                    ui.selectable_value(&mut direction, true, "Clockwise");
                });
            });
            let mut total: u32 = counts.iter().sum();
            ui.horizontal(|ui| {
                ui.label("Segments");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(egui::DragValue::new(&mut total).range(1..=255));
                });
            });
            // Keep the sides in the order a strip actually runs, so the first segment is where it starts
            let order: [Side; 4] = if direction {
                [Side::Left, Side::Top, Side::Right, Side::Bottom]
            } else {
                [Side::Right, Side::Top, Side::Left, Side::Bottom]
            };
            let ordered: Vec<Side> = order.iter().copied().filter(|s| chosen.contains(s)).collect();
            if !ordered.is_empty()
                && (ordered != *sides || direction != *clockwise || total != counts.iter().sum::<u32>())
            {
                let weights: Vec<f32> =
                    ordered.iter().map(|s| if matches!(s, Side::Top | Side::Bottom) { aspect } else { 1.0 }).collect();
                *counts = split_counts(total, &weights);
                *sides = ordered;
                *clockwise = direction;
                edited = true;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Per side").color(style::FAINT));
                for (side, count) in sides.iter().zip(counts.iter_mut()) {
                    ui.label(RichText::new(side.label()).color(style::SECONDARY));
                    edited |= ui.add(egui::DragValue::new(count).range(1..=200)).changed();
                }
            });
            edited |= slider_row(ui, "Depth", depth, 0.03..=0.5, |v| format!("{:.0}%", v * 100.0));
        }
        ZoneLayout::Grid { rows, cols, snake } => {
            ui.horizontal(|ui| {
                ui.label("Rows × columns");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    edited |= ui.add(egui::DragValue::new(cols).range(1..=32)).changed();
                    ui.label("×");
                    edited |= ui.add(egui::DragValue::new(rows).range(1..=32)).changed();
                });
            });
            edited |= ui.checkbox(snake, "Every other row runs backwards (zigzag wiring)").changed();
        }
        ZoneLayout::Columns { count, vertical } => {
            ui.horizontal(|ui| {
                ui.label("Bands");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    edited |= ui.add(egui::DragValue::new(count).range(1..=255)).changed();
                });
            });
            edited |= ui.checkbox(vertical, "Stacked top to bottom (for upright bars and lamps)").changed();
        }
    }
    if light.layout.segments() > 1 {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            edited |= ui.checkbox(&mut light.reverse, "Reverse").changed();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                edited |= ui.add(egui::DragValue::new(&mut light.offset).range(-255..=255)).changed();
                ui.label("Shift start by");
            });
        });
    }
    edited
}

fn connection_editor(ui: &mut egui::Ui, light: &mut Light) -> bool {
    let mut edited = false;
    match &mut light.connection {
        Connection::Lan { mode, gradient, .. } => {
            ui.horizontal(|ui| {
                ui.label("Mode");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    edited |= ui.selectable_value(mode, LanMode::Color, "Whole light").changed();
                    edited |= ui.selectable_value(mode, LanMode::Razer, "Per segment").changed();
                });
            });
            if *mode == LanMode::Razer {
                edited |= ui.checkbox(gradient, "Let the light blend between segments").changed();
            }
        }
        Connection::Ble { mode, command, .. } => {
            ui.horizontal(|ui| {
                ui.label("Mode");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    edited |= ui.selectable_value(mode, BleMode::Segments, "Per segment").changed();
                    edited |= ui.selectable_value(mode, BleMode::Single, "Whole light").changed();
                });
            });
            ui.horizontal(|ui| {
                ui.label("Colour command");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("command")
                        .selected_text(match command {
                            BleColorCommand::Modern => "Modern",
                            BleColorCommand::Legacy => "Legacy",
                            BleColorCommand::Segment => "Segmented",
                        })
                        .show_ui(ui, |ui| {
                            edited |=
                                ui.selectable_value(command, BleColorCommand::Modern, "Modern (most lights)").changed();
                            edited |= ui
                                .selectable_value(command, BleColorCommand::Legacy, "Legacy (older lights)")
                                .changed();
                            edited |=
                                ui.selectable_value(command, BleColorCommand::Segment, "Segmented (RGBIC)").changed();
                        });
                });
            });
            ui.label(
                RichText::new("If the colour doesn't change, try another colour command.")
                    .size(12.0)
                    .color(style::FAINT),
            );
        }
    }
    let mut fps = light.max_fps;
    edited |= slider_row(ui, "Most updates a second", &mut fps, 1.0..=60.0, |v| format!("{v:.0}"));
    light.max_fps = fps;
    ui.horizontal(|ui| {
        ui.label("When LightSync stops");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            egui::ComboBox::from_id_salt("on_exit")
                .selected_text(match light.on_exit {
                    OnExit::Restore => "Put it back",
                    OnExit::Off => "Turn it off",
                    OnExit::Keep => "Leave it",
                })
                .show_ui(ui, |ui| {
                    edited |=
                        ui.selectable_value(&mut light.on_exit, OnExit::Restore, "Put it back as it was").changed();
                    edited |= ui.selectable_value(&mut light.on_exit, OnExit::Off, "Turn it off").changed();
                    edited |= ui.selectable_value(&mut light.on_exit, OnExit::Keep, "Leave the last colour").changed();
                });
        });
    });
    edited
}

fn color_editor(ui: &mut egui::Ui, color: &mut ColorSettings) -> bool {
    let mut edited = false;
    edited |= slider_row(ui, "Brightness", &mut color.brightness, 0.0..=1.0, |v| format!("{:.0}%", v * 100.0));
    edited |= slider_row(ui, "Saturation", &mut color.saturation, 0.0..=2.0, |v| format!("{:.0}%", v * 100.0));
    edited |= slider_row(ui, "Contrast (gamma)", &mut color.gamma, 0.5..=2.5, |v| format!("{v:.2}"));
    edited |= slider_row(ui, "Smoothing", &mut color.smoothing_ms, 0.0..=1000.0, |v| format!("{v:.0} ms"));
    let [r, g, b] = &mut color.white_balance;
    edited |= slider_row(ui, "Red balance", r, 0.3..=1.0, |v| format!("{:.0}%", v * 100.0));
    edited |= slider_row(ui, "Green balance", g, 0.3..=1.0, |v| format!("{:.0}%", v * 100.0));
    edited |= slider_row(ui, "Blue balance", b, 0.3..=1.0, |v| format!("{:.0}%", v * 100.0));
    let mut black = color.black_level as f32;
    if slider_row(ui, "Dark scenes turn off below", &mut black, 0.0..=60.0, |v| format!("{v:.0}")) {
        color.black_level = black as u8;
        edited = true;
    }
    let mut delta = color.min_delta as f32;
    if slider_row(ui, "Ignore changes smaller than", &mut delta, 1.0..=30.0, |v| format!("{v:.0}")) {
        color.min_delta = delta as u8;
        edited = true;
    }
    ui.horizontal(|ui| {
        ui.label("Measure zones by");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            edited |= ui.selectable_value(&mut color.sampling, Sampling::Mean, "Average").changed();
            edited |= ui.selectable_value(&mut color.sampling, Sampling::Vivid, "Vivid colours").changed();
        });
    });
    if ui.small_button("Reset colour").clicked() {
        *color = ColorSettings::default();
        edited = true;
    }
    edited
}

/// A labeled slider with its value on the right. Double click the label to see the default.
fn slider_row(
    ui: &mut egui::Ui,
    title: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    show: impl Fn(f32) -> String,
) -> bool {
    ui.horizontal(|ui| {
        ui.label(title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(show(*value)).monospace().color(style::SECONDARY));
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    let changed = ui.add(egui::Slider::new(value, range).show_value(false)).changed();
    ui.add_space(2.0);
    changed
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).font(style::semibold(14.0)).color(style::SECONDARY));
    ui.add_space(4.0);
}

fn footnote(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.add(Label::new(RichText::new(text).size(12.0).color(style::FAINT)).wrap());
}

// Add page

impl MainWindow {
    fn add_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Add lights").font(style::semibold(22.0)).color(Color32::WHITE));
        ui.add(Label::new(RichText::new("LightSync looks for Govee lights on your network and over Bluetooth. Wi-Fi lights are fast enough for every segment; Bluetooth lights follow the screen with one colour.").color(style::SECONDARY)).wrap());
        ui.add_space(10.0);
        let scan = self.model.scan();
        ui.horizontal(|ui| {
            if scan == Scan::Scanning {
                ui.spinner();
                ui.label("Looking for lights (about 5 seconds)…");
            } else if ui.button(RichText::new("Look for lights").strong()).clicked() {
                self.model.start_scan();
            }
        });
        if scan == Scan::Idle {
            self.model.start_scan();
        }
        let aspect = Self::aspect(self.model.status().as_ref());
        match scan {
            Scan::Failed(reason) => {
                ui.add(Label::new(RichText::new(reason).color(style::WARNING)).wrap());
            }
            Scan::Done(found) => {
                let wifi_ids: Vec<String> =
                    found.iter().filter(|f| f.link == "wifi").map(|f| short_id(&f.id)).collect();
                for (heading, link) in [("Wi-Fi", "wifi"), ("Bluetooth", "bluetooth")] {
                    let group: Vec<_> = found.iter().filter(|f| f.link == link).collect();
                    section(ui, heading);
                    style::card(ui, |ui| {
                        if group.is_empty() {
                            let text = if link == "wifi" {
                                "None found. In the Govee app, open the light's settings (the gear) and switch on LAN Control. The PC and the light must be on the same network."
                            } else {
                                "None found. Bluetooth lights need to be close by and not connected to a phone."
                            };
                            ui.add(Label::new(RichText::new(text).color(style::FAINT)).wrap());
                        }
                        for f in group {
                            let added = self.model.settings.lights.iter().any(|l| l.id.eq_ignore_ascii_case(&f.id));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&f.sku).font(style::semibold(14.0)));
                                let detail = match f.rssi {
                                    Some(rssi) => format!("{} · signal {rssi} dBm", f.address),
                                    None => f.address.clone(),
                                };
                                ui.label(RichText::new(detail).color(style::FAINT));
                                if link == "bluetooth" && wifi_ids.contains(&short_id(&f.id)) {
                                    ui.label(RichText::new("also on Wi-Fi, use that").size(12.0).color(style::WARNING));
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if added {
                                        ui.label(RichText::new("Added").color(style::GOOD));
                                    } else if ui.button("Add").clicked() {
                                        let light = if link == "wifi" {
                                            Light::lan(&f.id, &f.sku, &f.address, aspect)
                                        } else {
                                            Light::ble(&f.address, &f.sku)
                                        };
                                        self.page = Page::Light(light.id.clone());
                                        self.model.settings.lights.push(light);
                                        self.model.edited();
                                    }
                                });
                            });
                        }
                    });
                }
            }
            _ => {}
        }
    }

    // General page

    fn general_page(&mut self, ui: &mut egui::Ui, status: Option<&EngineStatus>) {
        ui.label(RichText::new("Screen & colour").font(style::semibold(22.0)).color(Color32::WHITE));
        let mut edited = false;
        let settings = &mut self.model.settings;
        let (capture, lights_off) = (&mut settings.capture, &mut settings.lights_off);

        section(ui, "Screen");
        style::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Display");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let current = self
                        .monitors
                        .get(capture.monitor as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("Display {}", capture.monitor + 1));
                    egui::ComboBox::from_id_salt("monitor").selected_text(current).width(240.0).show_ui(ui, |ui| {
                        for (i, name) in self.monitors.iter().enumerate() {
                            edited |= ui.selectable_value(&mut capture.monitor, i as u32, name).changed();
                        }
                    });
                });
            });
            edited |= slider_row(ui, "Most analyses a second", &mut capture.max_fps, 5.0..=60.0, |v| format!("{v:.0}"));
            let mut detail = capture.detail as f32;
            if slider_row(ui, "Detail", &mut detail, 32.0..=320.0, |v| format!("{v:.0} px")) {
                capture.detail = detail as u32;
                edited = true;
            }
            edited |= ui.checkbox(&mut capture.letterbox, "Ignore black bars in films").changed();
            ui.horizontal(|ui| {
                ui.label("Turn the lights off");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = |choice: LightsOff| match choice {
                        LightsOff::Never => "Never",
                        LightsOff::ScreenOff => "When the screen turns off",
                        LightsOff::ScreenOffOrLocked => "When the screen turns off or I lock the PC",
                    };
                    egui::ComboBox::from_id_salt("lights_off").selected_text(label(*lights_off)).width(300.0).show_ui(
                        ui,
                        |ui| {
                            for choice in [LightsOff::ScreenOff, LightsOff::ScreenOffOrLocked, LightsOff::Never] {
                                edited |= ui.selectable_value(lights_off, choice, label(choice)).changed();
                            }
                        },
                    );
                });
            });
        });
        let note = match status {
            Some(s) if s.state == SyncState::Running && s.fps < 0.5 => {
                "The screen is still, so LightSync is resting.".to_string()
            }
            Some(s) if s.state == SyncState::Running => {
                format!("Analysing {:.0} times a second right now. A still screen costs nothing.", s.fps)
            }
            _ => "A still screen costs nothing: LightSync only works when the picture changes.".into(),
        };
        footnote(ui, &note);

        section(ui, "Colour for every light");
        style::card(ui, |ui| edited |= color_editor(ui, &mut self.model.settings.color));
        footnote(ui, "A light can have its own colour tuning on its page.");

        section(ui, "App");
        style::card(ui, |ui| {
            if crate::setup::is_installed() {
                let mut at_sign_in = crate::setup::starts_at_sign_in();
                if ui.checkbox(&mut at_sign_in, "Start when I sign in").changed() {
                    let _ = crate::setup::set_start_at_sign_in(at_sign_in);
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Set Up…").clicked() {
                    run_self("setup");
                }
                if ui.button("Open log").clicked() {
                    let _ = std::process::Command::new("notepad.exe").arg(crate::log::path()).spawn();
                }
                if ui.button("Uninstall…").clicked() {
                    run_self("uninstall");
                }
            });
        });
        ui.add_space(12.0);
        ui.add(
            Label::new(
                RichText::new(format!("LightSync {}. {CREDITS}", env!("CARGO_PKG_VERSION")))
                    .size(11.0)
                    .color(style::FAINT),
            )
            .wrap(),
        );
        if edited {
            self.model.edited();
        }
    }
}

fn run_self(argument: &str) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(argument).spawn();
    }
}
