//! Install and uninstall, for the person signed in.
//!
//! Everything lives in the user's own profile, so installing needs no administrator: the app goes in
//! %LOCALAPPDATA%\Programs\LightSync, it starts at sign in from HKCU's Run key, it's listed in
//! Settings > Apps through HKCU's Uninstall key, and it gets a Start menu shortcut.
//!
//! The one thing that does need admin is a Windows Firewall rule: lights answer on UDP port 4002 from
//! addresses Windows hasn't seen us talk to, which the firewall drops otherwise. That step asks once.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile,
};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, EVENT_MODIFY_STATE, GetExitCodeProcess, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{IShellLinkW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows_core::{HSTRING, Interface, PCWSTR, w};

use crate::log::log;

pub const APP_NAME: &str = "LightSync";
pub const APP_EXE: &str = "LightSync.exe";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LightSync";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const FIREWALL_RULE: &str = "LightSync (Govee lights)";
pub const QUIT_EVENT: PCWSTR = w!("Local\\LightSyncQuit");
/// Passed by the sign in entry, so the app starts quietly in the tray instead of opening its window.
pub const BACKGROUND_FLAG: &str = "--background";

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("Programs").join(APP_NAME)
}

fn shortcut_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{APP_NAME}.lnk"))
}

pub fn installed_exe() -> PathBuf {
    install_dir().join(APP_EXE)
}

pub fn is_installed() -> bool {
    installed_exe().exists() && get_string(UNINSTALL_KEY, "DisplayVersion").is_some()
}

pub fn installed_version() -> Option<String> {
    get_string(UNINSTALL_KEY, "DisplayVersion")
}

/// True when this process is the installed copy.
pub fn running_installed_copy() -> bool {
    let here = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let there = installed_exe().canonicalize().ok();
    here.is_some() && here == there
}

/// Copies the app into the profile and registers it. No admin, no dialogs.
pub fn install() -> Result<()> {
    let dir = install_dir();
    log!("installing into {}", dir.display());
    ask_app_to_quit();
    std::fs::create_dir_all(&dir)?;
    let exe = installed_exe();
    if !running_installed_copy() {
        copy_retrying(&std::env::current_exe()?, &exe)?;
    }
    set_start_at_sign_in(true)?;
    let size_kb = std::fs::metadata(&exe).map_or(0, |m| (m.len() / 1024) as u32);
    set_string(UNINSTALL_KEY, "DisplayName", APP_NAME)?;
    set_string(UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    set_string(UNINSTALL_KEY, "Publisher", "lightsync (open source)")?;
    set_string(UNINSTALL_KEY, "InstallLocation", &dir.to_string_lossy())?;
    set_string(UNINSTALL_KEY, "DisplayIcon", &exe.to_string_lossy())?;
    set_string(UNINSTALL_KEY, "UninstallString", &format!("\"{}\" uninstall", exe.display()))?;
    set_string(UNINSTALL_KEY, "QuietUninstallString", &format!("\"{}\" uninstall --quiet", exe.display()))?;
    set_string(UNINSTALL_KEY, "URLInfoAbout", env!("CARGO_PKG_REPOSITORY"))?;
    set_dword(UNINSTALL_KEY, "EstimatedSize", size_kb)?;
    set_dword(UNINSTALL_KEY, "NoModify", 1)?;
    set_dword(UNINSTALL_KEY, "NoRepair", 1)?;
    if let Err(e) = create_shortcut(&exe, &shortcut_path()) {
        log!("no Start menu shortcut: {e:#}");
    }
    log!("installed");
    Ok(())
}

/// Starts the installed app, in the tray, or with its window open.
pub fn start_app(show_window: bool) -> Result<()> {
    let mut command = Command::new(installed_exe());
    if !show_window {
        command.arg(BACKGROUND_FLAG);
    }
    command.spawn().context("couldn't start LightSync")?;
    Ok(())
}

/// What uninstalling removes, for the uninstall window to show before anything happens.
pub fn removal_list() -> Vec<String> {
    let mut list = vec![
        format!("The app in {}", install_dir().display()),
        "Starting when you sign in, the Start menu shortcut and the Settings > Apps entry".into(),
        format!("The log in {}", crate::log::folder().display()),
    ];
    if firewall_allowed() {
        list.push("The Windows Firewall rule for the lights (asks for permission once)".into());
    }
    list
}

/// Removes everything. Your lights go back to how they were when the app quits.
pub fn uninstall(keep_settings: bool) -> Result<()> {
    log!("uninstalling");
    ask_app_to_quit();
    if firewall_allowed()
        && let Err(e) = run_elevated(&["firewall", "remove", "--elevated"])
    {
        log!("the firewall rule stays: {e:#}");
    }
    delete_tree(UNINSTALL_KEY)?;
    delete_value(RUN_KEY, APP_NAME)?;
    let _ = std::fs::remove_file(shortcut_path());
    let _ = std::fs::remove_dir_all(crate::log::folder());
    if !keep_settings && let Some(dir) = crate::log::settings_path().parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
    let dir = install_dir();
    if running_installed_copy() {
        // We can't delete the exe we're running from, so a detached shell does it once we've exited
        delete_after_exit(&dir)?;
    } else if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("couldn't remove {}", dir.display()))?;
    }
    Ok(())
}

