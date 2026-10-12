//! The parts of LightSync with no OS code: Govee wire formats, screen zones, colour processing
//! and settings. The app crate adds capture, networking, Bluetooth and the windows.

pub mod color;
pub mod govee;
pub mod icon;
pub mod settings;
pub mod zones;
