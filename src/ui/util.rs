use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;

/// Convert HSV (h in degrees, s/v in 0..1) to RGB in 0..1.
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let h = h.rem_euclid(360.0) / 60.0;
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i as u32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Approximate the RGB tint of white light at a given color temperature
/// (Tanner Helland's curve fit), for previewing kelvin values.
pub fn kelvin_to_rgb(kelvin: u16) -> (f64, f64, f64) {
    let t = kelvin as f64 / 100.0;
    let r = if t <= 66.0 {
        255.0
    } else {
        329.698727446 * (t - 60.0).powf(-0.1332047592)
    };
    let g = if t <= 66.0 {
        99.4708025861 * t.ln() - 161.1195681661
    } else {
        288.1221695283 * (t - 60.0).powf(-0.0755148492)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.5177312231 * (t - 10.0).ln() - 305.0447927307
    };
    (
        (r / 255.0).clamp(0.0, 1.0),
        (g / 255.0).clamp(0.0, 1.0),
        (b / 255.0).clamp(0.0, 1.0),
    )
}

/// Rate-limits slider input so we don't flood bulbs with UDP/HTTP requests
/// while dragging. Runs at most one action per interval; the latest action
/// submitted during the cooldown fires when it ends.
#[derive(Clone)]
pub struct Throttler {
    inner: Rc<RefCell<ThrottlerInner>>,
    interval: Duration,
}

struct ThrottlerInner {
    last_fire: Option<Instant>,
    pending: Option<Box<dyn FnOnce()>>,
    scheduled: bool,
}

impl Throttler {
    pub fn new(interval_ms: u64) -> Self {
        Throttler {
            inner: Rc::new(RefCell::new(ThrottlerInner {
                last_fire: None,
                pending: None,
                scheduled: false,
            })),
            interval: Duration::from_millis(interval_ms),
        }
    }

    pub fn run(&self, f: impl FnOnce() + 'static) {
        let mut inner = self.inner.borrow_mut();
        let now = Instant::now();
        let ready = inner
            .last_fire
            .is_none_or(|t| now.duration_since(t) >= self.interval);
        if ready && !inner.scheduled {
            inner.last_fire = Some(now);
            drop(inner);
            f();
            return;
        }
        inner.pending = Some(Box::new(f));
        if !inner.scheduled {
            inner.scheduled = true;
            let elapsed = inner
                .last_fire
                .map(|t| now.duration_since(t))
                .unwrap_or_default();
            let delay = self.interval.saturating_sub(elapsed);
            let inner_rc = self.inner.clone();
            drop(inner);
            glib::timeout_add_local_once(delay, move || {
                let mut inner = inner_rc.borrow_mut();
                inner.scheduled = false;
                inner.last_fire = Some(Instant::now());
                let pending = inner.pending.take();
                drop(inner);
                if let Some(f) = pending {
                    f();
                }
            });
        }
    }
}

/// The color a bulb visually shows at full value (ignoring brightness).
pub fn visible_rgb(c: &crate::model::Hsbk) -> (f64, f64, f64) {
    if c.saturation == 0 {
        kelvin_to_rgb(c.kelvin)
    } else {
        hsv_to_rgb(
            (c.hue as f64 / 65535.0) * 360.0,
            c.saturation as f64 / 65535.0,
            1.0,
        )
    }
}

/// Shared mutable color list driving a [`color_dot`] swatch.
pub type SharedColors = Rc<RefCell<Vec<(f64, f64, f64)>>>;

/// Trace a rounded-rectangle path.
fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - radius, y + radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(x + w - radius, y + h - radius, radius, 0.0, FRAC_PI_2);
    cr.arc(x + radius, y + h - radius, radius, FRAC_PI_2, PI);
    cr.arc(x + radius, y + radius, radius, PI, 1.5 * PI);
    cr.close_path();
}

/// Chip corner radius, proportional to its height.
fn chip_radius(h: f64) -> f64 {
    (h * 0.4).min(12.0)
}

