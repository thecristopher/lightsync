//! The tray icon: the app's home while no window is open, and where the engine lives.

use std::cell::RefCell;
use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, w};

use crate::engine::Engine;
use crate::log::log;
use crate::panel::{CREDITS, icon_rgba, subtitle, title};
use crate::setup::QUIT_EVENT;

const WM_TRAY: u32 = WM_APP + 1;
const TIMER_STATUS: usize = 1;
const MENU_OPEN: usize = 10;
const MENU_SYNC: usize = 11;
const MENU_ABOUT: usize = 12;
const MENU_SETUP: usize = 13;
const MENU_LOG: usize = 14;
const MENU_UNINSTALL: usize = 15;
const MENU_QUIT: usize = 16;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

struct App {
    engine: Arc<Engine>,
    icon: HICON,
    taskbar_created: u32,
}

/// True when another copy already has the tray in this session.
pub fn already_running() -> bool {
    unsafe {
        let probe = CreateMutexW(None, false, w!("Local\\LightSyncApp"));
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        if let Ok(handle) = probe {
            let _ = CloseHandle(handle);
        }
        exists
    }
}

/// Opens the main window, or brings it forward. It's its own process, so closing it gives back all
/// its memory while the tray keeps syncing.
pub fn open_window() {
    run_self("window");
}

fn run_self(argument: &str) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).arg(argument).spawn();
    }
}

pub fn run(show_window: bool) -> Result<()> {
    unsafe {
        let _single = CreateMutexW(None, true, w!("Local\\LightSyncApp"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log!("already running");
            if show_window {
                open_window();
            }
            return Ok(());
        }
        let quit = CreateEventW(None, true, false, QUIT_EVENT)?;

        let engine = Arc::new(Engine::start());
        if let Err(e) = crate::control::serve(engine.handler()) {
            log!("no control pipe, so the window and the commands won't reach this engine: {e:#}");
        }

        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("LightSyncTray"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // Not "LightSync", so finding the main window by its title never lands on this hidden one
        let window = CreateWindowExW(
            Default::default(),
            w!("LightSyncTray"),
            w!("LightSync Tray"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        APP.with(|app| {
            *app.borrow_mut() =
                Some(App { engine, icon: make_icon(), taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")) })
        });
        add_icon(window, NIM_ADD);
        SetTimer(Some(window), TIMER_STATUS, 2000, None);
        log!("tray running");
        if show_window {
            open_window();
        }

        // The uninstaller (or a newer install) asks us to leave through this event
        let (window_id, quit_id) = (window.0 as isize, quit.0 as isize);
        std::thread::spawn(move || {
            WaitForSingleObject(HANDLE(quit_id as _), INFINITE);
            let _ = PostMessageW(Some(HWND(window_id as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        });

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        // Dropping the engine hands every light back the way it was
        APP.with(|app| app.borrow_mut().take());
        let _ = CloseHandle(quit);
        log!("tray stopped");
    }
    Ok(())
}

fn with_engine<T>(work: impl FnOnce(&Engine) -> T) -> Option<T> {
    APP.with(|app| app.borrow().as_ref().map(|a| work(&a.engine)))
}

unsafe fn add_icon(window: HWND, action: NOTIFY_ICON_MESSAGE) {
    let icon = APP.with(|app| app.borrow().as_ref().map(|a| a.icon)).unwrap_or_default();
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    let status = with_engine(|e| e.status());
    let tip: Vec<u16> = format!("LightSync\n{}\n{}", title(status.as_ref()), subtitle(status.as_ref()))
        .encode_utf16()
        .take(data.szTip.len() - 1)
        .collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe {
        let _ = Shell_NotifyIconW(action, &data);
    }
}

unsafe fn show_menu(window: HWND) -> Result<()> {
    let status = with_engine(|e| e.status());
    let enabled = with_engine(|e| e.enabled()).unwrap_or(false);
    unsafe {
        let menu = CreatePopupMenu()?;
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(title(status.as_ref())))?;
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(subtitle(status.as_ref())))?;
        AppendMenuW(menu, MF_SEPARATOR, 0, None)?;
        AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open LightSync"))?;
        SetMenuDefaultItem(menu, MENU_OPEN as u32, 0)?;
        AppendMenuW(menu, if enabled { MF_STRING | MF_CHECKED } else { MF_STRING }, MENU_SYNC, w!("Sync with screen"))?;
        AppendMenuW(menu, MF_SEPARATOR, 0, None)?;
        AppendMenuW(menu, MF_STRING, MENU_ABOUT, w!("About LightSync"))?;
        AppendMenuW(menu, MF_STRING, MENU_SETUP, w!("Set Up…"))?;
        AppendMenuW(menu, MF_STRING, MENU_LOG, w!("Open Log"))?;
        AppendMenuW(menu, MF_SEPARATOR, 0, None)?;
        AppendMenuW(menu, MF_STRING, MENU_UNINSTALL, w!("Uninstall LightSync…"))?;
        AppendMenuW(menu, MF_SEPARATOR, 0, None)?;
        AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit LightSync"))?;
        let mut point = POINT::default();
        GetCursorPos(&mut point)?;
        // Without this the menu won't close when you click elsewhere
        let _ = SetForegroundWindow(window);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, point.x, point.y, None, window, None);
        let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
        DestroyMenu(menu)?;
    }
    Ok(())
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let taskbar_created = APP.with(|app| app.borrow().as_ref().map(|a| a.taskbar_created)).unwrap_or(0);
        match message {
            WM_TRAY => {
                match (lparam.0 & 0xFFFF) as u32 {
                    WM_LBUTTONUP => open_window(),
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        let _ = show_menu(window);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TIMER => {
                add_icon(window, NIM_MODIFY);
                LRESULT(0)
            }
            WM_COMMAND => {
                match wparam.0 & 0xFFFF {
                    MENU_OPEN => open_window(),
                    MENU_SYNC => {
                        with_engine(|e| e.set_enabled(!e.enabled()));
                        add_icon(window, NIM_MODIFY);
                    }
                    MENU_ABOUT => {
                        let text = format!("LightSync {}\n\n{CREDITS}", env!("CARGO_PKG_VERSION"));
                        MessageBoxW(
                            Some(window),
                            &HSTRING::from(text),
                            w!("About LightSync"),
                            MB_OK | MB_ICONINFORMATION,
                        );
                    }
                    MENU_SETUP => run_self("setup"),
                    MENU_LOG => {
                        let _ = Command::new("notepad.exe").arg(crate::log::path()).spawn();
                    }
                    MENU_UNINSTALL => run_self("uninstall"),
                    MENU_QUIT => {
                        let _ = DestroyWindow(window);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(window);
                LRESULT(0)
            }
            WM_DESTROY => {
                add_icon(window, NIM_DELETE);
                PostQuitMessage(0);
                LRESULT(0)
            }
            // Explorer restarted, so the icon has to be added again
            m if m == taskbar_created && m != 0 => {
                add_icon(window, NIM_ADD);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

/// The tray icon, drawn here so there's no icon file to ship.
fn make_icon() -> HICON {
    const SIZE: i32 = 32;
    let mut pixels = icon_rgba(SIZE as u32);
    // Windows wants BGRA
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIZE,
                biHeight: -SIZE,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        };
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
        let mask = CreateBitmap(SIZE, SIZE, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        })
        .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
