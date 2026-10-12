//! The windows: the main settings window, and the setup and uninstall checklists. Each runs as its
//! own process next to the tray, so closing a window gives back all its memory while sync carries on.

mod main_window;
mod model;
mod setup_window;
mod style;

use anyhow::{Result, anyhow};
use eframe::egui;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow};
use windows_core::{HSTRING, w};

use crate::panel::icon_rgba;

const MAIN_WINDOW_TITLE: &str = "LightSync";

pub fn run_main_window() -> Result<()> {
    // One window at a time: a second click just brings the open one forward
    let _single = unsafe { CreateMutexW(None, true, w!("Local\\LightSyncWindow"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        bring_forward(MAIN_WINDOW_TITLE);
        return Ok(());
    }
    run(MAIN_WINDOW_TITLE, [1120.0, 760.0], [900.0, 600.0], true, |ctx| Box::new(main_window::MainWindow::new(ctx)))
}

pub fn run_setup_window() -> Result<()> {
    run("Set Up LightSync", [580.0, 640.0], [540.0, 560.0], false, |ctx| Box::new(setup_window::SetupWindow::new(ctx)))
}

pub fn run_uninstall_window() -> Result<()> {
    run("Uninstall LightSync", [540.0, 440.0], [500.0, 400.0], false, |ctx| {
        Box::new(setup_window::UninstallWindow::new(ctx))
    })
}

fn run(
    title: &str,
    size: [f32; 2],
    min_size: [f32; 2],
    resizable: bool,
    make: impl FnOnce(&egui::Context) -> Box<dyn eframe::App> + 'static,
) -> Result<()> {
    let icon = egui::IconData { rgba: icon_rgba(64), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(size)
            .with_min_inner_size(min_size)
            .with_resizable(resizable)
            .with_maximize_button(resizable)
            .with_icon(icon),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(title, options, Box::new(move |cc| Ok(make(&cc.egui_ctx)))).map_err(|e| anyhow!("{e}"))
}

fn bring_forward(title: &str) {
    unsafe {
        if let Ok(window) = FindWindowW(None, &HSTRING::from(title)) {
            let _ = ShowWindow(window, SW_RESTORE);
            let _ = SetForegroundWindow(window);
        }
    }
}
