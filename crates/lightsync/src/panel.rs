//! What the app says about the engine, shared by the tray and the windows, and the app's icon.

use crate::control::{EngineStatus, SyncState};

pub fn title(status: Option<&EngineStatus>) -> String {
    match status.map(|s| s.state) {
        None => "LightSync isn't running".into(),
        Some(SyncState::Paused) => "Sync is paused".into(),
        Some(SyncState::NoLights) => "No lights yet".into(),
        Some(SyncState::Starting) => "Starting".into(),
        Some(SyncState::Running) => "Syncing".into(),
        Some(SyncState::Trouble) => "Can't see the screen".into(),
    }
}

pub fn subtitle(status: Option<&EngineStatus>) -> String {
    let Some(status) = status else { return "Start it from the Start menu".into() };
    match status.state {
        SyncState::Running if status.fps < 0.5 => format!("{} · screen is still", status.detail),
        SyncState::Running => format!("{} · {:.0} fps", status.detail, status.fps),
        SyncState::NoLights => "Open LightSync and add your lights".into(),
        _ => status.detail.clone(),
    }
}

pub const CREDITS: &str = "Open source and free. Made by Cristopher Cervantes.\n\nAn independent project, not affiliated \
with or endorsed by Govee. Govee and DreamView are trademarks of their owners, used here only to say which lights this \
works with. Protocol knowledge thanks to OpenRGB, Home Assistant and govee2mqtt.";

pub use lightsync_core::icon::icon_rgba;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_explains_each_state() {
        assert_eq!(title(None), "LightSync isn't running");
        let status = EngineStatus {
            state: SyncState::Running,
            detail: "2 of 3 lights live".into(),
            fps: 29.6,
            ..Default::default()
        };
        assert_eq!(subtitle(Some(&status)), "2 of 3 lights live · 30 fps");
        let still = EngineStatus { fps: 0.0, ..status };
        assert_eq!(subtitle(Some(&still)), "2 of 3 lights live · screen is still");
    }
}
