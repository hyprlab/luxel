mod bulb_row;
mod color_wheel;
mod util;
mod wizard;

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib::SignalHandlerId;
use gtk::{gio, glib};

use crate::config::{Config, Scene, SceneBulb, ScenesFile, TuyaDevice};
use crate::model::{
    Backend, BulbState, CloudCommand, DeviceKind, Event, Hsbk, LanCommand, Subnet, TuyaCommand,
};
use crate::{cloud, lan, tuya};
use bulb_row::BulbRow;
use color_wheel::ColorWheel;
use util::{color_dot, disable_scroll, scene_chip, visible_rgb, SharedColors, Throttler};

/// Room name for bulbs with no LIFX group and no user-assigned room.
const UNGROUPED: &str = "Other";

/// A bulb as shown in the UI: the merged view of what the LAN and Cloud
/// backends each know about it.
#[derive(Debug, Clone)]
pub struct Merged {
    pub state: BulbState,
    pub has_lan: bool,
    pub has_cloud: bool,
    pub lan_connected: bool,
    pub lan_target: Option<u64>,
}

/// One room's UI: a boxed-list card whose first row is the room header
/// (styled exactly like the All Lights row); the bulb rows live in a nested
/// list inside a revealer row of the same card, so clicking the header
/// expands/collapses them with a slide animation.
struct RoomSection {
    root: gtk::ListBox,
    header: adw::ActionRow,
    lights_row: gtk::ListBoxRow,
    revealer: gtk::Revealer,
    list: gtk::ListBox,
    switch: gtk::Switch,
    h_switch: SignalHandlerId,
    scale: gtk::Scale,
    h_scale: SignalHandlerId,
    spin: gtk::SpinButton,
    color_btn: gtk::MenuButton,
    dot: gtk::DrawingArea,
    dot_color: SharedColors,
}

pub struct Ui {
    pub window: adw::ApplicationWindow,
    stack: gtk::Stack,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    scenes_group: adw::PreferencesGroup,
    scene_rows: RefCell<Vec<adw::ActionRow>>,
    house_switch: gtk::Switch,
    h_house_switch: OnceCell<SignalHandlerId>,
    house_scale: gtk::Scale,
    h_house_scale: OnceCell<SignalHandlerId>,
    house_spin: gtk::SpinButton,
    house_color_btn: gtk::MenuButton,
    house_dot: gtk::DrawingArea,
    house_dot_color: SharedColors,
    rooms_box: gtk::Box,
    sections: RefCell<HashMap<String, RoomSection>>,
    rows: RefCell<HashMap<String, BulbRow>>,
    /// Which room section each bulb's row currently lives in.
    row_room: RefCell<HashMap<String, String>>,
    merged: RefCell<HashMap<String, Merged>>,
    lan_tx: mpsc::Sender<LanCommand>,
    cloud_tx: mpsc::Sender<CloudCommand>,
    tuya_tx: mpsc::Sender<TuyaCommand>,
    config: RefCell<Config>,
}

fn room_sort_key(name: &str) -> (bool, String) {
    (name == UNGROUPED, name.to_lowercase())
}

fn list_sort_by_title(a: &gtk::ListBoxRow, b: &gtk::ListBoxRow) -> gtk::Ordering {
    let title = |row: &gtk::ListBoxRow| {
        row.downcast_ref::<adw::PreferencesRow>()
            .map(|r| r.title().to_lowercase())
            .unwrap_or_default()
    };
    title(a).cmp(&title(b)).into()
}

/// Animate a room card open or closed.
fn apply_expansion(section: &RoomSection, expanded: bool) {
    if expanded {
        section.lights_row.set_visible(true);
        section.revealer.set_reveal_child(true);
    } else {
        // The row itself is hidden once the slide-up finishes (see the
        // child-revealed handler) so no separator is left behind.
        section.revealer.set_reveal_child(false);
    }
}

fn compact_scale() -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 100.0, 1.0);
    scale.set_size_request(120, -1);
    scale.set_valign(gtk::Align::Center);
    scale.set_tooltip_text(Some("Brightness"));
    disable_scroll(&scale);
    scale
}

/// A percent field bound to a brightness slider's adjustment.
fn percent_spin(adjustment: &gtk::Adjustment) -> gtk::SpinButton {
    gtk::SpinButton::builder()
        .adjustment(adjustment)
        .climb_rate(5.0)
        .digits(0)
        .valign(gtk::Align::Center)
        .tooltip_text("Brightness in percent")
        .build()
}

