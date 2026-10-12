//! Which part of the screen drives which segment of a light, and how a zone's colour is measured.
//!
//! Zones are rectangles `[x0, y0, x1, y1]` in 0..1, relative to the picture once black bars are
//! taken away, so a letterboxed film still reaches the edges of the lights.

use serde::{Deserialize, Serialize};

pub type Zone = [f32; 4];
/// The picture's rectangle inside the frame, in 0..1, after black bars are cut.
pub type Content = [f32; 4];
pub const FULL: Content = [0.0, 0.0, 1.0, 1.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Top,
    Right,
    Bottom,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Left, Side::Top, Side::Right, Side::Bottom];

    pub fn label(self) -> &'static str {
        match self {
            Side::Left => "Left",
            Side::Top => "Top",
            Side::Right => "Right",
            Side::Bottom => "Bottom",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layout {
    /// One colour for the whole light, from one region of the screen.
    Single { region: Zone },
    /// Segments around the screen's edges, in the order the strip runs.
    Edges {
        sides: Vec<Side>,
        /// Segments per side, in the order of `sides`
        counts: Vec<u32>,
        /// How far into the screen each zone reaches, as a fraction of the screen's height
        depth: f32,
        /// Clockwise as seen from the front: up the left, across the top, down the right
        clockwise: bool,
    },
    /// Rows and columns, for panels and curtain lights.
    Grid { rows: u32, cols: u32, snake: bool },
    /// One full-width band per segment, from the top of the screen down (vertical bars, lamps).
    Columns { count: u32, vertical: bool },
}

impl Default for Layout {
    fn default() -> Self {
        Layout::Single { region: FULL }
    }
}

impl Layout {
    pub fn edges(sides: &[Side], segments: u32, aspect: f32) -> Layout {
        let weights: Vec<f32> =
            sides.iter().map(|s| if matches!(s, Side::Top | Side::Bottom) { aspect } else { 1.0 }).collect();
        Layout::Edges { sides: sides.to_vec(), counts: split_counts(segments, &weights), depth: 0.15, clockwise: true }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Layout::Single { .. } => "Whole screen",
            Layout::Edges { .. } => "Screen edges",
            Layout::Grid { .. } => "Grid",
            Layout::Columns { .. } => "Bands",
        }
    }

    pub fn segments(&self) -> usize {
        match self {
            Layout::Single { .. } => 1,
            Layout::Edges { counts, .. } => counts.iter().sum::<u32>() as usize,
            Layout::Grid { rows, cols, .. } => (rows * cols) as usize,
            Layout::Columns { count, .. } => *count as usize,
        }
    }

    /// The zones, `reverse`d and rotated by `offset` to match where the strip starts.
    pub fn zones(&self, aspect: f32, reverse: bool, offset: i32) -> Vec<Zone> {
        let mut zones = match self {
            Layout::Single { region } => vec![*region],
            Layout::Edges { sides, counts, depth, clockwise } => {
                let (dy, dx) = (depth.clamp(0.01, 0.5), (depth / aspect.max(0.1)).clamp(0.005, 0.5));
                sides.iter().zip(counts).flat_map(|(&side, &k)| side_cells(side, k, dx, dy, *clockwise)).collect()
            }
            Layout::Grid { rows, cols, snake } => {
                let (rows, cols) = ((*rows).max(1), (*cols).max(1));
                let mut zones = Vec::new();
                for r in 0..rows {
                    let order: Vec<u32> =
                        if *snake && r % 2 == 1 { (0..cols).rev().collect() } else { (0..cols).collect() };
                    for c in order {
                        zones.push(cell(c, cols, r, rows));
                    }
                }
                zones
            }
            Layout::Columns { count, vertical } => {
                let n = (*count).max(1);
                (0..n).map(|i| if *vertical { cell(0, 1, i, n) } else { cell(i, n, 0, 1) }).collect()
            }
        };
        if reverse {
            zones.reverse();
        }
        if !zones.is_empty() {
            let len = zones.len() as i32;
            zones.rotate_right(offset.rem_euclid(len) as usize);
        }
        zones
    }
}

fn cell(c: u32, cols: u32, r: u32, rows: u32) -> Zone {
    let (cols, rows) = (cols as f32, rows as f32);
    [c as f32 / cols, r as f32 / rows, (c + 1) as f32 / cols, (r + 1) as f32 / rows]
}

