//! Govee Bluetooth lights, through Windows' own Bluetooth LE stack (WinRT).
//!
//! Bluetooth manages ~10-20 writes a second, so this is frugal: in segment mode it only writes the
//! segments that changed, and merges segments sharing a colour into one masked packet.
//!
//! Newer models want their frames encrypted. A light that doesn't answer a plain keepalive gets the
//! handshake, and whichever way works is remembered for the next connect.

use std::hash::{BuildHasher, RandomState};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use lightsync_core::govee::{self as gp, BleColorCommand, Rgb};
use lightsync_core::settings::{BleMode, OnExit};
use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher, BluetoothLEScanningMode,
};
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristic, GattClientCharacteristicConfigurationDescriptorValue, GattCommunicationStatus,
    GattDeviceService, GattValueChangedEventArgs, GattWriteOption,
};
use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothLEDevice};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, DataWriter};
use windows_core::GUID;

use super::{Light, average};

/// Segments quantized to steps of this size share packets more often.
const QUANT: u8 = 8;
/// Replies come back within ~50 ms; lights that never reply are given this long.
const REPLY_WAIT: Duration = Duration::from_millis(1500);

pub fn parse_address(address: &str) -> Option<u64> {
    let hex: String = address.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (hex.len() == 12).then(|| u64::from_str_radix(&hex, 16).ok()).flatten()
}

pub fn format_address(address: u64) -> String {
    let bytes = address.to_be_bytes();
    bytes[2..].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BleFound {
    pub address: String,
    pub name: String,
    pub sku: String,
    pub rssi: i16,
}

/// Listens for Govee advertisements for `duration`.
pub fn scan(duration: Duration) -> Result<Vec<BleFound>> {
    let found: Arc<Mutex<Vec<BleFound>>> = Arc::default();
    let watcher = BluetoothLEAdvertisementWatcher::new()?;
    watcher.SetScanningMode(BluetoothLEScanningMode::Active)?;
    let sink = found.clone();
    watcher.Received(
        &TypedEventHandler::<BluetoothLEAdvertisementWatcher, BluetoothLEAdvertisementReceivedEventArgs>::new(
            move |_, args| {
                let Some(args) = args.as_ref() else { return Ok(()) };
                let name = args.Advertisement()?.LocalName()?.to_string();
                if !gp::BLE_NAME_PREFIXES.iter().any(|p| name.starts_with(p)) {
                    return Ok(());
                }
                let address = format_address(args.BluetoothAddress()?);
                let parts: Vec<&str> = name.split('_').collect();
                let sku = if parts.len() > 2 { parts[1].to_string() } else { name.clone() };
                let rssi = args.RawSignalStrengthInDBm()?;
                let mut found = sink.lock().unwrap();
                match found.iter_mut().find(|f| f.address == address) {
                    Some(existing) => existing.rssi = existing.rssi.max(rssi),
                    None => found.push(BleFound { address, name, sku, rssi }),
                }
                Ok(())
            },
        ),
    )?;
    watcher.Start().context("Bluetooth is off or missing")?;
    std::thread::sleep(duration);
    let _ = watcher.Stop();
    let mut found = found.lock().unwrap().clone();
    found.sort_by_key(|f| -f.rssi);
    Ok(found)
}

pub struct BleLight {
    address: u64,
    mode: BleMode,
    command: BleColorCommand,
    on_exit: OnExit,
    min_delta: u8,
    device: Option<BluetoothLEDevice>,
    service: Option<GattDeviceService>,
    characteristic: Option<GattCharacteristic>,
    replies: Option<Replies>,
    /// Learned on the first connect: `Some(true)` needs the handshake, `Some(false)` takes plaintext.
    encrypted: Option<bool>,
    key: Option<[u8; 16]>,
    last: Option<Vec<Rgb>>,
}

/// Notifications from the reply characteristic.
struct Replies {
    characteristic: GattCharacteristic,
    token: i64,
    frames: Receiver<Vec<u8>>,
}

impl Replies {
    fn subscribe(characteristic: GattCharacteristic) -> Result<Self> {
        let (sender, frames) = sync_channel(16);
        let token = characteristic.ValueChanged(
            &TypedEventHandler::<GattCharacteristic, GattValueChangedEventArgs>::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    let buffer = args.CharacteristicValue()?;
                    let mut frame = vec![0; buffer.Length()? as usize];
                    DataReader::FromBuffer(&buffer)?.ReadBytes(&mut frame)?;
                    // Replies nobody waits for are dropped once the queue is full
                    let _ = sender.try_send(frame);
                }
                Ok(())
            }),
        )?;
        let status = characteristic
            .WriteClientCharacteristicConfigurationDescriptorAsync(
                GattClientCharacteristicConfigurationDescriptorValue::Notify,
            )?
            .join()?;
        if status != GattCommunicationStatus::Success {
            let _ = characteristic.RemoveValueChanged(token);
            bail!("couldn't listen for its replies");
        }
        Ok(Self { characteristic, token, frames })
    }

    fn clear(&self) {
        while self.frames.try_recv().is_ok() {}
    }

    /// The first reply `accept` makes something of, within `wait`.
    fn wait_for<T>(&self, wait: Duration, mut accept: impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
        let until = Instant::now() + wait;
        while let Ok(frame) = self.frames.recv_timeout(until.saturating_duration_since(Instant::now())) {
            if let Some(found) = accept(&frame) {
                return Some(found);
            }
        }
        None
    }
}