const APP_CSS: &str = "
@define-color window_bg_color #171021;
@define-color view_bg_color #1d1429;
@define-color headerbar_bg_color #1d1429;
@define-color dialog_bg_color #1d1429;
@define-color popover_bg_color #261a37;
@define-color card_bg_color alpha(#b794ff, 0.08);
@define-color accent_bg_color #7b2bf9;
@define-color accent_color #b794ff;
.about-version-chip {
  background-color: alpha(@accent_color, 0.15);
  color: @accent_color;
  border-radius: 999px;
  padding: 1px 12px;
  font-weight: 600;
}
.about-coffee { font-size: 1.2em; }
.plug-chip {
  background-color: @accent_bg_color;
  color: white;
  border-radius: 999px;
  padding: 3px 8px;
  font-size: 0.75em;
  font-weight: 700;
}
.plug-chip.off {
  background-color: alpha(@accent_bg_color, 0.22);
  color: alpha(white, 0.5);
}
list.room-lights { background: transparent; }
list.room-lights > row { background: transparent; }
list.room-lights > row + row { border-top: 1px solid alpha(currentColor, 0.08); }
";

pub fn activate(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    // Make the bundled icons available no matter how the app was launched
    // (installed themes only cover the flatpak): write them to the cache
    // dir and register it as an unthemed icon search path.
    let icon_dir = glib::user_cache_dir().join("luxel").join("icons");
    let _ = std::fs::create_dir_all(&icon_dir);
    let bundled: [(&str, &[u8]); 3] = [
        (
            "external-link-symbolic.svg",
            include_bytes!("../../data/icons/external-link-symbolic.svg"),
        ),
        (
            "update-symbolic.svg",
            include_bytes!("../../data/icons/update-symbolic.svg"),
        ),
        (
            "io.github.hyprlab.Luxel.png",
            include_bytes!("../../data/icons/io.github.hyprlab.Luxel-256.png"),
        ),
    ];
    for (name, bytes) in bundled {
        let _ = std::fs::write(icon_dir.join(name), bytes);
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&icon_dir);
    }

    // Dark purple theme, dark-only (no light mode).
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let css = gtk::CssProvider::new();
    css.load_from_string(APP_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let (event_tx, event_rx) = async_channel::unbounded::<Event>();
    let (lan_tx, lan_rx) = mpsc::channel();
    let (cloud_tx, cloud_rx) = mpsc::channel();
    let (tuya_tx, tuya_rx) = mpsc::channel();
    lan::spawn(event_tx.clone(), lan_rx);
    cloud::spawn(event_tx.clone(), cloud_rx);
    tuya::spawn(event_tx, tuya_rx);

    let config = Config::load();
    let _ = cloud_tx.send(CloudCommand::Configure {
        token: config.cloud_token.clone(),
        enabled: config.cloud_enabled,
    });
    if !config.tuya_devices.is_empty() {
        let _ = tuya_tx.send(TuyaCommand::Configure(config.tuya_devices.clone()));
    }
    let subnets: Vec<Subnet> = config
        .lan_subnets
        .iter()
        .filter_map(|s| Subnet::parse(s))
        .collect();
    if !subnets.is_empty() {
        let _ = lan_tx.send(LanCommand::SetSubnets(subnets));
    }

    // Header bar
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Luxel", ""))
        .build();
    let refresh_btn = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Rescan for lights")
        .action_name("app.refresh")
        .build();
    header.pack_start(&refresh_btn);
    // Primary menu per the GNOME HIG: sections, Settings then Keyboard
    // Shortcuts, About last, no Quit item.
    let menu = gio::Menu::new();
    let menu_main = gio::Menu::new();
    menu_main.append(Some("_Settings"), Some("app.preferences"));
    menu_main.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
    menu.append_section(None, &menu_main);
    let menu_about = gio::Menu::new();
    menu_about.append(Some("_About Luxel"), Some("app.about"));
    menu.append_section(None, &menu_about);
    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .build();
    header.pack_end(&menu_btn);

    // Cloud error banner
    let banner = adw::Banner::builder().button_label("Settings").build();
    banner.connect_button_clicked(|_| {
        if let Some(app) = gio::Application::default() {
            app.activate_action("preferences", None);
        }
    });

    // Empty / searching page
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .build();
    let prefs_btn = gtk::Button::builder()
        .label("Settings…")
        .action_name("app.preferences")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    let empty_child = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();
    empty_child.append(&spinner);
    empty_child.append(&prefs_btn);
    let empty_page = adw::StatusPage::builder()
        .title("Looking for Lights")
        .description(
            "Make sure your LIFX bulbs are powered on and connected \
             to the same network as this computer. Bulbs on another \
             subnet, the LIFX Cloud, and SmartLife/Tuya smart plugs \
             can be set up in Settings.",
        )
        .child(&empty_child)
        .build();

    // Scenes
    let scenes_group = adw::PreferencesGroup::builder()
        .title("Scenes")
        .description("Snapshots of all lights you can restore with one click")
        .build();
    let save_scene_btn = gtk::Button::builder()
        .child(&adw::ButtonContent::builder()
            .icon_name("list-add-symbolic")
            .label("Save Current")
            .build())
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .build();
    let scenes_menu = gio::Menu::new();
    scenes_menu.append(Some("_Import Scenes…"), Some("app.import-scenes"));
    scenes_menu.append(Some("_Export Scenes…"), Some("app.export-scenes"));
    let scenes_more_btn = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&scenes_menu)
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text("Import or export scenes")
        .build();
    let scenes_header_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    scenes_header_box.append(&save_scene_btn);
    scenes_header_box.append(&scenes_more_btn);
    scenes_group.set_header_suffix(Some(&scenes_header_box));

    // Whole-house master controls
    let house_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    let house_scale = compact_scale();
    let (house_dot, house_dot_color) = color_dot(45);
    let house_color_btn = gtk::MenuButton::builder()
        .child(&house_dot)
        .tooltip_text("Color of all lights")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let house_row = adw::ActionRow::builder()
        .title("All Lights")
        .subtitle("Whole house")
        .build();
    let house_spin = percent_spin(&house_scale.adjustment());
    house_row.add_suffix(&house_color_btn);
    house_row.add_suffix(&house_scale);
    house_row.add_suffix(&house_spin);
    house_row.add_suffix(&house_switch);
    let house_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    house_list.append(&house_row);

    // Room sections live here
    let rooms_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .build();

    let scroll_page = |child: &gtk::Widget| {
        let clamp = adw::Clamp::builder()
            .maximum_size(620)
            .margin_top(18)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .child(child)
            .build();
        gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build()
    };

    // Lights tab: whole-house card + room sections.
    let lights_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .build();
    lights_box.append(&house_list);
    lights_box.append(&rooms_box);

    let view_stack = adw::ViewStack::new();
    view_stack.add_titled_with_icon(
        &scroll_page(lights_box.upcast_ref()),
        Some("lights"),
        "Lights",
        "weather-clear-symbolic",
    );
    view_stack.add_titled_with_icon(
        &scroll_page(scenes_group.upcast_ref()),
        Some("scenes"),
        "Scenes",
        "starred-symbolic",
    );
    header.set_title_widget(Some(
        &adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build(),
    ));

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&empty_page, Some("empty"));
    stack.add_named(&view_stack, Some("list"));
    stack.set_visible_child_name("empty");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&banner);
    content.append(&stack);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&content));

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&toasts));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Luxel")
        .default_width(480)
        .default_height(760)
        .content(&toolbar_view)
        .build();

    let ui = Rc::new(Ui {
        window: window.clone(),
        stack,
        banner,
        toasts,
        scenes_group,
        scene_rows: RefCell::new(Vec::new()),
        house_switch: house_switch.clone(),
        h_house_switch: OnceCell::new(),
        house_scale: house_scale.clone(),
        h_house_scale: OnceCell::new(),
        house_spin,
        house_color_btn: house_color_btn.clone(),
        house_dot,
        house_dot_color,
        rooms_box,
        sections: RefCell::new(HashMap::new()),
        rows: RefCell::new(HashMap::new()),
        row_room: RefCell::new(HashMap::new()),
        merged: RefCell::new(HashMap::new()),
        lan_tx,
        cloud_tx,
        tuya_tx,
        config: RefCell::new(config),
    });

    // House controls (connected after Ui exists so handlers can reach it).
    let h = house_switch.connect_active_notify({
        let ui = ui.clone();
        move |sw| ui.room_set_power(None, sw.is_active())
    });
    let _ = ui.h_house_switch.set(h);
    let house_throttle = Throttler::new(120);
    let h = house_scale.connect_value_changed({
        let ui = ui.clone();
        move |scale| {
            let value = scale.value();
            let ui = ui.clone();
            house_throttle.run(move || ui.room_set_brightness(None, value));
        }
    });
    let _ = ui.h_house_scale.set(h);
    house_color_btn.set_popover(Some(&color_popover(&ui, None, house_scale.adjustment())));

    save_scene_btn.connect_clicked({
        let ui = ui.clone();
        move |_| show_save_scene_dialog(&ui)
    });
    ui.rebuild_scenes();

    // Application actions
    let refresh = gio::ActionEntry::builder("refresh")
        .activate({
            let ui = ui.clone();
            move |_: &adw::Application, _, _| {
                let _ = ui.lan_tx.send(LanCommand::Discover);
                let _ = ui.cloud_tx.send(CloudCommand::Refresh);
                let _ = ui.tuya_tx.send(TuyaCommand::Refresh);
            }
        })
        .build();
    let preferences = gio::ActionEntry::builder("preferences")
        .activate({
            let ui = ui.clone();
            move |_: &adw::Application, _, _| show_preferences(&ui)
        })
        .build();
    let shortcuts = gio::ActionEntry::builder("shortcuts")
        .activate({
            let ui = ui.clone();
            move |_: &adw::Application, _, _| show_shortcuts(&ui)
        })
        .build();
    let about = gio::ActionEntry::builder("about")
        .activate({
            let ui = ui.clone();
            move |_: &adw::Application, _, _| show_about(&ui)
        })
        .build();
    let quit = gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| app.quit())
        .build();
    let export_scenes = gio::ActionEntry::builder("export-scenes")
        .activate({
            let ui = ui.clone();
            move |_: &adw::Application, _, _| export_scenes_dialog(&ui)
        })
        .build();
    let import_scenes = gio::ActionEntry::builder("import-scenes")
        .activate({
            let ui = ui.clone();
            move |_: &adw::Application, _, _| import_scenes_dialog(&ui)
        })
        .build();
    app.add_action_entries([
        refresh,
        preferences,
        shortcuts,
        about,
        quit,
        export_scenes,
        import_scenes,
    ]);
    app.set_accels_for_action("app.refresh", &["<primary>r"]);
    app.set_accels_for_action("app.preferences", &["<primary>comma"]);
    app.set_accels_for_action("app.shortcuts", &["<primary>question"]);
    app.set_accels_for_action("app.quit", &["<primary>q"]);

    // Hidden demo mode for screenshots: LUXEL_DEMO=1 populates sample
    // bulbs and scenes instead of waiting for discovery. Point
    // XDG_CONFIG_HOME somewhere disposable when using it.
    if let Some(mode) = std::env::var_os("LUXEL_DEMO") {
        demo_populate(&ui);
        if mode == "scenes" {
            view_stack.set_visible_child_name("scenes");
        }
    }

    // Pump backend events into the UI.
    glib::spawn_future_local({
        let ui = ui.clone();
        async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    Event::Upsert(state) => ui.upsert(state),
                    Event::CloudError(Some(msg)) => {
                        ui.banner.set_title(&msg);
                        ui.banner.set_revealed(true);
                    }
                    Event::CloudError(None) => ui.banner.set_revealed(false),
                    Event::TuyaFound { id, host, version } => {
                        ui.tuya_found(&id, &host, &version);
                    }
                    Event::TuyaLocateDone { found } => {
                        ui.toast(&match found {
                            0 => "Network scan finished — no devices found. Check the \
                                  subnet and that the devices are powered."
                                .to_string(),
                            1 => "Network scan finished — found 1 device".to_string(),
                            n => format!("Network scan finished — found {n} devices"),
                        });
                    }
                }
            }
        }
    });

    window.present();
}

impl Ui {
    fn upsert(self: &Rc<Self>, state: BulbState) {
        let id = state.id.clone();
        let mut map = self.merged.borrow_mut();
        let m = map.entry(id.clone()).or_insert_with(|| Merged {
            state: state.clone(),
            has_lan: false,
            has_cloud: false,
            lan_connected: false,
            lan_target: None,
        });
        match state.backend {
            Backend::Lan => {
                m.has_lan = true;
                m.lan_connected = state.connected;
                m.lan_target = state.lan_target;
                let fallback_group = m.state.group.take();
                m.state = state;
                if m.state.group.is_none() {
                    m.state.group = fallback_group;
                }
            }
            Backend::Cloud => {
                m.has_cloud = true;
                if m.has_lan && m.lan_connected {
                    // The local view is authoritative while reachable; only
                    // borrow metadata the LAN backend may not have yet.
                    if m.state.group.is_none() {
                        m.state.group = state.group;
                    }
                } else {
                    let lan_target = m.lan_target;
                    let fallback_group = m.state.group.take();
                    m.state = state;
                    m.state.lan_target = lan_target;
                    if m.state.group.is_none() {
                        m.state.group = fallback_group;
                    }
                }
            }
            // Tuya ids are namespaced ("tuya:..."), so a Tuya device is never
            // another view of a LIFX bulb: its backend is the only source.
            Backend::Tuya => {
                m.state = state;
            }
        }
        drop(map);
        self.place_and_refresh(&id);
    }

