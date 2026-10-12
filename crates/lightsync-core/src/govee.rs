//! Govee wire formats, as plain functions so they're easy to test.
//!
//! All of it comes from public reverse engineering of the Govee apps and firmware:
//! the LAN API (Govee's "LAN Control" docs, Home Assistant's `govee_light_local`), the `razer`
//! streaming protocol behind DreamView and Razer Chroma (OpenRGB's `GoveeController`), and the
//! BLE packets (`wez/govee2mqtt`, Home Assistant's `govee-ble`).

use std::net::Ipv4Addr;

use serde_json::{Value, json};

pub type Rgb = [u8; 3];

pub fn xor(data: &[u8]) -> u8 {
    data.iter().fold(0, |c, b| c ^ b)
}

// ---------------------------------------------------------------------------------------------- LAN

pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
/// Devices listen here for the multicast scan
pub const SCAN_PORT: u16 = 4001;
/// Devices send every reply here
pub const LISTEN_PORT: u16 = 4002;
/// Devices take commands here
pub const CONTROL_PORT: u16 = 4003;

fn message(cmd: &str, data: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "msg": { "cmd": cmd, "data": data } })).unwrap_or_default()
}

pub fn lan_scan() -> Vec<u8> {
    message("scan", json!({ "account_topic": "reserve" }))
}

pub fn lan_status() -> Vec<u8> {
    message("devStatus", json!({}))
}

pub fn lan_turn(on: bool) -> Vec<u8> {
    message("turn", json!({ "value": u8::from(on) }))
}

pub fn lan_brightness(percent: u8) -> Vec<u8> {
    message("brightness", json!({ "value": percent.clamp(1, 100) }))
}

pub fn lan_color(rgb: Rgb, kelvin: u32) -> Vec<u8> {
    let [r, g, b] = rgb;
    message("colorwc", json!({ "color": { "r": r, "g": g, "b": b }, "colorTemInKelvin": kelvin }))
}

pub fn razer_enable(on: bool) -> Vec<u8> {
    with_xor(vec![0xBB, 0x00, 0x01, 0xB1, u8::from(on)])
}

/// One colour per segment. Up to 255 segments; `gradient` lets the device blend between them.
pub fn razer_frame(colors: &[Rgb], gradient: bool) -> Vec<u8> {
    let n = colors.len().clamp(1, 255);
    let length = (2 + 3 * n) as u16;
    let mut packet = vec![0xBB, (length >> 8) as u8, length as u8, 0xB0, u8::from(gradient), n as u8];
    for rgb in colors.iter().take(n) {
        packet.extend_from_slice(rgb);
    }
    with_xor(packet)
}

pub fn lan_razer(payload: &[u8]) -> Vec<u8> {
    message("razer", json!({ "pt": base64(payload) }))
}

/// BLE packets tunnelled over Wi-Fi.
pub fn lan_ptreal(packets: &[Vec<u8>]) -> Vec<u8> {
    message("ptReal", json!({ "command": packets.iter().map(|p| base64(p)).collect::<Vec<_>>() }))
}

