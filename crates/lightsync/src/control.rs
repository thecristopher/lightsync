//! How the CLI and the window reach the running engine: one JSON object per line over a named pipe.

use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use lightsync_core::govee::Rgb;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, GetLastError, HANDLE};
use windows::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_core::HSTRING;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl ControlRequest {
    pub fn new(command: &str) -> Self {
        Self { command: command.into(), id: None }
    }

    pub fn with_id(command: &str, id: &str) -> Self {
        Self { command: command.into(), id: Some(id.into()) }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<EngineStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<Vec<Found>>,
}

impl ControlResponse {
    pub fn success() -> Self {
        Self { ok: true, ..Default::default() }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self { ok: false, message: Some(message.into()), ..Default::default() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    #[default]
    Paused,
    NoLights,
    Starting,
    Running,
    Trouble,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    #[default]
    Connecting,
    Live,
    Trouble,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LightStatus {
    pub id: String,
    pub name: String,
    pub state: LinkState,
    pub detail: String,
    /// What the light is showing, one colour per zone
    pub colors: Vec<Rgb>,
    pub sends_per_second: f32,
}

/// A snapshot of the engine for the tray, the window and `LightSync status`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineStatus {
    pub state: SyncState,
    pub detail: String,
    pub monitor: String,
    /// The captured display's size, for drawing zones in proportion
    #[serde(default)]
    pub screen: (u32, u32),
    /// Screen analyses per second; 0 while the screen is still
    pub fps: f32,
    pub lights: Vec<LightStatus>,
}

/// A light a scan turned up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    /// "wifi" or "bluetooth"
    pub link: String,
    pub id: String,
    pub sku: String,
    /// IP address or Bluetooth address
    pub address: String,
    #[serde(default)]
    pub rssi: Option<i16>,
}

/// One pipe per Windows session, so two people signed in at once each talk to their own engine.
fn pipe_name() -> String {
    let mut session = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(std::process::id(), &mut session);
    }
    format!(r"\\.\pipe\LightSync-{session}")
}

pub type Handler = Arc<dyn Fn(ControlRequest) -> ControlResponse + Send + Sync>;

/// Serves the pipe on background threads for as long as the process lives.
pub fn serve(handler: Handler) -> Result<()> {
    serve_on(&pipe_name(), handler)
}

fn serve_on(name: &str, handler: Handler) -> Result<()> {
    let name = HSTRING::from(name);
    // Make the first instance now, so a failure shows up here instead of silently on a thread.
    // Handles aren't Send, so the first one crosses into the thread as a plain number.
    let first = create_instance(&name)?.0 as isize;
    std::thread::Builder::new().name("control pipe".into()).spawn(move || {
        let mut next = Some(HANDLE(first as _));
        loop {
            let instance = match next.take().map_or_else(|| create_instance(&name), Ok) {
                Ok(instance) => instance,
                Err(e) => {
                    crate::log::log!("control pipe stopped: {e:#}");
                    return;
                }
            };
            let connected = unsafe { ConnectNamedPipe(instance, None) };
            if connected.is_err() && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                drop(unsafe { std::fs::File::from_raw_handle(instance.0) });
                continue;
            }
            let handler = handler.clone();
            let raw = instance.0 as isize;
            // Each client on its own thread, so a slow scan never holds up status polling
            std::thread::spawn(move || serve_client(HANDLE(raw as _), &handler));
        }
    })?;
    Ok(())
}

fn create_instance(name: &HSTRING) -> Result<HANDLE> {
    let handle = unsafe {
        CreateNamedPipeW(
            name,
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        return Err(windows_core::Error::from_thread()).context("couldn't open the control pipe");
    }
    Ok(handle)
}

fn serve_client(pipe: HANDLE, handler: &Handler) {
    let file = unsafe { std::fs::File::from_raw_handle(pipe.0) };
    let mut reader = BufReader::new(&file);
    let mut line = String::new();
    let response = match reader.read_line(&mut line) {
        Ok(_) => match serde_json::from_str::<ControlRequest>(&line) {
            Ok(request) => handler(request),
            Err(_) => ControlResponse::failure("couldn't read that request"),
        },
        Err(_) => return,
    };
    if let Ok(mut reply) = serde_json::to_vec(&response) {
        reply.push(b'\n');
        let mut writer = &file;
        let _ = writer.write_all(&reply);
        unsafe {
            let _ = FlushFileBuffers(pipe);
            let _ = DisconnectNamedPipe(pipe);
        }
    }
}

/// Sends one request to the running engine and waits for its answer.
pub fn send(request: &ControlRequest) -> Result<ControlResponse> {
    send_to(&pipe_name(), request)
}

fn send_to(name: &str, request: &ControlRequest) -> Result<ControlResponse> {
    let pipe = std::fs::OpenOptions::new().read(true).write(true).open(name).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!("LightSync isn't running. Start it from the Start menu."),
        _ => anyhow!("couldn't reach LightSync: {e}"),
    })?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    (&pipe).write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(&pipe).read_line(&mut reply)?;
    serde_json::from_str(&reply).context("no sensible reply from LightSync")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_and_reply_cross_the_pipe() {
        let handler: Handler = Arc::new(|request| ControlResponse {
            message: Some(format!("got {} {}", request.command, request.id.unwrap_or_default())),
            ..ControlResponse::success()
        });
        // Its own pipe, so the test never talks to a LightSync that's actually running
        let name = format!(r"\\.\pipe\LightSync-test-{}", std::process::id());
        serve_on(&name, handler).unwrap();
        let reply = send_to(&name, &ControlRequest::with_id("identify", "abc")).unwrap();
        assert_eq!(reply.message.as_deref(), Some("got identify abc"));
    }

    #[test]
    fn requests_are_compact() {
        assert_eq!(serde_json::to_string(&ControlRequest::new("status")).unwrap(), r#"{"command":"status"}"#);
    }
}