    /// The room a bulb belongs to: user override, else the bulb's LIFX group.
    fn room_name(&self, id: &str, m: &Merged) -> String {
        if let Some(room) = self.config.borrow().rooms.get(id) {
            if !room.is_empty() {
                return room.clone();
            }
        }
        m.state
            .group
            .clone()
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| UNGROUPED.to_string())
    }

    /// Create the bulb's row if needed, move it to the right room section,
    /// and sync all affected widgets.
    fn place_and_refresh(self: &Rc<Self>, id: &str) {
        let Some(snapshot) = self.merged.borrow().get(id).cloned() else {
            return;
        };
        let room = self.room_name(id, &snapshot);

        self.rows
            .borrow_mut()
            .entry(id.to_string())
            .or_insert_with(|| BulbRow::new(id.to_string(), self.clone()));

        let title_changed = self.rows.borrow().get(id).map(|r| r.row.title().to_string())
            != Some(glib::markup_escape_text(&snapshot.state.label).to_string());

        let prev_room = self.row_room.borrow().get(id).cloned();
        let moved = prev_room.as_deref() != Some(room.as_str());
        if moved {
            let row_widget = self.rows.borrow().get(id).unwrap().row.clone();
            self.row_room
                .borrow_mut()
                .insert(id.to_string(), room.clone());
            if let Some(prev) = prev_room {
                self.remove_from_section(&prev, &row_widget);
            }
            let list = self.ensure_section(&room);
            list.append(&row_widget);
        }

        if let Some(row) = self.rows.borrow().get(id) {
            row.apply(&snapshot, &room);
        }
        if moved || title_changed {
            if let Some(section) = self.sections.borrow().get(&room) {
                section.list.invalidate_sort();
            }
        }
        self.refresh_headers();
        self.stack.set_visible_child_name("list");
    }

    fn remove_from_section(&self, room: &str, row_widget: &adw::ExpanderRow) {
        let mut sections = self.sections.borrow_mut();
        if let Some(section) = sections.get(room) {
            section.list.remove(row_widget);
            let empty = !self.row_room.borrow().values().any(|r| r == room);
            if empty {
                self.rooms_box.remove(&section.root);
                sections.remove(room);
            }
        }
    }

    /// Collapse or expand a room card, with animation.
    fn toggle_room(&self, room: &str) {
        let expand = {
            let mut cfg = self.config.borrow_mut();
            let was_collapsed = cfg.collapsed_rooms.iter().any(|r| r == room);
            if was_collapsed {
                cfg.collapsed_rooms.retain(|r| r != room);
            } else {
                cfg.collapsed_rooms.push(room.to_string());
            }
            cfg.save();
            was_collapsed
        };
        if let Some(section) = self.sections.borrow().get(room) {
            apply_expansion(section, expand);
        }
    }

    /// Get the room's card list, creating the section (sorted into place,
    /// with "Other" always last) if it doesn't exist yet.
    fn ensure_section(self: &Rc<Self>, room: &str) -> gtk::ListBox {
        if let Some(section) = self.sections.borrow().get(room) {
            return section.list.clone();
        }
        let section = new_room_section(room, self);
        let key = room_sort_key(room);
        let prev: Option<gtk::Widget> = {
            let sections = self.sections.borrow();
            let mut before: Vec<&String> = sections
                .keys()
                .filter(|name| room_sort_key(name) < key)
                .collect();
            before.sort_by_key(|name| room_sort_key(name));
            before
                .last()
                .map(|name| sections[*name].root.clone().upcast())
        };
        self.rooms_box.insert_child_after(&section.root, prev.as_ref());
        let list = section.list.clone();
        self.sections.borrow_mut().insert(room.to_string(), section);
        list
    }

    /// Recompute the whole-house and per-room switches and sliders.
    fn refresh_headers(&self) {
        let merged = self.merged.borrow();
        let row_room = self.row_room.borrow();

        // The distinct colors of the lit bulbs in scope (sorted by bulb id
        // for stability, deduped, capped); empty means everything is off
        // and the chip renders gray.
        // Plugs count toward the switch and the device tally but are left
        // out of the brightness average and the color swatches.
        let summarize = |ids: &mut dyn Iterator<Item = &String>| {
            let mut any_on = false;
            let mut sum = 0.0;
            let mut count = 0usize;
            let mut bulbs = 0usize;
            let mut lit: Vec<(&String, (f64, f64, f64))> = Vec::new();
            for id in ids {
                let Some(m) = merged.get(id) else { continue };
                any_on |= m.state.powered;
                count += 1;
                if m.state.kind == DeviceKind::Plug {
                    continue;
                }
                sum += m.state.color.brightness as f64 / 65535.0 * 100.0;
                bulbs += 1;
                if m.state.powered {
                    lit.push((id, visible_rgb(&m.state.color)));
                }
            }
            lit.sort_by_key(|(id, _)| id.as_str());
            let mut colors: Vec<(f64, f64, f64)> = Vec::new();
            let mut seen: Vec<(i32, i32, i32)> = Vec::new();
            for (_, rgb) in lit {
                let key = (
                    (rgb.0 * 24.0).round() as i32,
                    (rgb.1 * 24.0).round() as i32,
                    (rgb.2 * 24.0).round() as i32,
                );
                if !seen.contains(&key) && colors.len() < 8 {
                    seen.push(key);
                    colors.push(rgb);
                }
            }
            (any_on, sum, count, bulbs, colors)
        };

        let (any_on, sum, count, bulbs, colors) = summarize(&mut merged.keys());
        if count > 0 {
            if let Some(h) = self.h_house_switch.get() {
                self.house_switch.block_signal(h);
                self.house_switch.set_active(any_on);
                self.house_switch.unblock_signal(h);
            }
            if let Some(h) = self.h_house_scale.get() {
                if bulbs > 0 {
                    self.house_scale.block_signal(h);
                    self.house_scale.set_value(sum / bulbs as f64);
                    self.house_scale.unblock_signal(h);
                }
            }
            // Brightness and color make no sense when only plugs are around.
            self.house_scale.set_visible(bulbs > 0);
            self.house_spin.set_visible(bulbs > 0);
            self.house_color_btn.set_visible(bulbs > 0);
            *self.house_dot_color.borrow_mut() = colors;
            self.house_dot.queue_draw();
        }

        for (name, section) in self.sections.borrow().iter() {
            let (any_on, sum, count, bulbs, colors) = summarize(
                &mut row_room
                    .iter()
                    .filter(|(_, room)| *room == name)
                    .map(|(id, _)| id),
            );
            if count > 0 {
                section.switch.block_signal(&section.h_switch);
                section.switch.set_active(any_on);
                section.switch.unblock_signal(&section.h_switch);
                if bulbs > 0 {
                    section.scale.block_signal(&section.h_scale);
                    section.scale.set_value(sum / bulbs as f64);
                    section.scale.unblock_signal(&section.h_scale);
                }
                // A room holding only plugs keeps just its power switch.
                section.scale.set_visible(bulbs > 0);
                section.spin.set_visible(bulbs > 0);
                section.color_btn.set_visible(bulbs > 0);
                *section.dot_color.borrow_mut() = colors;
                section.dot.queue_draw();
                let noun = if bulbs == count { "light" } else { "device" };
                section.header.set_subtitle(&if count == 1 {
                    format!("1 {noun}")
                } else {
                    format!("{count} {noun}s")
                });
            }
        }
    }

    fn route_power(&self, m: &Merged, on: bool) {
        if m.state.backend == Backend::Tuya {
            let _ = self.tuya_tx.send(TuyaCommand::SetPower {
                id: m.state.id.clone(),
                on,
            });
            return;
        }
        if m.has_lan && m.lan_connected {
            if let Some(target) = m.lan_target {
                let _ = self.lan_tx.send(LanCommand::SetPower { target, on });
                return;
            }
        }
        if m.has_cloud {
            let _ = self.cloud_tx.send(CloudCommand::SetPower {
                id: m.state.id.clone(),
                on,
            });
        } else if let Some(target) = m.lan_target {
            let _ = self.lan_tx.send(LanCommand::SetPower { target, on });
        }
    }

    fn route_color(&self, m: &Merged, color: Hsbk, duration_ms: u32) {
        if m.state.kind == DeviceKind::Plug {
            return;
        }
        if m.has_lan && m.lan_connected {
            if let Some(target) = m.lan_target {
                let _ = self.lan_tx.send(LanCommand::SetColor {
                    target,
                    color,
                    duration_ms,
                });
                return;
            }
        }
        if m.has_cloud {
            let _ = self.cloud_tx.send(CloudCommand::SetColor {
                id: m.state.id.clone(),
                color,
                duration_ms,
            });
        } else if let Some(target) = m.lan_target {
            let _ = self.lan_tx.send(LanCommand::SetColor {
                target,
                color,
                duration_ms,
            });
        }
    }

    fn refresh_row(&self, id: &str, snapshot: &Merged) {
        let room = self.row_room.borrow().get(id).cloned();
        if let (Some(row), Some(room)) = (self.rows.borrow().get(id), room) {
            row.apply(snapshot, &room);
        }
    }

    fn apply_power(&self, id: &str, on: bool) {
        let snapshot = {
            let mut merged = self.merged.borrow_mut();
            let Some(m) = merged.get_mut(id) else { return };
            m.state.powered = on;
            m.clone()
        };
        self.route_power(&snapshot, on);
        self.refresh_row(id, &snapshot);
    }

    fn apply_adjust(&self, id: &str, f: impl FnOnce(&mut Hsbk), duration_ms: u32) {
        let snapshot = {
            let mut merged = self.merged.borrow_mut();
            let Some(m) = merged.get_mut(id) else { return };
            // Plugs have no color or brightness to adjust.
            if m.state.kind == DeviceKind::Plug {
                return;
            }
            f(&mut m.state.color);
            m.clone()
        };
        self.route_color(&snapshot, snapshot.state.color, duration_ms);
        self.refresh_row(id, &snapshot);
    }

    fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Toggle one bulb (from its row's switch).
    pub fn set_power(&self, id: &str, on: bool) {
        self.apply_power(id, on);
        self.refresh_headers();
    }

    /// Mutate one bulb's color with `f` and push the result to the bulb.
    pub fn adjust(&self, id: &str, f: impl FnOnce(&mut Hsbk), duration_ms: u32) {
        self.apply_adjust(id, f, duration_ms);
        self.refresh_headers();
    }

    fn ids_in_room(&self, room: Option<&str>) -> Vec<String> {
        self.row_room
            .borrow()
            .iter()
            .filter(|(_, r)| room.is_none_or(|want| want == r.as_str()))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Toggle every bulb in a room, or the whole house when `room` is None.
    pub fn room_set_power(&self, room: Option<&str>, on: bool) {
        for id in self.ids_in_room(room) {
            self.apply_power(&id, on);
        }
        self.refresh_headers();
    }

    /// Set brightness (percent) for every bulb in a room / the whole house.
    pub fn room_set_brightness(&self, room: Option<&str>, percent: f64) {
        let value = ((percent / 100.0) * 65535.0).round() as u16;
        for id in self.ids_in_room(room) {
            self.apply_adjust(&id, |c| c.brightness = value, 200);
        }
        self.refresh_headers();
    }

    /// Set the color (hue/saturation) of every bulb in a room / the whole
    /// house, preserving each bulb's own brightness.
    pub fn room_set_color(&self, room: Option<&str>, hue: u16, saturation: u16) {
        for id in self.ids_in_room(room) {
            self.apply_adjust(
                &id,
                |c| {
                    c.hue = hue;
                    c.saturation = saturation;
                },
                250,
            );
        }
        self.refresh_headers();
    }

    /// Switch every bulb in a room / the whole house to white at the given
    /// color temperature.
    pub fn room_set_kelvin(&self, room: Option<&str>, kelvin: u16) {
        for id in self.ids_in_room(room) {
            self.apply_adjust(
                &id,
                |c| {
                    c.kelvin = kelvin;
                    c.saturation = 0;
                },
                250,
            );
        }
        self.refresh_headers();
    }

    /// Move a bulb to a (possibly new) room; empty clears the override.
    pub fn assign_room(self: &Rc<Self>, id: &str, room: &str) {
        {
            let mut cfg = self.config.borrow_mut();
            if room.is_empty() {
                cfg.rooms.remove(id);
            } else {
                cfg.rooms.insert(id.to_string(), room.to_string());
            }
            cfg.save();
        }
        self.place_and_refresh(id);
    }

    /// Save a scene capturing only the bulbs listed in `ids`.
    fn save_scene(self: &Rc<Self>, name: &str, ids: &[String]) {
        let bulbs: Vec<SceneBulb> = self
            .merged
            .borrow()
            .values()
            .filter(|m| ids.contains(&m.state.id))
            .map(|m| SceneBulb {
                id: m.state.id.clone(),
                powered: m.state.powered,
                hue: m.state.color.hue,
                saturation: m.state.color.saturation,
                brightness: m.state.color.brightness,
                kelvin: m.state.color.kelvin,
            })
            .collect();
        if bulbs.is_empty() {
            return;
        }
        {
            let mut cfg = self.config.borrow_mut();
            cfg.scenes.retain(|s| s.name != name);
            cfg.scenes.push(Scene {
                name: name.to_string(),
                bulbs,
            });
            cfg.scenes.sort_by_key(|s| s.name.to_lowercase());
            cfg.save();
        }
        self.rebuild_scenes();
    }

    fn activate_scene(&self, name: &str) {
        let Some(scene) = self
            .config
            .borrow()
            .scenes
            .iter()
            .find(|s| s.name == name)
            .cloned()
        else {
            return;
        };
        for bulb in &scene.bulbs {
            let snapshot = {
                let mut merged = self.merged.borrow_mut();
                let Some(m) = merged.get_mut(&bulb.id) else {
                    continue;
                };
                m.state.color = Hsbk {
                    hue: bulb.hue,
                    saturation: bulb.saturation,
                    brightness: bulb.brightness,
                    kelvin: bulb.kelvin,
                };
                m.state.powered = bulb.powered;
                m.clone()
            };
            self.route_color(&snapshot, snapshot.state.color, 400);
            self.route_power(&snapshot, bulb.powered);
            self.refresh_row(&bulb.id, &snapshot);
        }
        self.refresh_headers();
    }

    fn rename_scene(self: &Rc<Self>, old: &str, new: &str) {
        {
            let mut cfg = self.config.borrow_mut();
            let Some(scene) = cfg.scenes.iter_mut().find(|s| s.name == old) else {
                return;
            };
            scene.name = new.to_string();
            cfg.scenes.sort_by_key(|s| s.name.to_lowercase());
            cfg.save();
        }
        self.rebuild_scenes();
    }

    fn delete_scene(self: &Rc<Self>, name: &str) {
        {
            let mut cfg = self.config.borrow_mut();
            cfg.scenes.retain(|s| s.name != name);
            cfg.save();
        }
        self.rebuild_scenes();
    }

    fn rebuild_scenes(self: &Rc<Self>) {
        for row in self.scene_rows.borrow_mut().drain(..) {
            self.scenes_group.remove(&row);
        }
        let scenes = self.config.borrow().scenes.clone();
        for scene in &scenes {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&scene.name))
                .activatable(true)
                .build();
            // Chip previewing the scene's distinct lit colors.
            let mut colors: Vec<(f64, f64, f64)> = Vec::new();
            let mut seen: Vec<(i32, i32, i32)> = Vec::new();
            for bulb in scene.bulbs.iter().filter(|b| b.powered) {
                let rgb = visible_rgb(&Hsbk {
                    hue: bulb.hue,
                    saturation: bulb.saturation,
                    brightness: bulb.brightness,
                    kelvin: bulb.kelvin,
                });
                let key = (
                    (rgb.0 * 24.0).round() as i32,
                    (rgb.1 * 24.0).round() as i32,
                    (rgb.2 * 24.0).round() as i32,
                );
                if !seen.contains(&key) && colors.len() < 8 {
                    seen.push(key);
                    colors.push(rgb);
                }
            }
            row.add_prefix(&scene_chip(45, &colors));
            row.connect_activated({
                let ui = self.clone();
                let name = scene.name.clone();
                move |_| ui.activate_scene(&name)
            });
            let update = gtk::Button::builder()
                .icon_name("update-symbolic")
                .tooltip_text("Update scene from current lights")
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            update.connect_clicked({
                let ui = self.clone();
                let name = scene.name.clone();
                move |_| show_update_scene_dialog(&ui, &name)
            });
            row.add_suffix(&update);
            let rename = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Rename scene")
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            rename.connect_clicked({
                let ui = self.clone();
                let name = scene.name.clone();
                move |_| show_rename_scene_dialog(&ui, &name)
            });
            row.add_suffix(&rename);
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Delete scene")
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            delete.connect_clicked({
                let ui = self.clone();
                let name = scene.name.clone();
                move |_| ui.delete_scene(&name)
            });
            row.add_suffix(&delete);
            self.scenes_group.add(&row);
            self.scene_rows.borrow_mut().push(row);
        }
    }

    /// A network scan identified a configured Tuya device: store its
    /// address (and confirmed protocol version) and reconnect.
    fn tuya_found(self: &Rc<Self>, id: &str, host: &str, version: &str) {
        let name = {
            let mut cfg = self.config.borrow_mut();
            let Some(dev) = cfg.tuya_devices.iter_mut().find(|d| d.id.trim() == id) else {
                return;
            };
            dev.host = host.to_string();
            // The scan proved this version works, so pin it.
            dev.version = version.to_string();
            let name = if dev.name.trim().is_empty() {
                host.to_string()
            } else {
                dev.name.clone()
            };
            cfg.save();
            name
        };
        let devices = self.config.borrow().tuya_devices.clone();
        let _ = self.tuya_tx.send(TuyaCommand::Configure(devices));
        self.toast(&format!("Found “{name}” at {host}"));
    }

    /// Drop Tuya devices no longer present in the configuration.
    fn purge_tuya(&self) {
        let keep: Vec<String> = self
            .config
            .borrow()
            .tuya_devices
            .iter()
            .filter(|d| d.is_complete())
            .map(|d| format!("tuya:{}", d.id.trim()))
            .collect();
        let stale: Vec<String> = self
            .merged
            .borrow()
            .keys()
            .filter(|id| id.starts_with("tuya:") && !keep.contains(id))
            .cloned()
            .collect();
        for id in &stale {
            self.merged.borrow_mut().remove(id);
            let row = self.rows.borrow_mut().remove(id);
            let room = self.row_room.borrow_mut().remove(id);
            if let (Some(row), Some(room)) = (row, room) {
                self.remove_from_section(&room, &row.row);
            }
        }
        if !stale.is_empty() {
            self.refresh_headers();
            if self.rows.borrow().is_empty() {
                self.stack.set_visible_child_name("empty");
            }
        }
    }

    /// Drop bulbs that are only known through the cloud (called when the
    /// user disables cloud control).
    fn purge_cloud(&self) {
        let cloud_only: Vec<String> = self
            .merged
            .borrow()
            .iter()
            .filter(|(_, m)| !m.has_lan)
            .map(|(id, _)| id.clone())
            .collect();
        {
            let mut merged = self.merged.borrow_mut();
            for m in merged.values_mut() {
                m.has_cloud = false;
            }
            for id in &cloud_only {
                merged.remove(id);
            }
        }
        for id in &cloud_only {
            let row = self.rows.borrow_mut().remove(id);
            let room = self.row_room.borrow_mut().remove(id);
            if let (Some(row), Some(room)) = (row, room) {
                self.remove_from_section(&room, &row.row);
            }
        }
        self.refresh_headers();
        if self.rows.borrow().is_empty() {
            self.stack.set_visible_child_name("empty");
        }
        self.banner.set_revealed(false);
    }
}

