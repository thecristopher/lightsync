// A GUI app, so double clicking doesn't flash a console. Run from a terminal it borrows that one.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod capture;
mod cli;
mod console;
mod control;
mod engine;
mod lights;
mod log;
mod panel;
mod setup;
mod tray;
mod ui;
mod watch;

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::log::log;

const USAGE: &str = "\
LightSync: sync your Govee lights with your screen

  LightSync                       open the app (offers to set itself up the first time)
  LightSync status                what's going on with sync and every light
  LightSync pause | resume        stop or start syncing (lights go back to how they were)
  LightSync discover              find Govee lights over Wi-Fi and Bluetooth
  LightSync identify <light>      flash a light's first (red) and last (blue) segment
  LightSync install [--quiet]     install for you on this PC (no admin needed)
  LightSync uninstall [--quiet] [--keep-settings]
                                  add --window to either for the checklist window instead
  LightSync start                 run the engine in this terminal instead of the tray
  LightSync quit                  quit the running app, handing every light back";

fn main() {
    console::attach();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let quiet = flag("--quiet");
    let positional: Vec<&str> = args.iter().skip(1).filter(|a| !a.starts_with("--")).map(String::as_str).collect();
    // Opened from Explorer, Settings or the tray there's no console, so install and uninstall show
    // their window. From a terminal they act straight away, unless --window asks for the window.
    let gui = (!console::attached() || flag("--window")) && !quiet;

    let code = match args.first().map(String::as_str) {
        None => start(),
        Some(setup::BACKGROUND_FLAG) => report(tray::run(false), false),
        Some("window") => report(ui::run_main_window(), false),
        Some("setup") => report(ui::run_setup_window(), false),
        Some("install") if gui => report(ui::run_setup_window(), false),
        Some("install") => report(cli::install(quiet), quiet),
        Some("uninstall") if gui => report(ui::run_uninstall_window(), false),
        Some("uninstall") => report(cli::uninstall(quiet, flag("--keep-settings")), quiet),
        Some("firewall") if flag("--elevated") => report(setup::firewall(positional.first() != Some(&"remove")), true),
        Some("status") => report(cli::status(), true),
        Some(command @ ("pause" | "resume" | "toggle")) => report(cli::simple(command), true),
        Some("discover") => report(cli::discover(), true),
        Some("identify") => report(cli::identify(positional.first().copied()), true),
        Some("start") => report(run_in_terminal(), true),
        Some("quit") => {
            setup::ask_app_to_quit();
            0
        }
        Some("help" | "--help" | "-h") => {
            console::tell(USAGE, false);
            0
        }
        Some(other) => {
            console::warn(&format!("I don't know \"{other}\".\n\n{USAGE}"), false);
            2
        }
    };
    std::process::exit(code);
}

/// Opened by hand: set up first if this copy isn't the installed one, otherwise the tray plus the window.
fn start() -> i32 {
    if console::attached() {
        console::tell(USAGE, true);
        return 0;
    }
    if !setup::running_installed_copy() {
        return report(ui::run_setup_window(), false);
    }
    if tray::already_running() {
        tray::open_window();
        return 0;
    }
    report(tray::run(true), false)
}

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_ctrl_c(_: u32) -> windows_core::BOOL {
    STOP.store(true, Ordering::Relaxed);
    true.into()
}

/// The engine without the tray, for watching the log scroll by. Ctrl+C hands the lights back and leaves.
fn run_in_terminal() -> Result<()> {
    if tray::already_running() {
        bail!("LightSync is already running in the tray. Quit it from the tray icon first.");
    }
    let engine = std::sync::Arc::new(engine::Engine::start());
    control::serve(engine.handler())?;
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl_c), true)? };
    // `LightSync quit` and the uninstaller reach a terminal engine the same way as the tray
    let quit = unsafe { CreateEventW(None, true, false, setup::QUIT_EVENT)? };
    log!("Press Ctrl+C to stop.");
    while !STOP.load(Ordering::Relaxed) && unsafe { WaitForSingleObject(quit, 100) } != WAIT_OBJECT_0 {}
    drop(engine);
    unsafe {
        let _ = CloseHandle(quit);
    }
    Ok(())
}

fn report(result: Result<()>, quiet: bool) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            log::record(&format!("{e:#}"));
            console::warn(&format!("{e:#}"), quiet);
            1
        }
    }
}