/// Fill a chip: gray when empty, solid for one color, and a horizontal
/// linear gradient blending through the colors when there are several.
fn paint_chip(cr: &gtk::cairo::Context, w: f64, h: f64, colors: &[(f64, f64, f64)]) {
    rounded_rect(cr, 1.5, 1.5, w, h, chip_radius(h));
    match colors {
        [] => cr.set_source_rgb(0.45, 0.45, 0.45),
        [(r, g, b)] => cr.set_source_rgb(*r, *g, *b),
        _ => {
            let gradient = gtk::cairo::LinearGradient::new(1.5, 0.0, 1.5 + w, 0.0);
            let last = (colors.len() - 1) as f64;
            for (i, (r, g, b)) in colors.iter().enumerate() {
                gradient.add_color_stop_rgb(i as f64 / last, *r, *g, *b);
            }
            let _ = cr.set_source(&gradient);
        }
    }
    let _ = cr.fill_preserve();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
    cr.set_line_width(1.0);
    let _ = cr.stroke();
}

/// A small rounded-rectangle color swatch (3:2 aspect; `width` sets the
/// size). Returns the widget and the shared color list that controls its
/// fill (call `queue_draw` after updating it). Multiple colors render as a
/// blended gradient.
pub fn color_dot(width: i32) -> (gtk::DrawingArea, SharedColors) {
    let colors: SharedColors = Rc::new(RefCell::new(vec![(0.45, 0.45, 0.45)]));
    let dot = gtk::DrawingArea::builder()
        .content_width(width)
        .content_height(width * 2 / 3)
        .valign(gtk::Align::Center)
        .build();
    let draw_colors = colors.clone();
    dot.set_draw_func(move |_, cr, w, h| {
        paint_chip(cr, w as f64 - 3.0, h as f64 - 3.0, &draw_colors.borrow());
    });
    (dot, colors)
}

/// Convert RGB in 0..1 to HSV (h in degrees, s/v in 0..1).
pub fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta < 1e-9 {
        0.0
    } else if (max - r).abs() < 1e-9 {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < 1e-9 {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let s = if max < 1e-9 { 0.0 } else { delta / max };
    (h, s, max)
}

/// Parse "#RRGGBB", "RRGGBB", "#RGB" or "RGB" into RGB in 0..1.
pub fn parse_hex(s: &str) -> Option<(f64, f64, f64)> {
    let s = s.trim().trim_start_matches('#');
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let (r, g, b) = match s.len() {
        6 => {
            let v = u32::from_str_radix(s, 16).ok()?;
            ((v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff)
        }
        3 => {
            let v = u32::from_str_radix(s, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            (r * 17, g * 17, b * 17)
        }
        _ => return None,
    };
    Some((r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0))
}

pub fn format_hex(r: f64, g: f64, b: f64) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (b.clamp(0.0, 1.0) * 255.0).round() as u8
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        assert_eq!(parse_hex("#FF8000"), Some((1.0, 128.0 / 255.0, 0.0)));
        assert_eq!(parse_hex("ff8000"), parse_hex("#FF8000"));
        assert_eq!(parse_hex("#F80"), Some((1.0, 136.0 / 255.0, 0.0)));
        assert_eq!(parse_hex("#GG0000"), None);
        assert_eq!(parse_hex("#FF80"), None);
        assert_eq!(format_hex(1.0, 128.0 / 255.0, 0.0), "#FF8000");

        let (h, s, v) = rgb_to_hsv(1.0, 0.5, 0.0);
        let (r, g, b) = hsv_to_rgb(h, s, v);
        assert!((r - 1.0).abs() < 1e-9 && (g - 0.5).abs() < 1e-9 && b.abs() < 1e-9);
    }
}

/// A static rounded-rectangle chip (3:2 aspect) previewing a set of colors
/// as a blended gradient — used for saved scenes. Gray when `colors` is
/// empty.
pub fn scene_chip(width: i32, colors: &[(f64, f64, f64)]) -> gtk::DrawingArea {
    let colors = colors.to_vec();
    let chip = gtk::DrawingArea::builder()
        .content_width(width)
        .content_height(width * 2 / 3)
        .valign(gtk::Align::Center)
        .build();
    chip.set_draw_func(move |_, cr, w, h| {
        paint_chip(cr, w as f64 - 3.0, h as f64 - 3.0, &colors);
    });
    chip
}