/// A popover that recolors a whole room, or the whole house when `room` is
/// None. A Colors/Whites toggle switches between the color wheel (plus hex
/// entry) and a warmth slider. `brightness_adj` is the header brightness
/// slider's adjustment: the popover embeds a second slider on the same
/// adjustment so the two always match and share one set of handlers.
fn color_popover(
    ui: &Rc<Ui>,
    room: Option<String>,
    brightness_adj: gtk::Adjustment,
) -> gtk::Popover {
    let wheel_throttle = Throttler::new(100);
    let wheel = ColorWheel::new({
        let ui = ui.clone();
        let room = room.clone();
        move |hue, sat| {
            let ui = ui.clone();
            let room = room.clone();
            wheel_throttle.run(move || {
                ui.room_set_color(
                    room.as_deref(),
                    ((hue / 360.0) * 65535.0).round() as u16,
                    (sat.clamp(0.0, 1.0) * 65535.0).round() as u16,
                );
            });
        }
    });

    let hex_entry = gtk::Entry::builder()
        .placeholder_text("#RRGGBB")
        .max_length(7)
        .width_chars(10)
        .halign(gtk::Align::Center)
        .build();
    hex_entry.connect_changed(|entry| {
        entry.remove_css_class("error");
    });
    hex_entry.connect_activate({
        let ui = ui.clone();
        let room = room.clone();
        move |entry| match util::parse_hex(&entry.text()) {
            Some((r, g, b)) => {
                entry.remove_css_class("error");
                let (h, s, _) = util::rgb_to_hsv(r, g, b);
                ui.room_set_color(
                    room.as_deref(),
                    ((h / 360.0) * 65535.0).round() as u16,
                    (s.clamp(0.0, 1.0) * 65535.0).round() as u16,
                );
            }
            None => entry.add_css_class("error"),
        }
    });

    let colors_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    colors_box.append(&wheel.widget);
    colors_box.append(&hex_entry);

    // Warmth slider + typed kelvin entry on one shared adjustment, with a
    // live LIFX shade name underneath.
    let kelvin_adj = gtk::Adjustment::new(3500.0, 1500.0, 9000.0, 100.0, 500.0, 0.0);
    let kelvin = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&kelvin_adj)
        .hexpand(true)
        .build();
    disable_scroll(&kelvin);
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
    let kelvin_throttle = Throttler::new(100);
    kelvin.connect_value_changed({
        let ui = ui.clone();
        let room = room.clone();
        move |scale| {
            let value = scale.value();
            let ui = ui.clone();
            let room = room.clone();
            kelvin_throttle.run(move || {
                ui.room_set_kelvin(room.as_deref(), value.round() as u16);
            });
        }
    });
    let kelvin_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    kelvin_box.append(&kelvin);
    kelvin_box.append(&kelvin_spin);
    let kelvin_label = gtk::Label::builder()
        .label(lifx_core::describe_kelvin(3500))
        .halign(gtk::Align::Center)
        .css_classes(["dim-label"])
        .build();
    kelvin_adj.connect_value_changed({
        let kelvin_label = kelvin_label.clone();
        move |adj| {
            kelvin_label.set_label(lifx_core::describe_kelvin(adj.value().round() as u16));
        }
    });
    let whites_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    whites_box.append(&kelvin_box);
    whites_box.append(&kelvin_label);

    // Colors / Whites view toggle (does not change the lights by itself).
    let colors_btn = gtk::ToggleButton::builder().label("Colors").active(true).build();
    let whites_btn = gtk::ToggleButton::builder().label("Whites").build();
    whites_btn.set_group(Some(&colors_btn));
    let mode_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::Center)
        .css_classes(["linked"])
        .build();
    mode_box.append(&colors_btn);
    mode_box.append(&whites_btn);
    colors_btn.connect_toggled({
        let colors_box = colors_box.clone();
        let whites_box = whites_box.clone();
        move |btn| {
            colors_box.set_visible(btn.is_active());
            whites_box.set_visible(!btn.is_active());
        }
    });

    // Brightness, mirroring the header slider via the shared adjustment.
    let brightness = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&brightness_adj)
        .hexpand(true)
        .build();
    disable_scroll(&brightness);
    let brightness_label = gtk::Label::builder()
        .label("Brightness")
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption-heading"])
        .build();
    let brightness_hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    brightness_hbox.append(&brightness);
    brightness_hbox.append(&percent_spin(&brightness_adj));
    let brightness_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .build();
    brightness_box.append(&brightness_label);
    brightness_box.append(&brightness_hbox);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        // Keep the popover the same width in both views: without this it
        // collapses in Whites mode, since bare sliders have almost no
        // natural width once the color wheel is hidden.
        .width_request(232)
        .build();
    content.append(&mode_box);
    content.append(&colors_box);
    content.append(&whites_box);
    content.append(&brightness_box);

    gtk::Popover::builder().child(&content).build()
}