impl Drop for Replies {
    fn drop(&mut self) {
        let _ = self.characteristic.RemoveValueChanged(self.token);
    }
}

fn nonce() -> [u8; 17] {
    let state = RandomState::new();
    let mut nonce = [0; 17];
    for (i, chunk) in nonce.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(&state.hash_one(i).to_le_bytes()[..chunk.len()]);
    }
    nonce
}

impl BleLight {
    pub fn new(address: &str, mode: BleMode, command: BleColorCommand, on_exit: OnExit, min_delta: u8) -> Result<Self> {
        let address = parse_address(address).with_context(|| format!("\"{address}\" isn't a Bluetooth address"))?;
        Ok(Self {
            address,
            mode,
            command,
            on_exit,
            min_delta,
            device: None,
            service: None,
            characteristic: None,
            replies: None,
            encrypted: None,
            key: None,
            last: None,
        })
    }

    fn write(&self, packet: &[u8]) -> Result<()> {
        match &self.key {
            Some(key) => self.write_raw(&gp::ble_cipher(packet, key, true)),
            None => self.write_raw(packet),
        }
    }

    fn write_raw(&self, packet: &[u8]) -> Result<()> {
        let characteristic = self.characteristic.as_ref().context("not connected")?;
        let writer = DataWriter::new()?;
        writer.WriteBytes(packet)?;
        let buffer = writer.DetachBuffer()?;
        let result = characteristic
            .WriteValueWithResultAndOptionAsync(&buffer, GattWriteOption::WriteWithoutResponse)?
            .join()?;
        if result.Status()? != GattCommunicationStatus::Success {
            bail!("the light stopped answering over Bluetooth");
        }
        Ok(())
    }

    /// Works out whether the light wants encrypted frames, and gets the session key if it does.
    fn negotiate(&mut self) -> Result<()> {
        self.key = None;
        let Some(replies) = &self.replies else {
            // Nothing to hear an answer on, so all that's left is plaintext
            return Ok(());
        };
        if self.encrypted.is_none() {
            replies.clear();
            self.write_raw(&gp::ble_keepalive())?;
            if replies.wait_for(REPLY_WAIT, |f| (f.first() == Some(&0xAA)).then_some(())).is_some() {
                self.encrypted = Some(false);
            }
        }
        if self.encrypted == Some(false) {
            return Ok(());
        }
        replies.clear();
        self.write_raw(&gp::ble_handshake(1, nonce()))?;
        let Some(key) = replies.wait_for(REPLY_WAIT, |f| gp::ble_handshake_reply(f, 1)) else {
            if self.encrypted.is_none() {
                // Answers neither way; plaintext is how it worked before encryption came along
                self.encrypted = Some(false);
                return Ok(());
            }
            bail!("it didn't answer the encryption handshake");
        };
        self.write_raw(&gp::ble_handshake(2, nonce()))?;
        if replies.wait_for(REPLY_WAIT, |f| gp::ble_handshake_reply(f, 2)).is_none() {
            bail!("it didn't confirm the encryption handshake");
        }
        self.encrypted = Some(true);
        self.key = Some(key);
        Ok(())
    }
}

