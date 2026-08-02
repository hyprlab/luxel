//! A LIFX-style hue/saturation color wheel: hue around the rim, white in the
//! center (saturation grows with radius). Drag or tap anywhere to pick.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::cairo;
use gtk::prelude::*;

use crate::ui::util::hsv_to_rgb;

const WHEEL_SIZE: i32 = 216;
const THUMB_RADIUS: f64 = 10.0;

pub struct ColorWheel {
    pub widget: gtk::DrawingArea,
    /// (hue degrees 0–360, saturation 0–1)
    hs: Rc<Cell<(f64, f64)>>,
}

impl ColorWheel {
    pub fn new(on_change: impl Fn(f64, f64) + 'static) -> ColorWheel {
        let area = gtk::DrawingArea::builder()
            .content_width(WHEEL_SIZE)
            .content_height(WHEEL_SIZE)
            .halign(gtk::Align::Center)
            .build();
        let hs: Rc<Cell<(f64, f64)>> = Rc::new(Cell::new((0.0, 0.0)));
        let surface: Rc<RefCell<Option<(i32, cairo::ImageSurface)>>> =
            Rc::new(RefCell::new(None));

        area.set_draw_func({
            let hs = hs.clone();
            let surface = surface.clone();
            move |_, cr, w, h| {
                let size = WHEEL_SIZE.min(w.min(h));
                {
                    let mut cache = surface.borrow_mut();
                    if cache.as_ref().map(|(s, _)| *s) != Some(size) {
                        *cache = Some((size, build_wheel_surface(size)));
                    }
                }
                let cache = surface.borrow();
                let (_, surf) = cache.as_ref().unwrap();

                let ox = (w - size) as f64 / 2.0;
                let oy = (h - size) as f64 / 2.0;
                let _ = cr.set_source_surface(surf, ox, oy);
                let _ = cr.paint();

                // Thumb
                let (hue, sat) = hs.get();
                let center_x = w as f64 / 2.0;
                let center_y = h as f64 / 2.0;
                let reach = size as f64 / 2.0 - THUMB_RADIUS - 2.0;
                let angle = hue.to_radians();
                let tx = center_x + angle.cos() * sat * reach;
                let ty = center_y + angle.sin() * sat * reach;
                let (r, g, b) = hsv_to_rgb(hue, sat, 1.0);
                cr.arc(tx, ty, THUMB_RADIUS, 0.0, 2.0 * PI);
                cr.set_source_rgb(r, g, b);
                let _ = cr.fill_preserve();
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(2.5);
                let _ = cr.stroke_preserve();
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            }
        });

        let on_change = Rc::new(on_change);
        let pick = {
            let hs = hs.clone();
            let area = area.clone();
            let on_change = on_change.clone();
            move |x: f64, y: f64| {
                let w = area.width() as f64;
                let h = area.height() as f64;
                let size = (WHEEL_SIZE as f64).min(w.min(h));
                let dx = x - w / 2.0;
                let dy = y - h / 2.0;
                let reach = size / 2.0 - THUMB_RADIUS - 2.0;
                let mut sat = ((dx * dx + dy * dy).sqrt() / reach).clamp(0.0, 1.0);
                // Snap to pure white near the center.
                if sat < 0.04 {
                    sat = 0.0;
                }
                let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                hs.set((hue, sat));
                area.queue_draw();
                on_change(hue, sat);
            }
        };

        let drag = gtk::GestureDrag::new();
        let start = Rc::new(Cell::new((0.0f64, 0.0f64)));
        drag.connect_drag_begin({
            let start = start.clone();
            let pick = pick.clone();
            move |_, x, y| {
                start.set((x, y));
                pick(x, y);
            }
        });
        drag.connect_drag_update({
            let start = start.clone();
            let pick = pick.clone();
            move |_, dx, dy| {
                let (sx, sy) = start.get();
                pick(sx + dx, sy + dy);
            }
        });
        area.add_controller(drag);

        ColorWheel { widget: area, hs }
    }

    /// Move the thumb without firing the change callback.
    pub fn set_hs(&self, hue_deg: f64, sat: f64) {
        self.hs.set((hue_deg.rem_euclid(360.0), sat.clamp(0.0, 1.0)));
        self.widget.queue_draw();
    }
}

/// Render the hue/saturation disk once per size into an image surface.
fn build_wheel_surface(size: i32) -> cairo::ImageSurface {
    let mut surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).expect("surface");
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().expect("surface data");
        let center = (size as f64 - 1.0) / 2.0;
        let radius = size as f64 / 2.0 - 1.0;
        let aa = 1.5 / radius; // anti-alias band width, in normalized radius
        for y in 0..size as usize {
            for x in 0..size as usize {
                let dx = x as f64 - center;
                let dy = y as f64 - center;
                let dist = (dx * dx + dy * dy).sqrt() / radius;
                let alpha = ((1.0 + aa - dist) / aa).clamp(0.0, 1.0);
                if alpha <= 0.0 {
                    continue;
                }
                let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                let sat = dist.min(1.0);
                let (r, g, b) = hsv_to_rgb(hue, sat, 1.0);
                // Premultiplied ARGB32 in native (little-endian) order: B G R A.
                let i = y * stride + x * 4;
                data[i] = (b * alpha * 255.0) as u8;
                data[i + 1] = (g * alpha * 255.0) as u8;
                data[i + 2] = (r * alpha * 255.0) as u8;
                data[i + 3] = (alpha * 255.0) as u8;
            }
        }
    }
    surface
}
