//! Listens for Windows turning the display off and on, the PC locking and unlocking, and sleep,
//! so the lights can go dark with the screen. A hidden window of its own receives the notifications,
//! which works the same whether the engine runs in the tray or in a terminal.

use std::cell::RefCell;

use anyhow::Result;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{POWERBROADCAST_SETTING, RegisterPowerSettingNotification};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::w;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenEvent {
    DisplayOff,
    DisplayOn,
    Locked,
    Unlocked,
    /// The PC is going to sleep or hibernate
    Suspending,
}

type Callback = Box<dyn Fn(ScreenEvent)>;

thread_local! {
    static CALLBACK: RefCell<Option<Callback>> = const { RefCell::new(None) };
}

/// Calls `on_event` (on the watcher's own thread) for as long as the process lives.
/// Windows reports the display's current state right away, so the first call says where things stand.
pub fn spawn(on_event: impl Fn(ScreenEvent) + Send + 'static) -> Result<()> {
    std::thread::Builder::new().name("screen watch".into()).spawn(move || {
        CALLBACK.with(|c| *c.borrow_mut() = Some(Box::new(on_event)));
        if let Err(e) = unsafe { listen() } {
            crate::log::log!("can't follow the screen turning off: {e:#}");
        }
    })?;
    Ok(())
}

unsafe fn listen() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("LightSyncScreenWatch"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // A plain hidden window rather than a message-only one: power notifications skip those
        let window = CreateWindowExW(
            Default::default(),
            w!("LightSyncScreenWatch"),
            w!("LightSync Screen Watch"),
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
        RegisterPowerSettingNotification(HANDLE(window.0), &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE)?;
        WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION)?;
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn emit(event: ScreenEvent) {
    CALLBACK.with(|c| {
        if let Some(callback) = c.borrow().as_ref() {
            callback(event);
        }
    });
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_POWERBROADCAST => {
            match wparam.0 as u32 {
                PBT_POWERSETTINGCHANGE => {
                    let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                    if setting.PowerSetting == GUID_CONSOLE_DISPLAY_STATE {
                        // 0 off, 1 on, 2 dimmed (still on)
                        emit(if setting.Data[0] == 0 { ScreenEvent::DisplayOff } else { ScreenEvent::DisplayOn });
                    }
                }
                PBT_APMSUSPEND => emit(ScreenEvent::Suspending),
                _ => {}
            }
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE => {
            match wparam.0 as u32 {
                WTS_SESSION_LOCK => emit(ScreenEvent::Locked),
                WTS_SESSION_UNLOCK => emit(ScreenEvent::Unlocked),
                _ => {}
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