impl Light for BleLight {
    fn connect(&mut self) -> Result<()> {
        let device = BluetoothLEDevice::FromBluetoothAddressAsync(self.address)?
            .join()
            .ok()
            .context("not in range, or Bluetooth is off")?;
        let started = Instant::now();
        let services = device
            .GetGattServicesForUuidWithCacheModeAsync(GUID::from_u128(gp::BLE_SERVICE), BluetoothCacheMode::Uncached)?
            .join()?;
        if services.Status()? != GattCommunicationStatus::Success || services.Services()?.Size()? == 0 {
            bail!("couldn't reach it over Bluetooth (is the Govee app connected to it on a phone?)");
        }
        let service = services.Services()?.GetAt(0)?;
        // Kept so close() can close it: Windows holds the link open while a service object lives
        self.service = Some(service.clone());
        self.device = Some(device);
        let characteristic = |uuid: u128| -> Result<Option<GattCharacteristic>> {
            let found = service.GetCharacteristicsForUuidAsync(GUID::from_u128(uuid))?.join()?;
            if found.Status()? != GattCommunicationStatus::Success || found.Characteristics()?.Size()? == 0 {
                return Ok(None);
            }
            Ok(Some(found.Characteristics()?.GetAt(0)?))
        };
        self.characteristic = Some(characteristic(gp::BLE_CONTROL_CHAR)?.context("it doesn't take Govee commands")?);
        self.replies = characteristic(gp::BLE_REPLY_CHAR)?.and_then(|c| Replies::subscribe(c).ok());
        self.last = None;
        self.negotiate()?;
        crate::log::log!(
            "bluetooth {} connected in {:?}{}",
            format_address(self.address),
            started.elapsed(),
            if self.key.is_some() { ", encrypted" } else { "" }
        );
        self.write(&gp::ble_power(true))
    }

    fn send(&mut self, colors: &[Rgb]) -> Result<()> {
        match self.mode {
            BleMode::Single => self.write(&gp::ble_color(average(colors), self.command))?,
            BleMode::Segments => {
                let changed: Vec<usize> = match &self.last {
                    Some(last) if last.len() == colors.len() => (0..colors.len())
                        .filter(|&i| (0..3).any(|c| last[i][c].abs_diff(colors[i][c]) >= self.min_delta.max(1)))
                        .collect(),
                    _ => (0..colors.len()).collect(),
                };
                let mask_bytes = gp::mask_bytes(colors.len());
                for (rgb, mask) in gp::group_segments(colors, changed, QUANT) {
                    self.write(&gp::ble_segment_color(rgb, mask, mask_bytes))?;
                }
            }
        }
        self.last = Some(colors.to_vec());
        Ok(())
    }

    fn keepalive(&mut self) -> Result<()> {
        self.write(&gp::ble_keepalive())
    }

    fn sleep(&mut self) {
        let _ = self.write(&gp::ble_power(false));
        // Let go of the connection too; connect() opens it again
        self.close(false);
    }

    fn close(&mut self, restore: bool) {
        if restore && self.on_exit == OnExit::Off {
            let _ = self.write(&gp::ble_power(false));
        }
        self.replies = None;
        self.characteristic = None;
        self.key = None;
        if let Some(service) = self.service.take() {
            let _ = service.Close();
        }
        if let Some(device) = self.device.take() {
            let _ = device.Close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_round_trip() {
        let address = parse_address("d0:36:39:34:47:1c").unwrap();
        assert_eq!(address, 0xD0363934471C);
        assert_eq!(format_address(address), "D0:36:39:34:47:1C");
        assert_eq!(parse_address("nope"), None);
    }
}