pub fn starts_at_sign_in() -> bool {
    get_string(RUN_KEY, APP_NAME).is_some()
}

pub fn set_start_at_sign_in(on: bool) -> Result<()> {
    if on {
        set_string(RUN_KEY, APP_NAME, &format!("\"{}\" {BACKGROUND_FLAG}", installed_exe().display()))
    } else {
        delete_value(RUN_KEY, APP_NAME)
    }
}

// Firewall

pub fn firewall_allowed() -> bool {
    hidden("netsh")
        .args(["advfirewall", "firewall", "show", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Asks Windows for admin and adds the rule.
pub fn request_firewall() -> Result<()> {
    run_elevated(&["firewall", "add", "--elevated"])
}

/// The admin half: let the installed app (and this copy) receive the lights' replies on UDP 4002.
pub fn firewall(add: bool) -> Result<()> {
    let _ = hidden("netsh")
        .args(["advfirewall", "firewall", "delete", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .output();
    if !add {
        return Ok(());
    }
    let mut programs = vec![installed_exe()];
    if let Ok(here) = std::env::current_exe()
        && !programs.contains(&here)
    {
        programs.push(here);
    }
    for program in programs {
        let status = hidden("netsh")
            .args(["advfirewall", "firewall", "add", "rule"])
            .arg(format!("name={FIREWALL_RULE}"))
            .args(["dir=in", "action=allow", "protocol=UDP", "localport=4002", "profile=any"])
            .arg(format!("program={}", program.display()))
            .status()?;
        if !status.success() {
            bail!("Windows Firewall wouldn't add the rule");
        }
    }
    Ok(())
}

/// Relaunches this exe through UAC and waits for it, failing if it did.
fn run_elevated(args: &[&str]) -> Result<()> {
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = HSTRING::from(args.join(" "));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).context("Windows didn't allow the administrator step")?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.hProcess, &mut code)?;
        let _ = CloseHandle(info.hProcess);
        if code != 0 {
            bail!("the administrator step failed, see {}", crate::log::path().display());
        }
    }
    Ok(())
}

/// Removes a folder from a detached shell once every process running from it has let go,
/// retrying each second for up to a minute.
fn delete_after_exit(dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    // /s makes cmd strip only the outer quotes so the quoted path inside survives
    let dir = dir.display();
    let script = format!(
        "\"for /l %i in (1,1,60) do @(if exist \"{dir}\" (ping -n 2 127.0.0.1 >nul & rmdir /s /q \"{dir}\" 2>nul))\""
    );
    hidden("cmd").args(["/d", "/s", "/c"]).raw_arg(script).spawn()?;
    Ok(())
}

/// Asks a running copy to quit (handing its lights back) and gives it a moment.
pub fn ask_app_to_quit() {
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, QUIT_EVENT) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
            std::thread::sleep(Duration::from_millis(2500));
        }
    }
}

fn hidden(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW.0);
    command
}

fn copy_retrying(from: &Path, to: &Path) -> Result<()> {
    for attempt in 0..10 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(_) if attempt < 9 => std::thread::sleep(Duration::from_millis(300)),
            Err(e) => return Err(e).with_context(|| format!("couldn't copy to {}", to.display())),
        }
    }
    unreachable!()
}

fn create_shortcut(target: &Path, link: &Path) -> Result<()> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> Result<()> {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(&HSTRING::from(target.as_os_str()))?;
            shell_link.SetDescription(w!("Sync your Govee lights with your screen"))?;
            shell_link.cast::<IPersistFile>()?.Save(&HSTRING::from(link.as_os_str()), true)?;
            Ok(())
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

// Registry helpers, all against HKEY_CURRENT_USER

fn open_key(path: &str) -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut key,
            None,
        )
        .ok()
        .with_context(|| format!(r"couldn't open HKCU\{path}"))?;
    }
    Ok(key)
}

fn get_string(path: &str, name: &str) -> Option<String> {
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if result.is_err() {
        return None;
    }
    let length = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn set_string(path: &str, name: &str, value: &str) -> Result<()> {
    let key = open_key(path)?;
    let wide: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    let result = unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result.ok().with_context(|| format!(r"couldn't write HKCU\{path}"))
}

fn set_dword(path: &str, name: &str, value: u32) -> Result<()> {
    let key = open_key(path)?;
    let result = unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_DWORD, Some(&value.to_le_bytes())) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result.ok().with_context(|| format!(r"couldn't write HKCU\{path}"))
}

fn delete_tree(path: &str) -> Result<()> {
    let result = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(path)) };
    if result.is_err() && result != ERROR_FILE_NOT_FOUND {
        return result.ok().with_context(|| format!(r"couldn't delete HKCU\{path}"));
    }
    Ok(())
}

fn delete_value(path: &str, name: &str) -> Result<()> {
    let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(path), &HSTRING::from(name)) };
    if result.is_err() && result != ERROR_FILE_NOT_FOUND {
        return result.ok().with_context(|| format!(r"couldn't delete HKCU\{path}\{name}"));
    }
    Ok(())
}