fn with_xor(mut packet: Vec<u8>) -> Vec<u8> {
    packet.push(xor(&packet));
    packet
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanDevice {
    pub ip: Ipv4Addr,
    pub device_id: String,
    pub sku: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceState {
    pub on: bool,
    pub brightness: u8,
    pub color: Rgb,
    pub kelvin: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanReply {
    Scan(LanDevice),
    Status(DeviceState),
}

pub fn parse_reply(data: &[u8]) -> Option<LanReply> {
    let value: Value = serde_json::from_slice(data).ok()?;
    let msg = value.get("msg")?;
    let data = msg.get("data")?;
    let int = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    match msg.get("cmd")?.as_str()? {
        "scan" => Some(LanReply::Scan(LanDevice {
            ip: data.get("ip")?.as_str()?.parse().ok()?,
            device_id: data.get("device")?.as_str()?.to_string(),
            sku: data.get("sku").and_then(Value::as_str).unwrap_or("Govee").to_string(),
        })),
        "devStatus" => {
            let color = data.get("color").cloned().unwrap_or(Value::Null);
            Some(LanReply::Status(DeviceState {
                on: int(data, "onOff") == 1,
                brightness: int(data, "brightness").min(100) as u8,
                color: [int(&color, "r") as u8, int(&color, "g") as u8, int(&color, "b") as u8],
                kelvin: int(data, "colorTemInKelvin") as u32,
            }))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------- BLE

pub const BLE_SERVICE: u128 = 0x00010203_0405_0607_0809_0a0b0c0d1910;
pub const BLE_CONTROL_CHAR: u128 = 0x00010203_0405_0607_0809_0a0b0c0d2b11;
/// Replies arrive here as notifications
pub const BLE_REPLY_CHAR: u128 = 0x00010203_0405_0607_0809_0a0b0c0d2b10;
/// "GVH…" names are thermometers and other sensors, so they're left out.
pub const BLE_NAME_PREFIXES: [&str; 4] = ["ihoment_", "Govee_", "GBK_", "Minger_"];

/// Govee BLE frames are always 20 bytes: the payload padded to 19, then an XOR checksum.
pub fn ble_packet(payload: &[u8]) -> Vec<u8> {
    let mut packet = payload[..payload.len().min(19)].to_vec();
    packet.resize(19, 0);
    with_xor(packet)
}

pub fn ble_keepalive() -> Vec<u8> {
    ble_packet(&[0xAA, 0x01])
}

pub fn ble_power(on: bool) -> Vec<u8> {
    ble_packet(&[0x33, 0x01, u8::from(on)])
}

pub fn ble_brightness(level: u8) -> Vec<u8> {
    ble_packet(&[0x33, 0x04, level])
}

/// Which "manual colour" sub-command a model family understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BleColorCommand {
    /// 0x0D, most current bulbs and strips
    #[default]
    Modern,
    /// 0x02, older strips and bulbs
    Legacy,
    /// 0x15 0x01 with every segment selected, for segmented models
    Segment,
}

pub fn ble_color(rgb: Rgb, command: BleColorCommand) -> Vec<u8> {
    let [r, g, b] = rgb;
    match command {
        BleColorCommand::Modern => ble_packet(&[0x33, 0x05, 0x0D, r, g, b]),
        BleColorCommand::Legacy => ble_packet(&[0x33, 0x05, 0x02, r, g, b]),
        BleColorCommand::Segment => ble_segment_color(rgb, 0xFFFF, 2),
    }
}

/// Colours the segments whose bit is set in `mask` (bit i is segment i, little endian).
pub fn ble_segment_color(rgb: Rgb, mask: u64, mask_bytes: usize) -> Vec<u8> {
    let [r, g, b] = rgb;
    let mut payload = vec![0x33, 0x05, 0x15, 0x01, r, g, b, 0, 0, 0, 0, 0];
    payload.extend_from_slice(&mask.to_le_bytes()[..mask_bytes.clamp(1, 7)]);
    ble_packet(&payload)
}

/// Newer models (H6097 and friends) only obey frames encrypted with a session key they hand out in a
/// two step handshake, the Govee app's "v1" scheme (`Controller4Aes`, as documented by
/// `teh-hippo/ha-govee-led-ble`). Plaintext is acknowledged, ignored, and dropped after ~10 s.
const BLE_HANDSHAKE_KEY: [u8; 16] = *b"MakingLifeSmarte";

/// AES-128-ECB over the whole 16 byte blocks, RC4 over the rest. Both directions, both phases.
pub fn ble_cipher(data: &[u8], key: &[u8; 16], encrypt: bool) -> Vec<u8> {
    use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
    let aes = aes::Aes128::new(key.into());
    let mut out = data.to_vec();
    let boundary = data.len() / 16 * 16;
    for block in out[..boundary].chunks_mut(16) {
        if encrypt {
            aes.encrypt_block(block.into());
        } else {
            aes.decrypt_block(block.into());
        }
    }
    rc4(key, &mut out[boundary..]);
    out
}

fn rc4(key: &[u8], data: &mut [u8]) {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    for byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        *byte ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
    }
}

/// Handshake `step` 1 asks for the session key, step 2 confirms it. `nonce` is any random bytes.
pub fn ble_handshake(step: u8, nonce: [u8; 17]) -> Vec<u8> {
    let mut frame = vec![0xE7, step];
    frame.extend_from_slice(&nonce);
    ble_cipher(&with_xor(frame), &BLE_HANDSHAKE_KEY, true)
}

/// The 16 byte body of the light's answer to handshake `step` (the session key, for step 1).
pub fn ble_handshake_reply(frame: &[u8], step: u8) -> Option<[u8; 16]> {
    if frame.len() != 20 {
        return None;
    }
    let plain = ble_cipher(frame, &BLE_HANDSHAKE_KEY, false);
    (plain[0] == 0xE7 && plain[1] == step && xor(&plain) == 0).then(|| plain[2..18].try_into().unwrap())
}

/// Bytes a segment mask needs for `segments` segments (at least the 2 every model takes).
pub fn mask_bytes(segments: usize) -> usize {
    segments.div_ceil(8).clamp(2, 7)
}

/// Merges segments that share a (quantized) colour into `(colour, mask)` groups, so slow links send
/// one packet per distinct colour instead of one per segment.
pub fn group_segments(colors: &[Rgb], indices: impl IntoIterator<Item = usize>, quant: u8) -> Vec<(Rgb, u64)> {
    let q = quant.max(1) as u16;
    let mut groups: Vec<(Rgb, u64)> = Vec::new();
    for i in indices.into_iter().filter(|&i| i < colors.len() && i < 64) {
        let key = colors[i].map(|v| ((v as u16 + q / 2) / q * q).min(255) as u8);
        match groups.iter_mut().find(|(c, _)| *c == key) {
            Some((_, mask)) => *mask |= 1 << i,
            None => groups.push((key, 1 << i)),
        }
    }
    groups
}

/// Standard base64, small enough that it's not worth a dependency.
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn razer_enable_matches_openrgb() {
        assert_eq!(razer_enable(true), [0xBB, 0x00, 0x01, 0xB1, 0x01, 0x0A]);
        assert_eq!(razer_enable(false), [0xBB, 0x00, 0x01, 0xB1, 0x00, 0x0B]);
    }

    #[test]
    fn razer_frames_carry_every_segment_and_a_checksum() {
        let packet = razer_frame(&[[255, 0, 0], [0, 0, 255]], false);
        assert_eq!(packet[..6], [0xBB, 0x00, 8, 0xB0, 0x00, 2]);
        assert_eq!(packet[6..12], [255, 0, 0, 0, 0, 255]);
        assert_eq!(xor(&packet[..packet.len() - 1]), *packet.last().unwrap());
        assert_eq!(razer_frame(&[[1, 2, 3]], true)[4], 1);
    }

    #[test]
    fn razer_is_wrapped_as_base64_json() {
        let text = String::from_utf8(lan_razer(&razer_enable(true))).unwrap();
        assert_eq!(text, r#"{"msg":{"cmd":"razer","data":{"pt":"uwABsQEK"}}}"#);
    }

    #[test]
    fn base64_pads_like_everyone_else() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn ble_packets_are_20_bytes_with_a_checksum() {
        for packet in [ble_keepalive(), ble_power(true), ble_color([1, 2, 3], BleColorCommand::Modern)] {
            assert_eq!(packet.len(), 20);
            assert_eq!(xor(&packet[..19]), packet[19]);
        }
        assert_eq!(ble_color([9, 8, 7], BleColorCommand::Legacy)[..6], [0x33, 0x05, 0x02, 9, 8, 7]);
        let segment = ble_segment_color([9, 8, 7], 0x0105, 2);
        assert_eq!(segment[..7], [0x33, 0x05, 0x15, 0x01, 9, 8, 7]);
        assert_eq!(segment[12..14], [0x05, 0x01]);
    }

    fn bytes(hex: &str) -> Vec<u8> {
        hex.split(' ').map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    // Captured from an H6097
    #[test]
    fn ble_encryption_matches_a_real_light() {
        let reply = bytes("D8 41 2E 4A 4E 73 BD 65 27 10 8C 41 E3 86 06 02 D0 9A 40 F9");
        let key = ble_handshake_reply(&reply, 1).unwrap();
        assert_eq!(key.to_vec(), bytes("5F 8F 36 1A 65 1D AC 58 CB 8B 82 76 91 D9 B8 74"));
        assert_eq!(ble_handshake_reply(&reply, 2), None);
        let keepalive = bytes("9D B2 58 16 92 04 DA EC 8D 40 D2 7F 35 56 9D 20 7E 74 44 DB");
        assert_eq!(ble_cipher(&ble_keepalive(), &key, true), keepalive);
        assert_eq!(ble_cipher(&keepalive, &key, false), ble_keepalive());
    }

    #[test]
    fn ble_handshakes_round_trip() {
        let frame = ble_handshake(2, [7; 17]);
        assert_eq!(frame.len(), 20);
        assert_eq!(ble_handshake_reply(&frame, 2), Some([7; 16]));
    }

    #[test]
    fn equal_colours_share_one_packet() {
        let colors = [[255, 0, 0], [0, 0, 255], [255, 0, 0], [250, 2, 1]];
        assert_eq!(
            group_segments(&colors, 0..4, 1),
            vec![([255, 0, 0], 0b0101), ([0, 0, 255], 0b0010), ([250, 2, 1], 0b1000)]
        );
        assert_eq!(group_segments(&colors, 0..4, 16).len(), 2);
        assert_eq!(group_segments(&colors, [1], 1), vec![([0, 0, 255], 0b0010)]);
    }

    #[test]
    fn replies_are_understood() {
        let scan = br#"{"msg":{"cmd":"scan","data":{"ip":"192.168.68.127","device":"A2:98:D0:36:39:34:47:1C","sku":"H6046","bleVersionHard":"3.02.01"}}}"#;
        assert_eq!(
            parse_reply(scan),
            Some(LanReply::Scan(LanDevice {
                ip: Ipv4Addr::new(192, 168, 68, 127),
                device_id: "A2:98:D0:36:39:34:47:1C".into(),
                sku: "H6046".into()
            }))
        );
        let status = br#"{"msg":{"cmd":"devStatus","data":{"onOff":0,"brightness":100,"color":{"r":255,"g":0,"b":0},"colorTemInKelvin":0}}}"#;
        assert_eq!(
            parse_reply(status),
            Some(LanReply::Status(DeviceState { on: false, brightness: 100, color: [255, 0, 0], kelvin: 0 }))
        );
        assert_eq!(parse_reply(b"nonsense"), None);
    }
}
