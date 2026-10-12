//! Turning zone averages into the colours a light shows: saturation, white balance, gamma,
//! black level, brightness, and a frame-rate independent fade.

use serde::{Deserialize, Serialize};

use crate::govee::Rgb;
use crate::zones::Sampling;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorSettings {
    /// 0..1
    pub brightness: f32,
    /// 1 leaves colours as they are on screen
    pub saturation: f32,
    /// Above 1 deepens dark tones, below 1 lifts them
    pub gamma: f32,
    /// Per channel gain, to match the lights' white to the screen's
    pub white_balance: [f32; 3],
    /// Zones darker than this turn the segment off
    pub black_level: u8,
    /// How long a change takes to fade in; 0 is instant
    pub smoothing_ms: f32,
    /// Nothing is sent until some channel moves at least this much
    pub min_delta: u8,
    pub sampling: Sampling,
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self {
            brightness: 1.0,
            saturation: 1.15,
            gamma: 1.0,
            white_balance: [1.0, 1.0, 1.0],
            black_level: 6,
            smoothing_ms: 120.0,
            min_delta: 3,
            sampling: Sampling::Vivid,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Pipeline {
    state: Vec<[f32; 3]>,
    /// True once the fade has caught up with the screen, so the engine can rest.
    pub settled: bool,
}

impl Pipeline {
    pub fn process(&mut self, raw: &[[f32; 3]], settings: &ColorSettings, dt_seconds: f32) -> Vec<Rgb> {
        let target: Vec<[f32; 3]> = raw.iter().map(|&c| grade(c, settings)).collect();
        if self.state.len() != target.len() || settings.smoothing_ms <= 0.0 {
            self.state = target.clone();
        } else {
            let alpha = 1.0 - (-dt_seconds.max(0.0) * 1000.0 / settings.smoothing_ms).exp();
            for (s, t) in self.state.iter_mut().zip(&target) {
                for c in 0..3 {
                    s[c] += alpha * (t[c] - s[c]);
                }
            }
        }
        self.settled = self.state.iter().zip(&target).all(|(s, t)| (0..3).all(|c| (s[c] - t[c]).abs() < 0.5));
        self.state.iter().map(|s| s.map(|v| v.round().clamp(0.0, 255.0) as u8)).collect()
    }
}

fn grade(c: [f32; 3], s: &ColorSettings) -> [f32; 3] {
    let luma = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    let mut out = [0.0; 3];
    for i in 0..3 {
        let v = (luma + (c[i] - luma) * s.saturation) * s.white_balance[i];
        let v = v.clamp(0.0, 255.0);
        out[i] = if s.gamma != 1.0 { 255.0 * (v / 255.0).powf(s.gamma) } else { v };
    }
    if out.iter().cloned().fold(0.0, f32::max) < s.black_level as f32 {
        return [0.0; 3];
    }
    out.map(|v| v * s.brightness.clamp(0.0, 1.0))
}

/// Whether `next` differs enough from what was last sent to be worth a packet.
pub fn changed(last: Option<&[Rgb]>, next: &[Rgb], min_delta: u8) -> bool {
    match last {
        Some(last) if last.len() == next.len() => {
            last.iter().zip(next).any(|(a, b)| (0..3).any(|c| a[c].abs_diff(b[c]) >= min_delta.max(1)))
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_level_switches_dim_zones_off() {
        let settings = ColorSettings { saturation: 1.0, black_level: 10, smoothing_ms: 0.0, ..Default::default() };
        let out = Pipeline::default().process(&[[200.0, 0.0, 0.0], [5.0, 5.0, 5.0]], &settings, 0.0);
        assert_eq!(out, vec![[200, 0, 0], [0, 0, 0]]);
    }

    #[test]
    fn fades_take_the_time_they_say() {
        let settings = ColorSettings { saturation: 1.0, black_level: 0, smoothing_ms: 100.0, ..Default::default() };
        let mut pipeline = Pipeline::default();
        pipeline.process(&[[200.0, 0.0, 0.0]], &settings, 0.0);
        // one time constant gets 1 - 1/e of the way, about 63%
        let half = pipeline.process(&[[0.0, 0.0, 0.0]], &settings, 0.1)[0][0];
        assert!((65..=80).contains(&half), "{half}");
        assert!(!pipeline.settled);
        for _ in 0..60 {
            pipeline.process(&[[0.0, 0.0, 0.0]], &settings, 0.1);
        }
        assert!(pipeline.settled);
    }

    #[test]
    fn saturation_and_brightness_shape_the_colour() {
        let settings =
            ColorSettings { saturation: 2.0, brightness: 0.5, black_level: 0, smoothing_ms: 0.0, ..Default::default() };
        let out = Pipeline::default().process(&[[150.0, 100.0, 100.0]], &settings, 0.0)[0];
        assert!(out[0] > 75 && out[1] < 50, "{out:?}");
    }

    #[test]
    fn small_changes_are_not_worth_sending() {
        assert!(changed(None, &[[1, 1, 1]], 3));
        assert!(!changed(Some(&[[10, 10, 10]]), &[[12, 10, 8]], 3));
        assert!(changed(Some(&[[10, 10, 10]]), &[[13, 10, 10]], 3));
    }
}