fn side_cells(side: Side, k: u32, dx: f32, dy: f32, clockwise: bool) -> Vec<Zone> {
    (0..k)
        .map(|j| {
            let (a, b) = (j as f32 / k as f32, (j + 1) as f32 / k as f32);
            // Each side's natural clockwise direction: left goes up, top goes right, right goes down, bottom goes left
            let (lo, hi) = (a, b);
            let (rlo, rhi) = (1.0 - b, 1.0 - a);
            match (side, clockwise) {
                (Side::Left, true) | (Side::Right, false) => {
                    let x0 = if side == Side::Left { 0.0 } else { 1.0 - dx };
                    [x0, rlo, x0 + dx, rhi]
                }
                (Side::Left, false) | (Side::Right, true) => {
                    let x0 = if side == Side::Left { 0.0 } else { 1.0 - dx };
                    [x0, lo, x0 + dx, hi]
                }
                (Side::Top, true) | (Side::Bottom, false) => {
                    let y0 = if side == Side::Top { 0.0 } else { 1.0 - dy };
                    [lo, y0, hi, y0 + dy]
                }
                (Side::Top, false) | (Side::Bottom, true) => {
                    let y0 = if side == Side::Top { 0.0 } else { 1.0 - dy };
                    [rlo, y0, rhi, y0 + dy]
                }
            }
        })
        .collect()
}

/// Largest remainder split of `total` in proportion to `weights`, at least one each.
pub fn split_counts(total: u32, weights: &[f32]) -> Vec<u32> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let total = total.max(n as u32);
    let sum: f32 = weights.iter().sum::<f32>().max(f32::EPSILON);
    let raw: Vec<f32> = weights.iter().map(|w| total as f32 * w / sum).collect();
    let mut counts: Vec<u32> = raw.iter().map(|r| (*r as u32).max(1)).collect();
    while counts.iter().sum::<u32>() < total {
        let i = (0..n).max_by(|&a, &b| (raw[a] - counts[a] as f32).total_cmp(&(raw[b] - counts[b] as f32))).unwrap();
        counts[i] += 1;
    }
    while counts.iter().sum::<u32>() > total {
        let i = (0..n)
            .filter(|&i| counts[i] > 1)
            .max_by(|&a, &b| (counts[a] as f32 - raw[a]).total_cmp(&(counts[b] as f32 - raw[b])))
            .unwrap();
        counts[i] -= 1;
    }
    counts
}

/// A small screen capture: BGRA, rows packed with no padding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

