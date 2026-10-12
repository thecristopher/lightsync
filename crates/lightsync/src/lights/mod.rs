//! The lights the engine streams to. Each runs on its own thread, so a slow Bluetooth bulb never
//! holds up the screen capture or the other lights.

pub mod ble;
pub mod lan;

use anyhow::Result;
use lightsync_core::govee::Rgb;

pub trait Light: Send {
    /// Reaches the light and gets it ready to stream (power on, streaming mode, saved state).
    fn connect(&mut self) -> Result<()>;
    /// One colour per zone.
    fn send(&mut self, colors: &[Rgb]) -> Result<()>;
    /// Called when nothing was sent for a while, so the light stays in streaming mode.
    fn keepalive(&mut self) -> Result<()>;
    /// Switches the light off and leaves streaming mode, for while the screen is off.
    /// `connect` brings it back.
    fn sleep(&mut self);
    /// Hands the light back: the way it was, off, or as it is, depending on its settings.
    fn close(&mut self, restore: bool);
}

pub fn average(colors: &[Rgb]) -> Rgb {
    let n = colors.len().max(1) as u32;
    let mut sum = [0u32; 3];
    for c in colors {
        for i in 0..3 {
            sum[i] += c[i] as u32;
        }
    }
    sum.map(|s| ((s + n / 2) / n) as u8)
}
