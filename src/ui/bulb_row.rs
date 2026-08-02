use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib::{self, SignalHandlerId};

use crate::ui::color_wheel::ColorWheel;
use crate::ui::util::{
    color_dot, format_hex, hsv_to_rgb, parse_hex, rgb_to_hsv, visible_rgb, SharedColors,
    Throttler,
};
use crate::ui::{Merged, Ui};

const COLOR_DURATION_MS: u32 = 250;

pub struct BulbRow {
    pub row: adw::ExpanderRow,
    power: gtk::Switch,
    brightness: gtk::Scale,
    kelvin: gtk::Scale,
    kelvin_row: adw::ActionRow,
    wheel: ColorWheel,
    wheel_row: gtk::ListBoxRow,
    hex_row: adw::EntryRow,
    colors_btn: gtk::ToggleButton,
    whites_btn: gtk::ToggleButton,
    room_row: adw::EntryRow,
    dot: gtk::DrawingArea,
    dot_color: SharedColors,
    /// Last non-zero saturation, restored when toggling back to Colors.
    last_sat: Rc<Cell<u16>>,
    h_power: SignalHandlerId,
    h_brightness: SignalHandlerId,
    h_kelvin: SignalHandlerId,
    h_colors: SignalHandlerId,
    h_whites: SignalHandlerId,
}

impl BulbRow {
    pub fn new(id: String, ui: Rc<Ui>) -> BulbRow {
        let row = adw::ExpanderRow::builder().show_enable_switch(false).build();

        // Colored dot showing the bulb's current color.
        let (dot, dot_color) = color_dot(45);
        row.add_prefix(&dot);

        let power = gtk::Switch::builder().valign(gtk::Align::Center).build();
        row.add_suffix(&power);
        let h_power = power.connect_active_notify({
            let ui = ui.clone();
            let id = id.clone();
            move |sw| ui.set_power(&id, sw.is_active())
        });

        // Colors / Whites mode toggle. Switching modes acts on the bulb:
        // Whites desaturates to warm white, Colors restores the last color.
        let colors_btn = gtk::ToggleButton::builder().label("Colors").active(true).build();
        let whites_btn = gtk::ToggleButton::builder().label("Whites").build();
        whites_btn.set_group(Some(&colors_btn));
        let mode_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .halign(gtk::Align::Center)
            .css_classes(["linked"])
            .margin_top(8)
            .margin_bottom(8)
            .build();
        mode_box.append(&colors_btn);
        mode_box.append(&whites_btn);
        let mode_row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .focusable(false)
            .child(&mode_box)
            .build();
        row.add_row(&mode_row);

        let last_sat = Rc::new(Cell::new(65535u16));
        let h_colors = colors_btn.connect_toggled({
            let ui = ui.clone();
            let id = id.clone();
            let last_sat = last_sat.clone();
            move |btn| {
                if btn.is_active() {
                    // Restore at least a clearly visible saturation.
                    let sat = last_sat.get().max(6554);
                    ui.adjust(&id, move |c| c.saturation = sat, COLOR_DURATION_MS);
                }
            }
        });
        let h_whites = whites_btn.connect_toggled({
            let ui = ui.clone();
            let id = id.clone();
            move |btn| {
                if btn.is_active() {
                    ui.adjust(&id, |c| c.saturation = 0, COLOR_DURATION_MS);
                }
            }
        });

        // Brightness (slider + percent field on one shared adjustment)
        let brightness = make_scale(1.0, 100.0);
        let bri_spin = gtk::SpinButton::builder()
            .adjustment(&brightness.adjustment())
            .climb_rate(5.0)
            .digits(0)
            .valign(gtk::Align::Center)
            .tooltip_text("Brightness in percent")
            .build();
        let bri_row = adw::ActionRow::builder().title("Brightness").build();
        bri_row.add_suffix(&brightness);
        bri_row.add_suffix(&bri_spin);
        row.add_row(&bri_row);
        let bri_throttle = Throttler::new(100);
        let h_brightness = brightness.connect_value_changed({
            let ui = ui.clone();
            let id = id.clone();
            move |scale| {
                let value = scale.value();
                let ui = ui.clone();
                let id = id.clone();
                bri_throttle.run(move || {
                    ui.adjust(
                        &id,
                        |c| c.brightness = ((value / 100.0) * 65535.0).round() as u16,
                        150,
                    );
                });
            }
        });

        // Color temperature (white light). The slider and the numeric kelvin
        // entry share one adjustment, so they always agree.
        let kelvin_adj = gtk::Adjustment::new(3500.0, 1500.0, 9000.0, 100.0, 500.0, 0.0);
        let kelvin = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&kelvin_adj)
            .valign(gtk::Align::Center)
            .build();
        kelvin.set_size_request(150, -1);
        kelvin.add_mark(2700.0, gtk::PositionType::Bottom, None);
        kelvin.add_mark(4000.0, gtk::PositionType::Bottom, None);
        kelvin.add_mark(6500.0, gtk::PositionType::Bottom, None);
        let kelvin_spin = gtk::SpinButton::builder()
            .adjustment(&kelvin_adj)
            .climb_rate(100.0)
            .digits(0)
            .valign(gtk::Align::Center)
            .tooltip_text("Color temperature in kelvin (1500–9000)")
            .build();
        let kelvin_row = adw::ActionRow::builder()
            .title("Warmth")
            .subtitle(lifx_core::describe_kelvin(3500))
            .build();
        kelvin_row.add_suffix(&kelvin);
        kelvin_row.add_suffix(&kelvin_spin);
        row.add_row(&kelvin_row);
        // Display-only: keep the shade name current no matter how the value
        // changes (drag, typed entry, or state sync from the bulb).
        kelvin_adj.connect_value_changed({
            let kelvin_row = kelvin_row.clone();
            move |adj| {
                kelvin_row.set_subtitle(lifx_core::describe_kelvin(adj.value().round() as u16));
            }
        });
        let kelvin_throttle = Throttler::new(100);
        let h_kelvin = kelvin.connect_value_changed({
            let ui = ui.clone();
            let id = id.clone();
            move |scale| {
                let value = scale.value();
                let ui = ui.clone();
                let id = id.clone();
                kelvin_throttle.run(move || {
                    ui.adjust(
                        &id,
                        |c| {
                            c.kelvin = value.round() as u16;
                            c.saturation = 0;
                        },
                        COLOR_DURATION_MS,
                    );
                });
            }
        });