fn new_room_section(room: &str, ui: &Rc<Ui>) -> RoomSection {
    let scale = compact_scale();
    let switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .tooltip_text("All lights in this room")
        .build();
    let (dot, dot_color) = color_dot(45);
    let color_btn = gtk::MenuButton::builder()
        .child(&dot)
        .tooltip_text("Room color")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .popover(&color_popover(ui, Some(room.to_string()), scale.adjustment()))
        .build();

    // Identical widget and layout to the All Lights card. Clicking the row
    // slides the room's bulb rows open/closed inside the same card.
    let header = adw::ActionRow::builder()
        .title(glib::markup_escape_text(room))
        .activatable(true)
        .tooltip_text("Show or hide this room's lights")
        .build();
    let spin = percent_spin(&scale.adjustment());
    header.add_suffix(&color_btn);
    header.add_suffix(&scale);
    header.add_suffix(&spin);
    header.add_suffix(&switch);
    header.connect_activated({
        let ui = ui.clone();
        let room = room.to_string();
        move |_| ui.toggle_room(&room)
    });

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["room-lights"])
        .build();
    list.set_sort_func(list_sort_by_title);

    let expanded = !ui.config.borrow().collapsed_rooms.iter().any(|r| r == room);
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(250)
        .reveal_child(expanded)
        .child(&list)
        .build();
    let lights_row = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .focusable(false)
        .child(&revealer)
        .visible(expanded)
        .build();
    // Fully hide the row once the slide-up animation ends, so the card
    // shows no leftover separator under the header.
    revealer.connect_child_revealed_notify({
        let lights_row = lights_row.clone();
        move |revealer| {
            if !revealer.is_child_revealed() && !revealer.reveals_child() {
                lights_row.set_visible(false);
            }
        }
    });

    let root = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    root.append(&header);
    root.append(&lights_row);

    let h_switch = switch.connect_active_notify({
        let ui = ui.clone();
        let room = room.to_string();
        move |sw| ui.room_set_power(Some(&room), sw.is_active())
    });
    let throttle = Throttler::new(120);
    let h_scale = scale.connect_value_changed({
        let ui = ui.clone();
        let room = room.to_string();
        move |scale| {
            let value = scale.value();
            let ui = ui.clone();
            let room = room.clone();
            throttle.run(move || ui.room_set_brightness(Some(&room), value));
        }
    });

    RoomSection {
        root,
        header,
        lights_row,
        revealer,
        list,
        switch,
        h_switch,
        scale,
        h_scale,
        spin,
        color_btn,
        dot,
        dot_color,
    }
}

fn scenes_json_filters() -> gio::ListStore {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Luxel scenes (JSON)"));
    filter.add_pattern("*.json");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    filters
}

fn export_scenes_dialog(ui: &Rc<Ui>) {
    let count = ui.config.borrow().scenes.len();
    if count == 0 {
        ui.toast("No scenes to export yet");
        return;
    }
    let dialog = gtk::FileDialog::builder()
        .title("Export Scenes")
        .initial_name("luxel-scenes.json")
        .filters(&scenes_json_filters())
        .build();
    let ui = ui.clone();
    dialog.save(Some(&ui.window.clone()), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return }; // dismissed
        let Some(path) = file.path() else { return };
        let data = ScenesFile {
            app: "luxel".to_string(),
            version: 1,
            scenes: ui.config.borrow().scenes.clone(),
        };
        let written = serde_json::to_string_pretty(&data)
            .map_err(|e| e.to_string())
            .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
        match written {
            Ok(()) => ui.toast(&if count == 1 {
                "Exported 1 scene".to_string()
            } else {
                format!("Exported {count} scenes")
            }),
            Err(e) => ui.toast(&format!("Export failed: {e}")),
        }
    });
}

fn import_scenes_dialog(ui: &Rc<Ui>) {
    let dialog = gtk::FileDialog::builder()
        .title("Import Scenes")
        .filters(&scenes_json_filters())
        .build();
    let ui = ui.clone();
    dialog.open(Some(&ui.window.clone()), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return }; // dismissed
        let Some(path) = file.path() else { return };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                ui.toast(&format!("Import failed: {e}"));
                return;
            }
        };
        // Accept the wrapped format or a bare scene array.
        let imported: Vec<Scene> = match serde_json::from_str::<ScenesFile>(&text)
            .map(|f| f.scenes)
            .or_else(|_| serde_json::from_str::<Vec<Scene>>(&text))
        {
            Ok(scenes) => scenes,
            Err(_) => {
                ui.toast("Import failed: not a Luxel scenes file");
                return;
            }
        };
        if imported.is_empty() {
            ui.toast("The file contains no scenes");
            return;
        }
        let count = imported.len();
        {
            let mut cfg = ui.config.borrow_mut();
            for scene in imported {
                // Same-name scenes are replaced, so re-importing is safe.
                cfg.scenes.retain(|s| s.name != scene.name);
                cfg.scenes.push(scene);
            }
            cfg.scenes.sort_by_key(|s| s.name.to_lowercase());
            cfg.save();
        }
        ui.rebuild_scenes();
        ui.toast(&if count == 1 {
            "Imported 1 scene".to_string()
        } else {
            format!("Imported {count} scenes")
        });
    });
}

