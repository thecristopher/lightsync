//! Everything a person can set, saved as JSON. Missing fields fall back to defaults, so older
//! settings files keep loading as the app grows.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::color::ColorSettings;
use crate::govee::BleColorCommand;
use crate::zones::{Layout, Side};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Sync on or paused
    pub enabled: bool,
    pub capture: CaptureSettings,
    /// Colour tuning every light uses unless it has its own
    pub color: ColorSettings,
    /// When to switch the lights off along with the screen
    pub lights_off: LightsOff,
    pub lights: Vec<Light>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            capture: CaptureSettings::default(),
            color: ColorSettings::default(),
            lights_off: LightsOff::default(),
            lights: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSettings {
    /// Index of the display, in the order Windows lists them
    pub monitor: u32,
    /// Most analyses per second; a still screen costs nothing whatever this is
    pub max_fps: f32,
    /// Ignore black bars in films
    pub letterbox: bool,
    /// Rough width in pixels the screen is shrunk to before measuring zones
    pub detail: u32,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self { monitor: 0, max_fps: 30.0, letterbox: true, detail: 160 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Light {
    /// Stable key: the LAN device id or the Bluetooth address
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub connection: Connection,
    pub layout: Layout,
    /// The strip runs the other way
    pub reverse: bool,
    /// Rotate where the first segment is
    pub offset: i32,
    /// Own colour tuning, or `None` to follow the defaults
    pub color: Option<ColorSettings>,
    pub max_fps: f32,
    pub on_exit: OnExit,
}

impl Default for Light {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: "Light".into(),
            enabled: true,
            connection: Connection::default(),
            layout: Layout::default(),
            reverse: false,
            offset: 0,
            color: None,
            max_fps: 30.0,
            on_exit: OnExit::Restore,
        }
    }
}

impl Light {
    /// A new LAN light as found by a scan, laid out around the screen's edges.
    pub fn lan(device_id: &str, sku: &str, ip: &str, aspect: f32) -> Self {
        Self {
            id: device_id.to_string(),
            name: format!("{sku} {}", short_id(device_id)),
            connection: Connection::Lan {
                device_id: device_id.into(),
                ip: ip.into(),
                sku: sku.into(),
                mode: LanMode::Razer,
                gradient: false,
            },
            layout: Layout::edges(&[Side::Left, Side::Top, Side::Right], 15, aspect),
            max_fps: 30.0,
            ..Default::default()
        }
    }

    /// A new Bluetooth light, one colour for the whole screen: BLE is too slow for much more.
    pub fn ble(address: &str, sku: &str) -> Self {
        Self {
            id: address.to_string(),
            name: format!("{sku} {}", short_id(address)),
            connection: Connection::Ble {
                address: address.into(),
                sku: sku.into(),
                mode: BleMode::Single,
                command: BleColorCommand::Modern,
            },
            layout: Layout::default(),
            max_fps: 10.0,
            ..Default::default()
        }
    }

    pub fn sku(&self) -> &str {
        match &self.connection {
            Connection::Lan { sku, .. } | Connection::Ble { sku, .. } => sku,
        }
    }

    pub fn color<'a>(&'a self, defaults: &'a ColorSettings) -> &'a ColorSettings {
        self.color.as_ref().unwrap_or(defaults)
    }
}

/// The last four hex digits of an id, which is what Govee shows in device names too.
pub fn short_id(id: &str) -> String {
    let hex: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    hex[hex.len().saturating_sub(4)..].to_uppercase()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Connection {
    Lan { device_id: String, ip: String, sku: String, mode: LanMode, gradient: bool },
    Ble { address: String, sku: String, mode: BleMode, command: BleColorCommand },
}

impl Default for Connection {
    fn default() -> Self {
        Connection::Lan {
            device_id: String::new(),
            ip: String::new(),
            sku: String::new(),
            mode: LanMode::Razer,
            gradient: false,
        }
    }
}

/// Switching the lights off when nobody is looking at the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightsOff {
    /// Keep them on, showing the last colour
    Never,
    /// When Windows turns the display off (or the PC goes to sleep)
    #[default]
    ScreenOff,
    /// Also as soon as the PC is locked
    ScreenOffOrLocked,
}

impl LightsOff {
    /// Whether the lights should be off, given what Windows last said about the screen.
    pub fn applies(self, display_off: bool, locked: bool) -> bool {
        match self {
            LightsOff::Never => false,
            LightsOff::ScreenOff => display_off,
            LightsOff::ScreenOffOrLocked => display_off || locked,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanMode {
    /// Per-segment streaming, the protocol DreamView uses
    #[default]
    Razer,
    /// One colour for the whole light; works on every LAN model
    Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BleMode {
    #[default]
    Single,
    Segments,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnExit {
    /// Put back the colour and power the light had before
    #[default]
    Restore,
    Off,
    /// Leave the last colour showing
    Keep,
}

impl Settings {
    /// Reads the settings, or the defaults when there's no file yet.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes next to the file and renames over it, so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    }

    pub fn light_mut(&mut self, id: &str) -> Option<&mut Light> {
        self.lights.iter_mut().find(|l| l.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip() {
        let mut settings = Settings::default();
        settings.lights.push(Light::lan("A2:98:D0:36:39:34:47:1C", "H6046", "192.168.68.127", 16.0 / 9.0));
        settings.lights.push(Light::ble("D0:36:39:34:47:1C", "H6008"));
        settings.lights[1].color = Some(ColorSettings { saturation: 1.5, ..Default::default() });
        let path = std::env::temp_dir().join(format!("lightsync-settings-{}.json", std::process::id()));
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn old_or_partial_files_still_load() {
        let settings: Settings =
            serde_json::from_str(r#"{"capture":{"max_fps":20},"lights":[{"id":"x","name":"Desk"}]}"#).unwrap();
        assert_eq!(settings.capture.max_fps, 20.0);
        assert!(settings.capture.letterbox);
        assert_eq!(settings.lights[0].name, "Desk");
        assert!(settings.lights[0].enabled);
    }

    #[test]
    fn lights_go_off_only_when_asked() {
        assert!(!LightsOff::Never.applies(true, true));
        assert!(LightsOff::ScreenOff.applies(true, false));
        assert!(!LightsOff::ScreenOff.applies(false, true), "locking alone keeps them on");
        assert!(LightsOff::ScreenOffOrLocked.applies(false, true));
        assert_eq!(Settings::default().lights_off, LightsOff::ScreenOff);
    }

    #[test]
    fn new_lights_get_sensible_names() {
        assert_eq!(Light::lan("A2:98:D0:36:39:34:47:1C", "H6046", "1.2.3.4", 1.7).name, "H6046 471C");
        assert_eq!(Light::ble("98:17:3c:44:47:39", "H6008").name, "H6008 4739");
    }
}