        // Hue/saturation color wheel (LIFX-style; center = white).
        let wheel_throttle = Throttler::new(100);
        let wheel = ColorWheel::new({
            let ui = ui.clone();
            let id = id.clone();
            move |hue, sat| {
                let ui = ui.clone();
                let id = id.clone();
                wheel_throttle.run(move || {
                    ui.adjust(
                        &id,
                        move |c| {
                            c.hue = ((hue / 360.0) * 65535.0).round() as u16;
                            c.saturation = (sat.clamp(0.0, 1.0) * 65535.0).round() as u16;
                        },
                        COLOR_DURATION_MS,
                    );
                });
            }
        });
        wheel.widget.set_margin_top(12);
        wheel.widget.set_margin_bottom(12);
        let wheel_row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .focusable(false)
            .child(&wheel.widget)
            .build();
        row.add_row(&wheel_row);

        // Hex color entry (applied with the check button / Enter).
        let hex_row = adw::EntryRow::builder()
            .title("Hex color")
            .show_apply_button(true)
            .build();
        hex_row.connect_changed(|entry| {
            entry.remove_css_class("error");
        });
        hex_row.connect_apply({
            let ui = ui.clone();
            let id = id.clone();
            move |entry| match parse_hex(&entry.text()) {
                Some((r, g, b)) => {
                    entry.remove_css_class("error");
                    let (h, s, v) = rgb_to_hsv(r, g, b);
                    ui.adjust(
                        &id,
                        move |c| {
                            c.hue = ((h / 360.0) * 65535.0).round() as u16;
                            c.saturation = (s.clamp(0.0, 1.0) * 65535.0).round() as u16;
                            c.brightness = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
                        },
                        COLOR_DURATION_MS,
                    );
                }
                None => entry.add_css_class("error"),
            }
        });
        row.add_row(&hex_row);