fn show_rename_scene_dialog(ui: &Rc<Ui>, name: &str) {
    let entry = gtk::Entry::builder()
        .text(name)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading("Rename Scene")
        .extra_child(&entry)
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("rename", "_Rename")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    dialog.set_close_response("cancel");

    // Disable Rename while the name is empty or already taken by another scene.
    entry.connect_changed({
        let ui = ui.clone();
        let dialog = dialog.clone();
        let old = name.to_string();
        move |entry| {
            let text = entry.text().trim().to_string();
            let valid = !text.is_empty()
                && (text == old
                    || !ui.config.borrow().scenes.iter().any(|s| s.name == text));
            dialog.set_response_enabled("rename", valid);
        }
    });

    dialog.connect_response(None, {
        let ui = ui.clone();
        let entry = entry.clone();
        let old = name.to_string();
        move |_, response| {
            if response == "rename" {
                let new = entry.text().trim().to_string();
                if !new.is_empty() && new != old {
                    ui.rename_scene(&old, &new);
                }
            }
        }
    });
    dialog.present(Some(&ui.window));
    entry.grab_focus();
}

/// Build the "Lights to include" checklist (grouped by room). Returns the
/// widget and the per-bulb check buttons; `is_checked` sets initial state.
fn lights_checklist(
    ui: &Rc<Ui>,
    is_checked: impl Fn(&str) -> bool,
) -> (gtk::Box, Vec<(String, gtk::CheckButton)>) {
    let mut items: Vec<(String, String, String)> = {
        let merged = ui.merged.borrow();
        let row_room = ui.row_room.borrow();
        merged
            .values()
            .map(|m| {
                let room = row_room
                    .get(&m.state.id)
                    .cloned()
                    .unwrap_or_else(|| UNGROUPED.to_string());
                (m.state.id.clone(), m.state.label.clone(), room)
            })
            .collect()
    };
    items.sort_by_key(|(_, label, room)| (room.to_lowercase(), label.to_lowercase()));

    let checks_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let mut checks: Vec<(String, gtk::CheckButton)> = Vec::new();
    let mut last_room: Option<&str> = None;
    for (id, label, room) in &items {
        if last_room != Some(room.as_str()) {
            let header = gtk::Label::builder()
                .label(room)
                .halign(gtk::Align::Start)
                .css_classes(["dim-label", "caption-heading"])
                .build();
            if last_room.is_some() {
                header.set_margin_top(6);
            }
            checks_box.append(&header);
            last_room = Some(room.as_str());
        }
        let check = gtk::CheckButton::builder()
            .label(label)
            .active(is_checked(id))
            .build();
        checks_box.append(&check);
        checks.push((id.clone(), check));
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(260)
        .propagate_natural_height(true)
        .child(&checks_box)
        .build();
    let lights_label = gtk::Label::builder()
        .label("Lights to include")
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    content.append(&lights_label);
    content.append(&scroller);
    (content, checks)
}

fn show_save_scene_dialog(ui: &Rc<Ui>) {
    let entry = gtk::Entry::builder()
        .placeholder_text("e.g. Movie Night")
        .activates_default(true)
        .build();
    let (checklist, checks) = lights_checklist(ui, |_| true);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    content.append(&entry);
    content.append(&checklist);

    let dialog = adw::AlertDialog::builder()
        .heading("Save Scene")
        .body("Save the current state of the selected lights as a scene.")
        .extra_child(&content)
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("save", "_Save")]);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, {
        let ui = ui.clone();
        let entry = entry.clone();
        move |_, response| {
            if response == "save" {
                let name = entry.text().trim().to_string();
                let ids: Vec<String> = checks
                    .iter()
                    .filter(|(_, check)| check.is_active())
                    .map(|(id, _)| id.clone())
                    .collect();
                if !name.is_empty() && !ids.is_empty() {
                    ui.save_scene(&name, &ids);
                }
            }
        }
    });
    dialog.present(Some(&ui.window));
}

fn show_update_scene_dialog(ui: &Rc<Ui>, name: &str) {
    // Pre-check exactly the lights the scene currently contains.
    let members: Vec<String> = ui
        .config
        .borrow()
        .scenes
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.bulbs.iter().map(|b| b.id.clone()).collect())
        .unwrap_or_default();
    let (checklist, checks) = lights_checklist(ui, |id| members.iter().any(|m| m == id));

    let dialog = adw::AlertDialog::builder()
        .heading("Update Scene")
        .body(format!(
            "Capture the current state of the selected lights into “{name}”."
        ))
        .extra_child(&checklist)
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("update", "_Update")]);
    dialog.set_response_appearance("update", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("update"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, {
        let ui = ui.clone();
        let name = name.to_string();
        move |_, response| {
            if response == "update" {
                let ids: Vec<String> = checks
                    .iter()
                    .filter(|(_, check)| check.is_active())
                    .map(|(id, _)| id.clone())
                    .collect();
                if !ids.is_empty() {
                    ui.save_scene(&name, &ids);
                }
            }
        }
    });
    dialog.present(Some(&ui.window));
}

fn show_preferences(ui: &Rc<Ui>) {
    let lan_group = adw::PreferencesGroup::builder()
        .title("Local Network")
        .description(
            "Bulbs on a different subnet or VLAN — such as an isolated IoT \
             network — cannot hear broadcast discovery. List their subnets \
             here (CIDR form, comma-separated) and they will be probed \
             directly. Example: 192.168.20.0/24",
        )
        .build();
    let subnet_row = adw::EntryRow::builder().title("Subnets to scan").build();
    subnet_row.set_text(&ui.config.borrow().lan_subnets.join(", "));
    subnet_row.connect_changed({
        let ui = ui.clone();
        move |row| {
            let text = row.text();
            let mut entries = Vec::new();
            let mut parsed = Vec::new();
            let mut valid = true;
            for part in text
                .split([',', ' '])
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                match Subnet::parse(part) {
                    Some(subnet) => {
                        entries.push(part.to_string());
                        parsed.push(subnet);
                    }
                    None => valid = false,
                }
            }
            if !valid {
                row.add_css_class("error");
                return;
            }
            row.remove_css_class("error");
            {
                let mut cfg = ui.config.borrow_mut();
                cfg.lan_subnets = entries;
                cfg.save();
            }
            let _ = ui.lan_tx.send(LanCommand::SetSubnets(parsed));
        }
    });
    lan_group.add(&subnet_row);

    let group = adw::PreferencesGroup::builder()
        .title("LIFX Cloud")
        .description(
            "Optionally control your lights through your LIFX account — useful \
             when local discovery is blocked or you are on another network. \
             Create a personal access token at cloud.lifx.com/settings.",
        )
        .build();

    let enable_row = adw::SwitchRow::builder()
        .title("Cloud Control")
        .subtitle("Also list and control lights via the LIFX Cloud")
        .build();
    enable_row.set_active(ui.config.borrow().cloud_enabled);

    let token_row = adw::PasswordEntryRow::builder()
        .title("Personal Access Token")
        .build();
    token_row.set_text(&ui.config.borrow().cloud_token);

    enable_row.connect_active_notify({
        let ui = ui.clone();
        move |row| {
            let (token, enabled) = {
                let mut cfg = ui.config.borrow_mut();
                cfg.cloud_enabled = row.is_active();
                cfg.save();
                (cfg.cloud_token.clone(), cfg.cloud_enabled)
            };
            let _ = ui.cloud_tx.send(CloudCommand::Configure { token, enabled });
            if !enabled {
                ui.purge_cloud();
            }
        }
    });
    token_row.connect_changed({
        let ui = ui.clone();
        move |row| {
            let (token, enabled) = {
                let mut cfg = ui.config.borrow_mut();
                cfg.cloud_token = row.text().trim().to_string();
                cfg.save();
                (cfg.cloud_token.clone(), cfg.cloud_enabled)
            };
            let _ = ui.cloud_tx.send(CloudCommand::Configure { token, enabled });
        }
    });

    group.add(&enable_row);
    group.add(&token_row);

    // SmartLife / Tuya: the wizard in its own group, the configured
    // devices (with an add row at the bottom) in a second one.
    let tuya_group = adw::PreferencesGroup::builder()
        .title("SmartLife / Tuya")
        .description(
            "Control Tuya-based smart plugs (SmartLife app) directly over \
             the LAN. Tuya encrypts local control with a per-device secret \
             from the Tuya cloud — the setup wizard walks you through \
             getting the keys and finding the devices on your network.",
        )
        .build();
    let wizard_row = adw::ActionRow::builder()
        .title("Setup Wizard")
        .subtitle("Fetch device keys from the Tuya cloud and find the devices on your network")
        .activatable(true)
        .build();
    wizard_row.add_prefix(&gtk::Image::from_icon_name("preferences-other-symbolic"));
    wizard_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    wizard_row.connect_activated({
        let ui = ui.clone();
        move |_| wizard::show_wizard(&ui)
    });
    tuya_group.add(&wizard_row);

    let tuya_devices_group = adw::PreferencesGroup::builder()
        .title("SmartLife Devices")
        .build();
    let tuya_add_row = adw::ActionRow::builder()
        .title("Add Device Manually")
        .activatable(true)
        .build();
    tuya_add_row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
    tuya_devices_group.add(&tuya_add_row);
    let tuya_rows: Rc<RefCell<Vec<adw::ExpanderRow>>> = Rc::new(RefCell::new(Vec::new()));
    rebuild_tuya_rows(ui, &tuya_devices_group, &tuya_rows, &tuya_add_row);
    tuya_add_row.connect_activated({
        let ui = ui.clone();
        let group = tuya_devices_group.clone();
        let rows = tuya_rows.clone();
        move |row| {
            {
                let mut cfg = ui.config.borrow_mut();
                cfg.tuya_devices.push(TuyaDevice::default());
                cfg.save();
            }
            rebuild_tuya_rows(&ui, &group, &rows, row);
            if let Some(last) = rows.borrow().last() {
                last.set_expanded(true);
            }
        }
    });

    let page = adw::PreferencesPage::new();
    page.add(&lan_group);
    page.add(&tuya_group);
    page.add(&tuya_devices_group);
    page.add(&group);
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Settings");
    dialog.add(&page);
    dialog.present(Some(&ui.window));
}