impl Frame {
    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height.max(1) as f32
    }

    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 4;
        [self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sampling {
    /// Plain average of the zone
    Mean,
    /// Favours colourful pixels, so a red car on grey tarmac still reads red
    #[default]
    Vivid,
}

/// The average colour of every zone, in 0..255 floats.
pub fn sample(frame: &Frame, zones: &[Zone], content: Content, mode: Sampling) -> Vec<[f32; 3]> {
    let (w, h) = (frame.width as f32, frame.height as f32);
    let [cx0, cy0, cx1, cy1] = content;
    let (cw, ch) = (cx1 - cx0, cy1 - cy0);
    zones
        .iter()
        .map(|&[x0, y0, x1, y1]| {
            if frame.width == 0 || frame.height == 0 {
                return [0.0; 3];
            }
            let px0 = (((cx0 + x0 * cw) * w) as usize).min(frame.width - 1);
            let py0 = (((cy0 + y0 * ch) * h) as usize).min(frame.height - 1);
            let px1 = (((cx0 + x1 * cw) * w).ceil() as usize).clamp(px0 + 1, frame.width);
            let py1 = (((cy0 + y1 * ch) * h).ceil() as usize).clamp(py0 + 1, frame.height);
            let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
            for y in py0..py1 {
                for x in px0..px1 {
                    let p = frame.pixel(x, y);
                    let wgt = match mode {
                        Sampling::Mean => 1.0,
                        Sampling::Vivid => {
                            let chroma = (p.iter().max().unwrap() - p.iter().min().unwrap()) as f32 / 255.0;
                            chroma * chroma + 0.02
                        }
                    };
                    for c in 0..3 {
                        sum[c] += p[c] as f32 * wgt;
                    }
                    weight += wgt;
                }
            }
            sum.map(|s| s / weight)
        })
        .collect()
}

/// Finds symmetric black bars (letterbox and pillarbox) and only believes a change once it has
/// held for a few frames, so a dark scene doesn't make the zones jump around.
#[derive(Clone, Debug)]
pub struct Letterbox {
    pub content: Content,
    pending: Content,
    count: u32,
}

impl Default for Letterbox {
    fn default() -> Self {
        Self { content: FULL, pending: FULL, count: 0 }
    }
}

const BAR_THRESHOLD: u8 = 14;
const STABLE_FRAMES: u32 = 6;
const MAX_BAR: f32 = 0.3;

impl Letterbox {
    pub fn update(&mut self, frame: &Frame) -> Content {
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 {
            return self.content;
        }
        let bright = |x: usize, y: usize| frame.pixel(x, y).iter().any(|&v| v > BAR_THRESHOLD);
        // A line is part of a bar when under 2% of it is lit, which lets subtitles sit in the bar
        let dark_row = |y: usize| (0..w).filter(|&x| bright(x, y)).count() * 50 < w;
        let dark_col = |x: usize| (0..h).filter(|&y| bright(x, y)).count() * 50 < h;
        let rows: Vec<bool> = (0..h).map(dark_row).collect();
        if rows.iter().all(|&d| d) {
            return self.content; // a black frame says nothing about bars
        }
        let cols: Vec<bool> = (0..w).map(dark_col).collect();
        let bars = |dark: &[bool]| {
            let lead = dark.iter().take_while(|&&d| d).count();
            let trail = dark.iter().rev().take_while(|&&d| d).count();
            lead.min(trail).min((dark.len() as f32 * MAX_BAR) as usize)
        };
        let (by, bx) = (bars(&rows) as f32 / h as f32, bars(&cols) as f32 / w as f32);
        let candidate = [bx, by, 1.0 - bx, 1.0 - by];
        if candidate == self.content {
            self.count = 0;
        } else {
            if candidate == self.pending {
                self.count += 1;
            } else {
                (self.pending, self.count) = (candidate, 1);
            }
            if self.count >= STABLE_FRAMES {
                (self.content, self.count) = (candidate, 0);
            }
        }
        self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> Frame {
        let mut bgra = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = paint(x, y);
                bgra.extend_from_slice(&[b, g, r, 255]);
            }
        }
        Frame { width: w, height: h, bgra }
    }

    #[test]
    fn edges_walk_clockwise_from_the_bottom_left() {
        let layout = Layout::Edges {
            sides: vec![Side::Left, Side::Top, Side::Right],
            counts: vec![2, 2, 2],
            depth: 0.1,
            clockwise: true,
        };
        let z = layout.zones(1.0, false, 0);
        assert_eq!(z.len(), 6);
        assert!(z[0][1] > z[1][1], "left side goes up");
        assert!(z[2][0] < z[3][0], "top goes right");
        assert!(z[4][1] < z[5][1], "right side goes down");
        let ccw = Layout::Edges {
            sides: vec![Side::Right, Side::Top, Side::Left],
            counts: vec![2, 2, 2],
            depth: 0.1,
            clockwise: false,
        };
        let z = ccw.zones(1.0, false, 0);
        assert!(z[0][1] > z[1][1], "counter-clockwise, the right side goes up");
        assert!(z[2][0] > z[3][0], "and the top goes left");
    }

    #[test]
    fn wide_screens_give_the_top_more_segments() {
        assert_eq!(split_counts(20, &[1.0, 16.0 / 9.0, 1.0]), vec![5, 10, 5]);
        assert_eq!(split_counts(2, &[1.0, 1.0, 1.0]), vec![1, 1, 1]);
        match Layout::edges(&[Side::Left, Side::Top, Side::Right], 15, 3.55) {
            Layout::Edges { counts, .. } => assert_eq!(counts.iter().sum::<u32>(), 15),
            _ => unreachable!(),
        }
    }

    #[test]
    fn reverse_and_offset_move_the_start() {
        let layout = Layout::Columns { count: 4, vertical: false };
        let plain = layout.zones(1.0, false, 0);
        assert_eq!(layout.zones(1.0, true, 0)[0], plain[3]);
        assert_eq!(layout.zones(1.0, false, 1)[1], plain[0]);
    }

    #[test]
    fn grids_can_snake() {
        let z = Layout::Grid { rows: 2, cols: 2, snake: true }.zones(1.0, false, 0);
        assert_eq!((z[2][0], z[3][0]), (0.5, 0.0));
    }

    #[test]
    fn sampling_follows_the_content_box() {
        let f = frame(160, 90, |x, y| {
            if !(10..80).contains(&y) {
                [0, 0, 0]
            } else if x < 80 {
                [200, 0, 0]
            } else {
                [0, 0, 200]
            }
        });
        let mut letterbox = Letterbox::default();
        let mut content = FULL;
        for _ in 0..STABLE_FRAMES {
            content = letterbox.update(&f);
        }
        assert!((content[1] - 10.0 / 90.0).abs() < 1e-6);
        let zones = Layout::Grid { rows: 1, cols: 2, snake: false }.zones(1.0, false, 0);
        let colors = sample(&f, &zones, content, Sampling::Mean);
        assert!((colors[0][0] - 200.0).abs() < 0.01 && (colors[1][2] - 200.0).abs() < 0.01);
    }

    #[test]
    fn vivid_sampling_prefers_colour() {
        let f = frame(10, 10, |_, y| if y < 2 { [255, 0, 0] } else { [128, 128, 128] });
        let zones = [FULL];
        let vivid = sample(&f, &zones, FULL, Sampling::Vivid)[0];
        let mean = sample(&f, &zones, FULL, Sampling::Mean)[0];
        assert!(vivid[0] - vivid[1] > mean[0] - mean[1]);
    }
}