        // Room assignment (applied with the check button / Enter).
        let room_row = adw::EntryRow::builder()
            .title("Room")
            .show_apply_button(true)
            .build();
        room_row.connect_apply({
            let ui = ui.clone();
            let id = id.clone();
            move |entry| ui.assign_room(&id, entry.text().trim())
        });
        row.add_row(&room_row);

        BulbRow {
            row,
            power,
            brightness,
            kelvin,
            kelvin_row,
            wheel,
            wheel_row,
            hex_row,
            colors_btn,
            whites_btn,
            room_row,
            dot,
            dot_color,
            last_sat,
            h_power,
            h_brightness,
            h_kelvin,
            h_colors,
            h_whites,
        }
    }

    /// Sync all widgets to the given merged state without triggering the
    /// user-input signal handlers.
    pub fn apply(&self, m: &Merged, room: &str) {
        let s = &m.state;
        self.row.set_title(&glib::markup_escape_text(&s.label));

        let via_lan = m.has_lan && m.lan_connected;
        let reachable = via_lan || (m.has_cloud && s.connected);
        let subtitle = if via_lan {
            "Local"
        } else if reachable {
            "Cloud"
        } else {
            "Offline"
        };
        self.row.set_subtitle(subtitle);
        self.row.set_sensitive(reachable);

        self.power.block_signal(&self.h_power);
        self.power.set_active(s.powered);
        self.power.unblock_signal(&self.h_power);

        self.brightness.block_signal(&self.h_brightness);
        self.brightness
            .set_value((s.color.brightness as f64 / 65535.0) * 100.0);
        self.brightness.unblock_signal(&self.h_brightness);

        self.kelvin.block_signal(&self.h_kelvin);
        self.kelvin.set_value(s.color.kelvin as f64);
        self.kelvin.unblock_signal(&self.h_kelvin);

        self.wheel.set_hs(
            (s.color.hue as f64 / 65535.0) * 360.0,
            s.color.saturation as f64 / 65535.0,
        );

        // Colors vs. Whites mode follows the bulb's saturation.
        let whites = s.color.saturation == 0;
        if !whites {
            self.last_sat.set(s.color.saturation);
        }
        self.colors_btn.block_signal(&self.h_colors);
        self.whites_btn.block_signal(&self.h_whites);
        self.colors_btn.set_active(!whites);
        self.whites_btn.set_active(whites);
        self.colors_btn.unblock_signal(&self.h_colors);
        self.whites_btn.unblock_signal(&self.h_whites);
        self.wheel_row.set_visible(!whites);
        self.hex_row.set_visible(!whites);
        self.kelvin_row.set_visible(whites);

        // Hex reflects the full HSB color so it round-trips exactly.
        let hex_focused = self
            .hex_row
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN);
        if !whites && !hex_focused {
            let (r, g, b) = hsv_to_rgb(
                (s.color.hue as f64 / 65535.0) * 360.0,
                s.color.saturation as f64 / 65535.0,
                s.color.brightness as f64 / 65535.0,
            );
            let hex = format_hex(r, g, b);
            if self.hex_row.text() != hex {
                self.hex_row.set_text(&hex);
            }
        }

        // Don't fight the user while they're typing a room name.
        let focused = self
            .room_row
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN);
        if !focused && self.room_row.text() != room {
            self.room_row.set_text(room);
        }

        let (r, g, b) = visible_rgb(&s.color);
        let dot_rgb = if s.powered && reachable {
            (r, g, b)
        } else {
            (0.45, 0.45, 0.45)
        };
        *self.dot_color.borrow_mut() = vec![dot_rgb];
        self.dot.queue_draw();
    }
}

fn make_scale(min: f64, max: f64) -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, 1.0);
    scale.set_size_request(150, -1);
    scale.set_valign(gtk::Align::Center);
    scale
}