/// Push the configured Tuya devices to the backend thread and drop any rows
/// for devices that no longer exist.
fn push_tuya_config(ui: &Rc<Ui>) {
    let devices = ui.config.borrow().tuya_devices.clone();
    let _ = ui.tuya_tx.send(TuyaCommand::Configure(devices));
    ui.purge_tuya();
}

type TuyaFieldSetter = Rc<dyn Fn(&mut TuyaDevice, String)>;

/// The live status line for one configured device, from the merged state.
fn tuya_status_text(ui: &Rc<Ui>, index: usize) -> String {
    let Some(dev) = ui.config.borrow().tuya_devices.get(index).cloned() else {
        return String::new();
    };
    tuya_status_for(ui, &dev)
}

fn tuya_status_for(ui: &Rc<Ui>, dev: &TuyaDevice) -> String {
    if !dev.is_complete() {
        let mut missing = Vec::new();
        if dev.host.trim().is_empty() {
            missing.push("IP address");
        }
        if dev.id.trim().is_empty() {
            missing.push("device ID");
        }
        if dev.key.trim().len() != 16 {
            missing.push("local key");
        }
        return format!("Waiting for: {}", missing.join(", "));
    }
    let id = format!("tuya:{}", dev.id.trim());
    match ui.merged.borrow().get(&id) {
        Some(m) if m.state.connected => "Connected".to_string(),
        _ => "Connecting… (check IP, device ID and local key if this persists)".to_string(),
    }
}

/// (Re)build the per-device expander rows of the Tuya devices group,
/// keeping `add_row` as the last row. Fields save (and reach the backend)
/// as they change, and a status row reports the connection state live.
fn rebuild_tuya_rows(
    ui: &Rc<Ui>,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ExpanderRow>>>,
    add_row: &adw::ActionRow,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    group.remove(add_row);
    let devices = ui.config.borrow().tuya_devices.clone();
    for (i, dev) in devices.iter().enumerate() {
        let row = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(&if dev.name.trim().is_empty() {
                "New Device".to_string()
            } else {
                dev.name.clone()
            }))
            .subtitle(glib::markup_escape_text(&dev.host))
            .expanded(!dev.is_complete())
            .build();

        // A field entry that saves cfg.tuya_devices[i] as it changes.
        let entry = |title: &str, text: &str, set: TuyaFieldSetter| {
            let e = adw::EntryRow::builder().title(title).build();
            e.set_text(text);
            e.connect_changed({
                let ui = ui.clone();
                move |e| {
                    e.remove_css_class("error");
                    {
                        let mut cfg = ui.config.borrow_mut();
                        let Some(dev) = cfg.tuya_devices.get_mut(i) else {
                            return;
                        };
                        set(dev, e.text().trim().to_string());
                        cfg.save();
                    }
                    push_tuya_config(&ui);
                }
            });
            e
        };

        let name_row = entry("Name", &dev.name, Rc::new(|d, v| d.name = v));
        name_row.connect_changed({
            let row = row.clone();
            move |e| {
                let text = e.text().trim().to_string();
                row.set_title(&glib::markup_escape_text(&if text.is_empty() {
                    "New Device".to_string()
                } else {
                    text
                }));
            }
        });
        row.add_row(&name_row);

        let host_row = entry("IP address", &dev.host, Rc::new(|d, v| d.host = v));
        host_row.connect_changed({
            let row = row.clone();
            move |e| {
                let text = e.text().trim().to_string();
                if !text.is_empty() && text.parse::<std::net::Ipv4Addr>().is_err() {
                    e.add_css_class("error");
                }
                row.set_subtitle(&glib::markup_escape_text(&text));
            }
        });
        row.add_row(&host_row);

        row.add_row(&entry("Device ID", &dev.id, Rc::new(|d, v| d.id = v)));

        // The local key is a secret, so use a password row (same pattern as
        // the cloud token).
        let key_row = adw::PasswordEntryRow::builder()
            .title("Local key (16 characters)")
            .build();
        key_row.set_text(&dev.key);
        key_row.connect_changed({
            let ui = ui.clone();
            move |e| {
                let text = e.text().trim().to_string();
                if !text.is_empty() && text.len() != 16 {
                    e.add_css_class("error");
                } else {
                    e.remove_css_class("error");
                }
                {
                    let mut cfg = ui.config.borrow_mut();
                    let Some(dev) = cfg.tuya_devices.get_mut(i) else {
                        return;
                    };
                    dev.key = text;
                    cfg.save();
                }
                push_tuya_config(&ui);
            }
        });
        row.add_row(&key_row);

        // Live connection status, refreshed while the dialog is open.
        let status_row = adw::ActionRow::builder()
            .title("Status")
            .subtitle(glib::markup_escape_text(&tuya_status_text(ui, i)))
            .css_classes(["property"])
            .build();
        let seen_rooted = std::cell::Cell::new(false);
        glib::timeout_add_local(Duration::from_millis(700), {
            let ui = ui.clone();
            let status_row = status_row.clone();
            move || {
                let rooted = status_row.root().is_some();
                if seen_rooted.get() && !rooted {
                    // Dialog closed or rows rebuilt: stop this timer.
                    return glib::ControlFlow::Break;
                }
                seen_rooted.set(seen_rooted.get() || rooted);
                status_row.set_subtitle(&glib::markup_escape_text(&tuya_status_text(&ui, i)));
                glib::ControlFlow::Continue
            }
        });
        row.add_row(&status_row);

        let version_row = adw::ComboRow::builder()
            .title("Protocol version")
            .subtitle("Leave on Automatic unless detection fails")
            .model(&gtk::StringList::new(&["Automatic", "3.3", "3.4", "3.5"]))
            .build();
        version_row.set_selected(match dev.version.as_str() {
            "3.3" => 1,
            "3.4" => 2,
            "3.5" => 3,
            _ => 0,
        });
        version_row.connect_selected_notify({
            let ui = ui.clone();
            move |combo| {
                {
                    let mut cfg = ui.config.borrow_mut();
                    let Some(dev) = cfg.tuya_devices.get_mut(i) else {
                        return;
                    };
                    dev.version = match combo.selected() {
                        1 => "3.3",
                        2 => "3.4",
                        3 => "3.5",
                        _ => "auto",
                    }
                    .to_string();
                    cfg.save();
                }
                push_tuya_config(&ui);
            }
        });
        row.add_row(&version_row);

        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove device")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        remove.connect_clicked({
            let ui = ui.clone();
            let group = group.clone();
            let rows = rows.clone();
            let add_row = add_row.clone();
            move |_| {
                {
                    let mut cfg = ui.config.borrow_mut();
                    if i < cfg.tuya_devices.len() {
                        cfg.tuya_devices.remove(i);
                    }
                    cfg.save();
                }
                push_tuya_config(&ui);
                rebuild_tuya_rows(&ui, &group, &rows, &add_row);
            }
        });
        row.add_suffix(&remove);

        group.add(&row);
        rows.borrow_mut().push(row);
    }
    // The add row always comes after the device list.
    group.add(add_row);
}

/// Inject sample bulbs and scenes for screenshots (LUXEL_DEMO).
fn demo_populate(ui: &Rc<Ui>) {
    let deg = |d: f64| ((d / 360.0) * 65535.0).round() as u16;
    let pct = |p: f64| ((p / 100.0) * 65535.0).round() as u16;
    let bulbs: &[(&str, &str, &str, bool, Hsbk)] = &[
        ("d073d5000001", "Ceiling", "Living Room", true,
         Hsbk { hue: 0, saturation: 0, brightness: pct(80.0), kelvin: 2700 }),
        ("d073d5000002", "Floor Lamp", "Living Room", true,
         Hsbk { hue: deg(28.0), saturation: pct(85.0), brightness: pct(65.0), kelvin: 3500 }),
        ("d073d5000003", "TV Backlight", "Living Room", true,
         Hsbk { hue: deg(278.0), saturation: pct(90.0), brightness: pct(55.0), kelvin: 3500 }),
        ("d073d5000004", "Bedside", "Bedroom", true,
         Hsbk { hue: 0, saturation: 0, brightness: pct(35.0), kelvin: 2200 }),
        ("d073d5000005", "Reading Lamp", "Bedroom", false,
         Hsbk { hue: 0, saturation: 0, brightness: pct(60.0), kelvin: 2700 }),
        ("d073d5000006", "Desk", "Office", true,
         Hsbk { hue: 0, saturation: 0, brightness: pct(100.0), kelvin: 5000 }),
        ("d073d5000007", "Shelf", "Office", true,
         Hsbk { hue: deg(190.0), saturation: pct(80.0), brightness: pct(70.0), kelvin: 3500 }),
    ];
    for (id, label, room, powered, color) in bulbs {
        ui.upsert(BulbState {
            id: id.to_string(),
            backend: Backend::Lan,
            kind: DeviceKind::Bulb,
            label: label.to_string(),
            group: Some(room.to_string()),
            powered: *powered,
            color: *color,
            connected: true,
            lan_target: None,
        });
    }
    // A SmartLife/Tuya smart plug, to show the power-only row.
    ui.upsert(BulbState {
        id: "tuya:demo0123456789abcdefg".to_string(),
        backend: Backend::Tuya,
        kind: DeviceKind::Plug,
        label: "Fan".to_string(),
        group: Some("Office".to_string()),
        powered: true,
        color: Hsbk::default(),
        connected: true,
        lan_target: None,
    });
    {
        let mut cfg = ui.config.borrow_mut();
        let scene = |name: &str, entries: &[(&str, bool, Hsbk)]| Scene {
            name: name.to_string(),
            bulbs: entries
                .iter()
                .map(|(id, powered, c)| SceneBulb {
                    id: id.to_string(),
                    powered: *powered,
                    hue: c.hue,
                    saturation: c.saturation,
                    brightness: c.brightness,
                    kelvin: c.kelvin,
                })
                .collect(),
        };
        cfg.scenes = vec![
            scene("Movie Night", &[
                ("d073d5000001", false, Hsbk { hue: 0, saturation: 0, brightness: pct(50.0), kelvin: 2700 }),
                ("d073d5000002", true, Hsbk { hue: deg(25.0), saturation: pct(90.0), brightness: pct(25.0), kelvin: 3500 }),
                ("d073d5000003", true, Hsbk { hue: deg(278.0), saturation: pct(95.0), brightness: pct(40.0), kelvin: 3500 }),
            ]),
            scene("Focus", &[
                ("d073d5000006", true, Hsbk { hue: 0, saturation: 0, brightness: pct(100.0), kelvin: 5000 }),
                ("d073d5000007", true, Hsbk { hue: 0, saturation: 0, brightness: pct(90.0), kelvin: 4500 }),
            ]),
            scene("Sunset", &[
                ("d073d5000001", true, Hsbk { hue: deg(18.0), saturation: pct(80.0), brightness: pct(45.0), kelvin: 3500 }),
                ("d073d5000002", true, Hsbk { hue: deg(35.0), saturation: pct(95.0), brightness: pct(50.0), kelvin: 3500 }),
                ("d073d5000003", true, Hsbk { hue: deg(320.0), saturation: pct(75.0), brightness: pct(40.0), kelvin: 3500 }),
            ]),
        ];
    }
    ui.rebuild_scenes();
}

