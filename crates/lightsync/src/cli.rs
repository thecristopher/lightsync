//! The terminal commands.

use anyhow::{Context, Result, bail};
use lightsync_core::settings::Settings;

use crate::console::tell;
use crate::control::{ControlRequest, Found, LinkState, send};
use crate::panel::{subtitle, title};
use crate::setup;

pub fn install(quiet: bool) -> Result<()> {
    setup::install()?;
    setup::start_app(false)?;
    let mut message = String::from("LightSync is installed and running in the tray.");
    if !setup::firewall_allowed() {
        message.push_str(" Run `LightSync setup` to let the lights reach it through Windows Firewall.");
    }
    tell(&message, quiet);
    Ok(())
}

pub fn uninstall(quiet: bool, keep_settings: bool) -> Result<()> {
    setup::uninstall(keep_settings)?;
    tell("Uninstalled. Your lights are back to how they were.", quiet);
    Ok(())
}

pub fn status() -> Result<()> {
    let status = send(&ControlRequest::new("status"))?.status.context("no status from LightSync")?;
    let mut text = format!("{}: {}\n", title(Some(&status)), subtitle(Some(&status)));
    if !status.monitor.is_empty() {
        text.push_str(&format!("Screen: {}\n", status.monitor));
    }
    for light in &status.lights {
        let state = match light.state {
            LinkState::Live => format!("live, {:.0} updates/s", light.sends_per_second),
            LinkState::Connecting => "connecting".into(),
            LinkState::Trouble => format!("trouble: {}", light.detail),
        };
        text.push_str(&format!("  {}: {state}\n", light.name));
    }
    tell(text.trim_end(), false);
    Ok(())
}

pub fn simple(command: &str) -> Result<()> {
    let reply = send(&ControlRequest::new(command))?;
    if !reply.ok {
        bail!(reply.message.unwrap_or_default());
    }
    Ok(())
}

pub fn identify(light: Option<&str>) -> Result<()> {
    let Some(wanted) = light else { bail!("identify takes a light's name, like: identify \"H6046 471C\"") };
    let settings = Settings::load(&crate::log::settings_path()).map_err(anyhow::Error::msg)?;
    let found = settings
        .lights
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case(wanted) || l.id.eq_ignore_ascii_case(wanted))
        .with_context(|| {
            let names: Vec<&str> = settings.lights.iter().map(|l| l.name.as_str()).collect();
            format!("no light called \"{wanted}\". Lights: {}", names.join(", "))
        })?;
    let reply = send(&ControlRequest::with_id("identify", &found.id))?;
    if !reply.ok {
        bail!(reply.message.unwrap_or_default());
    }
    tell(&format!("{}: red is the first segment, blue the last, white walks from first to last.", found.name), false);
    Ok(())
}

/// Asks the running app (it owns the network port), or scans right here if it isn't running.
pub fn discover() -> Result<()> {
    let found = match send(&ControlRequest::new("discover")) {
        Ok(reply) if reply.ok => reply.found.unwrap_or_default(),
        Ok(reply) => bail!(reply.message.unwrap_or_default()),
        Err(_) => crate::engine::discover(&*crate::lights::lan::LanBus::open()?)?,
    };
    tell(&describe(&found), false);
    Ok(())
}

fn describe(found: &[Found]) -> String {
    if found.is_empty() {
        return "No Govee lights found. For Wi-Fi lights, turn on LAN Control in the Govee app.".into();
    }
    found
        .iter()
        .map(|f| match f.rssi {
            Some(rssi) => format!("{:<10} {:<9} {}  signal {rssi} dBm", f.link, f.sku, f.address),
            None => format!("{:<10} {:<9} {}  id {}", f.link, f.sku, f.address, f.id),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