/// A plain shortcuts list (deliberately no search — four shortcuts don't
/// need one, and AdwShortcutsDialog can't hide its search UI).
fn show_shortcuts(ui: &Rc<Ui>) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    for (title, accel) in [
        ("Rescan for Lights", "<primary>r"),
        ("Settings", "<primary>comma"),
        ("Keyboard Shortcuts", "<primary>question"),
        ("Quit", "<primary>q"),
    ] {
        let row = adw::ActionRow::builder().title(title).build();
        let label = gtk::ShortcutLabel::new(accel);
        label.set_valign(gtk::Align::Center);
        row.add_suffix(&label);
        list.append(&row);
    }
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&list));
    adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(360)
        .child(&tv)
        .build()
        .present(Some(&ui.window));
}

fn open_uri(uri: &str) {
    gtk::UriLauncher::new(uri).launch(
        None::<&gtk::Window>,
        gio::Cancellable::NONE,
        |_| {},
    );
}

fn show_about(ui: &Rc<Ui>) {
    let win = adw::Window::builder()
        .transient_for(&ui.window)
        .modal(false)
        .title("About Luxel")
        .default_width(460)
        .default_height(640)
        .build();

    // A navigation stack so Release Notes / Changelog slide in (and back out)
    // within the same window instead of spawning separate ones.
    let nav = adw::NavigationView::new();

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let clamp = adw::Clamp::builder().maximum_size(420).build();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_margin_top(18);
    page.set_margin_bottom(12);

    // Identity block.
    let icon = gtk::Image::from_icon_name(crate::APP_ID);
    icon.set_pixel_size(96);
    icon.set_margin_bottom(10);
    page.append(&icon);

    let name = gtk::Label::new(Some("Luxel"));
    name.add_css_class("title-1");
    page.append(&name);

    let version = gtk::Label::new(Some(env!("CARGO_PKG_VERSION")));
    version.add_css_class("about-version-chip");
    version.set_halign(gtk::Align::Center);
    version.set_margin_top(8);
    page.append(&version);

    let desc = gtk::Label::new(Some(
        "Control LIFX smart bulbs and SmartLife devices from your desktop — \
         locally, no cloud required.",
    ));
    desc.set_wrap(true);
    desc.set_justify(gtk::Justification::Center);
    desc.add_css_class("dim-label");
    desc.set_margin_top(12);
    page.append(&desc);

    // Release notes and changelog slide in as sub-pages of this window.
    let info = gtk::ListBox::new();
    info.add_css_class("boxed-list");
    info.set_selection_mode(gtk::SelectionMode::None);
    info.set_margin_top(20);

    let notes_row = adw::ActionRow::builder()
        .title("Release Notes")
        .subtitle(format!("What's new in {}", env!("CARGO_PKG_VERSION")))
        .activatable(true)
        .build();
    notes_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let nav = nav.clone();
        notes_row.connect_activated(move |_| nav.push_by_tag("notes"));
    }
    info.append(&notes_row);

    let changelog_row = adw::ActionRow::builder()
        .title("Changelog")
        .subtitle("Full version history")
        .activatable(true)
        .build();
    changelog_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let nav = nav.clone();
        changelog_row.connect_activated(move |_| nav.push_by_tag("changelog"));
    }
    info.append(&changelog_row);
    page.append(&info);

    // Project links. Each row shows its URL as a hover tooltip.
    let links_title = gtk::Label::new(Some("Project"));
    links_title.add_css_class("heading");
    links_title.set_halign(gtk::Align::Start);
    links_title.set_margin_top(20);
    links_title.set_margin_bottom(6);
    page.append(&links_title);

    let links = gtk::ListBox::new();
    links.add_css_class("boxed-list");
    links.set_selection_mode(gtk::SelectionMode::None);
    let mk_row = |title: &str, url: &str| -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .build();
        row.set_tooltip_text(Some(url));
        row.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
        let url = url.to_string();
        row.connect_activated(move |_| open_uri(&url));
        row
    };
    links.append(&mk_row("Website", "https://luxel.hyprlab.co"));
    links.append(&mk_row(
        "GitHub — Submit bug report or feature request",
        "https://github.com/hyprlab/luxel/issues",
    ));
    links.append(&mk_row("Contact — hyprlab@proton.me", "mailto:hyprlab@proton.me"));
    links.append(&mk_row("Source Code", "https://github.com/hyprlab/luxel"));
    links.append(&mk_row(
        "License (GNU AGPL v3)",
        "https://www.gnu.org/licenses/agpl-3.0.html",
    ));

    // Buy Me a Coffee — with a coffee-cup glyph as its leading icon.
    let coffee = adw::ActionRow::builder()
        .title("Buy Me a Coffee")
        .activatable(true)
        .build();
    coffee.set_tooltip_text(Some("https://buymeacoffee.com/hyprlab"));
    let cup = gtk::Label::new(Some("☕"));
    cup.add_css_class("about-coffee");
    coffee.add_prefix(&cup);
    coffee.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
    coffee.connect_activated(move |_| open_uri("https://buymeacoffee.com/hyprlab"));
    links.append(&coffee);
    page.append(&links);

    // Footer.
    let footer = gtk::Label::new(Some("© 2026 Hyprlab"));
    footer.add_css_class("dim-label");
    footer.add_css_class("caption");
    footer.set_wrap(true);
    footer.set_justify(gtk::Justification::Center);
    footer.set_margin_top(20);
    page.append(&footer);

    clamp.set_child(Some(&page));
    scroller.set_child(Some(&clamp));

    // The root page holds the identity + links; the sub-pages slide over it.
    let main_tv = adw::ToolbarView::new();
    let main_header = adw::HeaderBar::new();
    main_header.add_css_class("flat");
    main_tv.add_top_bar(&main_header);
    main_tv.set_content(Some(&scroller));
    nav.add(
        &adw::NavigationPage::builder()
            .title("About Luxel")
            .tag("main")
            .child(&main_tv)
            .build(),
    );
    nav.add(&notes_page("Release Notes", "notes", &md_to_pango(include_str!("../../RELEASE_NOTES.md"))));
    nav.add(&notes_page("Changelog", "changelog", &md_to_pango(include_str!("../../CHANGELOG.md"))));

    win.set_content(Some(&nav));
    win.present();
}

/// Minimal Markdown → Pango markup (headings, bullets) for the About sub-pages.
fn md_to_pango(md: &str) -> String {
    let mut out = String::new();
    for raw in md.lines() {
        let line = raw.trim_end();
        let rendered = if let Some(rest) = line.strip_prefix("## ") {
            format!("<b>{}</b>", glib::markup_escape_text(rest))
        } else if let Some(rest) = line.strip_prefix("# ") {
            format!("<big><b>{}</b></big>", glib::markup_escape_text(rest))
        } else if let Some(rest) = line.strip_prefix("- ") {
            format!("•  {}", glib::markup_escape_text(rest))
        } else if line.is_empty() {
            String::new()
        } else {
            glib::markup_escape_text(line).to_string()
        };
        out.push_str(&rendered);
        out.push('\n');
    }
    out
}

/// Build a scrollable About sub-page (Pango `markup`) for the navigation
/// stack, reachable by `tag`. Pushed pages get a back button and slide
/// animation from the parent `NavigationView`.
fn notes_page(title: &str, tag: &str, markup: &str) -> adw::NavigationPage {
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let clamp = adw::Clamp::builder().maximum_size(460).build();

    let label = gtk::Label::new(None);
    label.set_markup(markup);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_yalign(0.0);
    label.set_margin_top(18);
    label.set_margin_bottom(24);
    label.set_margin_start(18);
    label.set_margin_end(18);

    clamp.set_child(Some(&label));
    scroller.set_child(Some(&clamp));

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&scroller));

    adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(&tv)
        .build()
}
