//! Guided setup wizard for SmartLife/Tuya devices: walks through creating a
//! Tuya developer account, linking the SmartLife app, generating the
//! tinytuya `devices.json` file, importing it, and locating the devices on
//! the network.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config::TuyaDevice;
use crate::model::{LanCommand, Subnet, TuyaCommand};
use crate::tuya::cloud::{self, CloudAccount};
use crate::ui::{open_uri, push_tuya_config, tuya_status_for, Ui};

/// Ids of the devices the wizard imported this run, for the finalize page.
type ImportedIds = Rc<RefCell<Vec<String>>>;

pub fn show_wizard(ui: &Rc<Ui>) {
    let win = adw::Window::builder()
        .transient_for(&ui.window)
        .title("SmartLife Setup")
        .default_width(540)
        .default_height(760)
        .build();
    let toasts = adw::ToastOverlay::new();
    let nav = adw::NavigationView::new();
    toasts.set_child(Some(&nav));
    win.set_content(Some(&toasts));

    let imported: ImportedIds = Rc::new(RefCell::new(Vec::new()));

    intro_page(&nav);
    account_page(&nav);
    project_page(&nav);
    link_page(&nav);
    keys_page(&nav);
    fetch_page(ui, &nav, &toasts, &imported);
    generate_page(&nav, &toasts);
    import_page(ui, &nav, &win, &toasts, &imported);
    finalize_page(ui, &nav, &win, &toasts, &imported);

    win.present();
}

/// The device checklist plus "Add Selected Devices" button shared by the
/// cloud-fetch and file-import pages. `populate` fills it with devices.
struct DevicePicker {
    widget: gtk::Box,
    populate: Rc<dyn Fn(Vec<TuyaDevice>)>,
}

fn device_picker(
    ui: &Rc<Ui>,
    nav: &adw::NavigationView,
    toasts: &adw::ToastOverlay,
    imported: &ImportedIds,
) -> DevicePicker {
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 14);
    let list_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    widget.append(&list_box);
    let add_btn = gtk::Button::builder()
        .label("Add Selected Devices")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .visible(false)
        .build();
    widget.append(&add_btn);

    let parsed: Rc<RefCell<Vec<TuyaDevice>>> = Rc::new(RefCell::new(Vec::new()));
    let checks: Rc<RefCell<Vec<gtk::CheckButton>>> = Rc::new(RefCell::new(Vec::new()));

    let populate: Rc<dyn Fn(Vec<TuyaDevice>)> = Rc::new({
        let list_box = list_box.clone();
        let add_btn = add_btn.clone();
        let parsed = parsed.clone();
        let checks = checks.clone();
        move |devices: Vec<TuyaDevice>| {
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }
            checks.borrow_mut().clear();
            for dev in &devices {
                let short_id = if dev.id.len() > 10 {
                    format!("{}…", &dev.id[..10])
                } else {
                    dev.id.clone()
                };
                let detail = if dev.host.is_empty() {
                    short_id
                } else {
                    format!("{short_id} · {}", dev.host)
                };
                let check = gtk::CheckButton::builder()
                    .label(format!(
                        "{} ({detail})",
                        if dev.name.is_empty() { "Unnamed" } else { &dev.name }
                    ))
                    .active(true)
                    .build();
                list_box.append(&check);
                checks.borrow_mut().push(check);
            }
            *parsed.borrow_mut() = devices;
            add_btn.set_visible(true);
        }
    });

    add_btn.connect_clicked({
        let ui = ui.clone();
        let nav = nav.clone();
        let toasts = toasts.clone();
        let imported = imported.clone();
        move |_| {
            let selected: Vec<TuyaDevice> = parsed
                .borrow()
                .iter()
                .zip(checks.borrow().iter())
                .filter(|(_, c)| c.is_active())
                .map(|(d, _)| d.clone())
                .collect();
            if selected.is_empty() {
                toasts.add_toast(adw::Toast::new("No devices selected"));
                return;
            }
            let count = selected.len();
            imported.borrow_mut().clear();
            {
                let mut cfg = ui.config.borrow_mut();
                for dev in selected {
                    imported.borrow_mut().push(dev.id.clone());
                    if let Some(existing) = cfg
                        .tuya_devices
                        .iter_mut()
                        .find(|e| e.id.trim() == dev.id.trim())
                    {
                        // Refresh the key and fill gaps, but keep the
                        // user's own name/address if already set.
                        existing.key = dev.key;
                        if existing.name.trim().is_empty() {
                            existing.name = dev.name;
                        }
                        if existing.host.trim().is_empty() {
                            existing.host = dev.host;
                        }
                        if existing.version == "auto" {
                            existing.version = dev.version;
                        }
                    } else {
                        cfg.tuya_devices.push(dev);
                    }
                }
                cfg.save();
            }
            push_tuya_config(&ui);
            toasts.add_toast(adw::Toast::new(&if count == 1 {
                "Added 1 device".to_string()
            } else {
                format!("Added {count} devices")
            }));
            nav.push_by_tag("finalize");
        }
    });

    DevicePicker { widget, populate }
}

/// Step 5: fetch the device list (with local keys) straight from the Tuya
/// Cloud using the project's API credentials — no terminal needed.
fn fetch_page(
    ui: &Rc<Ui>,
    nav: &adw::NavigationView,
    toasts: &adw::ToastOverlay,
    imported: &ImportedIds,
) {
    let b = page(nav, "fetch", "Step 5 · Fetch Devices", None);
    b.append(&para(
        "Enter the project's API keys and Luxel fetches your devices — \
         including their local keys — directly from the Tuya Cloud. The \
         keys are stored only in Luxel's configuration on this computer.",
    ));

    let id_row = adw::EntryRow::builder().title("Access ID / Client ID").build();
    id_row.set_text(&ui.config.borrow().tuya_api_id);
    let secret_row = adw::PasswordEntryRow::builder()
        .title("Access Secret / Client Secret")
        .build();
    secret_row.set_text(&ui.config.borrow().tuya_api_secret);
    let region_row = adw::ComboRow::builder()
        .title("Data Center")
        .subtitle("Must match the project's data center")
        .model(&gtk::StringList::new(
            &cloud::REGIONS
                .iter()
                .map(|(_, label)| *label)
                .collect::<Vec<_>>(),
        ))
        .build();
    let saved_region = ui.config.borrow().tuya_api_region.clone();
    if let Some(pos) = cloud::REGIONS.iter().position(|(code, _)| *code == saved_region) {
        region_row.set_selected(pos as u32);
    }
    let creds_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    creds_list.append(&id_row);
    creds_list.append(&secret_row);
    creds_list.append(&region_row);
    b.append(&creds_list);

    let fetch_btn = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("folder-download-symbolic")
                .label("Fetch Devices from Tuya")
                .build(),
        )
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    let spinner = gtk::Spinner::builder().halign(gtk::Align::Center).build();
    let error_label = para("");
    error_label.add_css_class("error");
    error_label.set_visible(false);
    b.append(&fetch_btn);
    b.append(&spinner);
    b.append(&error_label);

    let picker = device_picker(ui, nav, toasts, imported);
    b.append(&picker.widget);

    // Fallback for accounts the API fetch can't serve.
    let fallback = gtk::Button::builder()
        .label("Use the tinytuya command-line tool instead…")
        .halign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    fallback.connect_clicked({
        let nav = nav.clone();
        move |_| nav.push_by_tag("generate")
    });
    b.append(&fallback);

    let populate = picker.populate.clone();
    fetch_btn.connect_clicked({
        let ui = ui.clone();
        let toasts = toasts.clone();
        let id_row = id_row.clone();
        let secret_row = secret_row.clone();
        let region_row = region_row.clone();
        let spinner = spinner.clone();
        let error_label = error_label.clone();
        move |btn| {
            let client_id = id_row.text().trim().to_string();
            let secret = secret_row.text().trim().to_string();
            let region = cloud::REGIONS
                .get(region_row.selected() as usize)
                .map(|(code, _)| code.to_string())
                .unwrap_or_else(|| "us".to_string());
            if client_id.is_empty() || secret.is_empty() {
                error_label.set_label("Enter both the Access ID and the Access Secret.");
                error_label.set_visible(true);
                return;
            }
            error_label.set_visible(false);
            {
                let mut cfg = ui.config.borrow_mut();
                cfg.tuya_api_id = client_id.clone();
                cfg.tuya_api_secret = secret.clone();
                cfg.tuya_api_region = region.clone();
                cfg.save();
            }
            btn.set_sensitive(false);
            spinner.start();

            let acct = CloudAccount {
                client_id,
                secret,
                region,
            };
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send_blocking(cloud::fetch_devices(&acct));
            });
            glib::spawn_future_local({
                let btn = btn.clone();
                let spinner = spinner.clone();
                let error_label = error_label.clone();
                let toasts = toasts.clone();
                let populate = populate.clone();
                async move {
                    let result = rx.recv().await;
                    btn.set_sensitive(true);
                    spinner.stop();
                    match result {
                        Ok(Ok(devices)) => {
                            let count = devices.len();
                            populate(devices);
                            toasts.add_toast(adw::Toast::new(&if count == 1 {
                                "Found 1 device — choose which to add".to_string()
                            } else {
                                format!("Found {count} devices — choose which to add")
                            }));
                        }
                        Ok(Err(e)) => {
                            error_label.set_label(&e);
                            error_label.set_visible(true);
                        }
                        Err(_) => {}
                    }
                }
            });
        }
    });
}

// ---- page scaffolding -----------------------------------------------------

/// A wizard page: header bar (with automatic back button), scrollable
/// clamped content box, and an optional "next" pill button at the bottom.
fn page(nav: &adw::NavigationView, tag: &str, title: &str, next: Option<(&str, &str)>) -> gtk::Box {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .build();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&content);
    if let Some((label, next_tag)) = next {
        let btn = gtk::Button::builder()
            .label(label)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .margin_top(10)
            .build();
        let nav = nav.clone();
        let next_tag = next_tag.to_string();
        btn.connect_clicked(move |_| nav.push_by_tag(&next_tag));
        outer.append(&btn);
    }

    let clamp = adw::Clamp::builder()
        .maximum_size(480)
        .margin_bottom(24)
        .child(&outer)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&scroller));
    nav.add(
        &adw::NavigationPage::builder()
            .title(title)
            .tag(tag)
            .child(&tv)
            .build(),
    );
    content
}

fn para(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build()
}

fn dim(text: &str) -> gtk::Label {
    let l = para(text);
    l.add_css_class("dim-label");
    l
}

/// Numbered instruction list.
fn steps(items: &[&str]) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for (i, item) in items.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let num = gtk::Label::builder()
            .label((i + 1).to_string())
            .valign(gtk::Align::Start)
            .css_classes(["about-version-chip"])
            .build();
        row.append(&num);
        let text = para(item);
        text.set_hexpand(true);
        row.append(&text);
        b.append(&row);
    }
    b
}

fn link_row(title: &str, uri: &str) -> gtk::ListBox {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(uri)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
    let uri = uri.to_string();
    row.connect_activated(move |_| open_uri(&uri));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.append(&row);
    list
}

/// A copyable terminal command.
fn command_row(toasts: &adw::ToastOverlay, cmd: &str) -> gtk::Box {
    let label = gtk::Label::builder()
        .label(cmd)
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .css_classes(["monospace"])
        .build();
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy command")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    copy.connect_clicked({
        let toasts = toasts.clone();
        let cmd = cmd.to_string();
        move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&cmd);
                toasts.add_toast(adw::Toast::new("Copied"));
            }
        }
    });
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["card"])
        .build();
    label.set_margin_top(10);
    label.set_margin_bottom(10);
    label.set_margin_start(12);
    b.append(&label);
    b.append(&copy);
    b
}

// ---- instruction pages ----------------------------------------------------

fn intro_page(nav: &adw::NavigationView) {
    let b = page(nav, "intro", "SmartLife Setup", Some(("Start", "account")));
    b.append(&para(
        "This wizard sets up smart plugs (and other devices) that use the \
         Tuya platform — the ones controlled by the SmartLife app.",
    ));
    b.append(&para(
        "Tuya devices encrypt local control with a per-device secret called \
         the local key. It can only be read from the Tuya cloud, so setup \
         means creating a free Tuya developer account, linking your \
         SmartLife account to it, and downloading each device's ID and key. \
         This is done once: afterwards the keys live in Luxel's \
         configuration and control is entirely local — no cloud, no phone.",
    ));
    b.append(&para("You will need:"));
    b.append(&steps(&[
        "The SmartLife app, with your devices already added to it.",
        "A web browser, for the Tuya developer site.",
    ]));
    b.append(&para(
        "Once the developer account is linked, Luxel fetches the keys and \
         finds the devices on your network by itself.",
    ));
    b.append(&dim("Takes about 10 minutes."));
}

fn account_page(nav: &adw::NavigationView) {
    let b = page(nav, "account", "Step 1 · Developer Account", Some(("Next", "project")));
    b.append(&para(
        "Create a free Tuya developer account (the company behind \
         SmartLife). No payment details are needed.",
    ));
    b.append(&steps(&[
        "Open the Tuya IoT Platform in your browser.",
        "Click “Sign Up”, register with any email address, and verify it.",
        "Log in to the platform.",
    ]));
    b.append(&link_row("Tuya IoT Platform", "https://iot.tuya.com"));
}

fn project_page(nav: &adw::NavigationView) {
    let b = page(nav, "project", "Step 2 · Cloud Project", Some(("Next", "link")));
    b.append(&para("Create a cloud project — the container your API keys belong to."));
    b.append(&steps(&[
        "In the left sidebar choose Cloud → Development, then click \
         “Create Cloud Project”.",
        "Name it anything (e.g. “Luxel”). Set Industry and Development \
         Method to “Smart Home”.",
        "Pick the Data Center that matches your SmartLife app account \
         region — in SmartLife: Me → Settings → Account and Security → \
         Region. A mismatched data center is the most common cause of \
         “no devices found” later.",
        "Click Create and authorize the preselected API services \
         (IoT Core, Authorization) when asked.",
    ]));
}

fn link_page(nav: &adw::NavigationView) {
    let b = page(nav, "link", "Step 3 · Link SmartLife", Some(("Next", "keys")));
    b.append(&para(
        "Link your SmartLife account so the project can see your devices.",
    ));
    b.append(&steps(&[
        "Open your new project and go to the “Devices” tab.",
        "Choose “Link Tuya App Account” (sometimes “Link App Account”) → \
         “Add App Account”. A QR code appears.",
        "In the SmartLife app, tap Me, then the scan icon in the top-right \
         corner, and scan the QR code. Confirm the link.",
        "Your devices now show under Devices → All Devices. If the list is \
         empty, the project's data center doesn't match your app region — \
         go back one step.",
    ]));
}

fn keys_page(nav: &adw::NavigationView) {
    let b = page(nav, "keys", "Step 4 · API Keys", Some(("Next", "fetch")));
    b.append(&para("Find the project's API keys; the next step needs them."));
    b.append(&steps(&[
        "Open the project's “Overview” tab.",
        "In the “Authorization Key” section, copy the Access ID/Client ID \
         and the Access Secret/Client Secret (use the copy buttons there).",
    ]));
    b.append(&dim(
        "You'll paste both values into Luxel on the next page.",
    ));
}

fn generate_page(nav: &adw::NavigationView, toasts: &adw::ToastOverlay) {
    let b = page(
        nav,
        "generate",
        "Fallback · tinytuya",
        Some(("Next: Import the File", "import")),
    );
    b.append(&para(
        "If the built-in fetch doesn't work for your account, the \
         open-source tinytuya tool can download your device IDs and local \
         keys into a file called devices.json (needs Python). In a \
         terminal:",
    ));
    b.append(&para("Install tinytuya (either command works):"));
    b.append(&command_row(toasts, "pipx install tinytuya"));
    b.append(&command_row(toasts, "python3 -m pip install --user tinytuya"));
    b.append(&para("Run the wizard in a fresh folder:"));
    b.append(&command_row(toasts, "mkdir -p ~/tuya && cd ~/tuya && tinytuya wizard"));
    b.append(&para("Answer its prompts:"));
    b.append(&steps(&[
        "API Key: the Access ID from the previous step.",
        "API Secret: the Access Secret.",
        "Device ID: just press Enter (“scan”).",
        "Region: the one matching your project's data center (e.g. us, eu, \
         cn, in).",
        "“Download DP name mappings?” — Enter for the default is fine.",
        "“Poll local devices?” — answer N; Luxel scans the network itself \
         in the final step (and unlike tinytuya it works across subnets).",
    ]));
    b.append(&dim("The result is a devices.json file in that folder (~/tuya)."));
}

// ---- import ---------------------------------------------------------------

/// Parse a tinytuya devices.json (or snapshot.json) into importable devices.
fn parse_devices_json(text: &str) -> Result<Vec<TuyaDevice>, String> {
    let val: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "not a JSON file".to_string())?;
    let arr = val
        .as_array()
        .cloned()
        .or_else(|| val.get("devices").and_then(|d| d.as_array()).cloned())
        .ok_or("no device list found in the file")?;
    let text_of = |v: &serde_json::Value, field: &str| {
        v.get(field)
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let mut out = Vec::new();
    for obj in &arr {
        let id = text_of(obj, "id");
        let key = text_of(obj, "key");
        if id.is_empty() || key.is_empty() {
            continue;
        }
        // Zigbee/Bluetooth children of a gateway have no LAN presence.
        if obj.get("sub").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let version = match obj.get("version") {
            Some(serde_json::Value::String(s)) if ["3.1", "3.2", "3.3", "3.4", "3.5"].contains(&s.as_str()) => s.clone(),
            Some(serde_json::Value::Number(n)) => n
                .as_f64()
                .map(|f| format!("{f:.1}"))
                .filter(|s| ["3.3", "3.4", "3.5"].contains(&s.as_str()))
                .unwrap_or_else(|| "auto".to_string()),
            _ => "auto".to_string(),
        };
        out.push(TuyaDevice {
            name: text_of(obj, "name"),
            host: text_of(obj, "ip"),
            id,
            key,
            version: if version == "3.1" || version == "3.2" {
                "auto".to_string()
            } else {
                version
            },
        });
    }
    if out.is_empty() {
        return Err("the file contains no devices with local keys".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::parse_devices_json;

    #[test]
    fn devices_json_parses() {
        // Bare list, tinytuya style; entries without keys or with sub=true
        // (gateway children) are skipped; version normalizes.
        let text = r#"[
            {"name":"Plug A","id":"aaaabbbbccccddddeeee","key":"0123456789abcdef",
             "ip":"10.7.1.20","version":"3.3","mac":"aa:bb"},
            {"name":"Plug B","id":"bbbbccccddddeeeeffff","key":"fedcba9876543210","version":3.4},
            {"name":"Sensor","id":"ccccdddd","key":"0000000000000000","sub":true},
            {"name":"No key","id":"ddddeeee","key":""}
        ]"#;
        let devices = parse_devices_json(text).unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "Plug A");
        assert_eq!(devices[0].host, "10.7.1.20");
        assert_eq!(devices[0].version, "3.3");
        assert_eq!(devices[1].host, "");
        assert_eq!(devices[1].version, "3.4");

        // Wrapped form (snapshot.json), and old versions fall back to auto.
        let text = r#"{"devices":[{"id":"aaaabbbbccccddddeeee","key":"0123456789abcdef","version":"3.1"}]}"#;
        let devices = parse_devices_json(text).unwrap();
        assert_eq!(devices[0].version, "auto");
        assert_eq!(devices[0].name, "");

        assert!(parse_devices_json("not json").is_err());
        assert!(parse_devices_json("[]").is_err());
        assert!(parse_devices_json(r#"{"scenes":[]}"#).is_err());
    }
}

fn import_page(
    ui: &Rc<Ui>,
    nav: &adw::NavigationView,
    win: &adw::Window,
    toasts: &adw::ToastOverlay,
    imported: &ImportedIds,
) {
    let b = page(nav, "import", "Fallback · Import", None);
    b.append(&para(
        "Pick the devices.json file that tinytuya created. Devices found in \
         it are listed below; choose which to add.",
    ));
    let open_btn = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("document-open-symbolic")
                .label("Choose devices.json…")
                .build(),
        )
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    b.append(&open_btn);

    let picker = device_picker(ui, nav, toasts, imported);
    b.append(&picker.widget);

    let populate = picker.populate.clone();
    open_btn.connect_clicked({
        let win = win.clone();
        let toasts = toasts.clone();
        move |_| {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("tinytuya device file (JSON)"));
            filter.add_pattern("*.json");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let dialog = gtk::FileDialog::builder()
                .title("Import devices.json")
                .filters(&filters)
                .build();
            let toasts = toasts.clone();
            let populate = populate.clone();
            dialog.open(Some(&win), gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return }; // dismissed
                let Some(path) = file.path() else { return };
                let devices = std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|text| parse_devices_json(&text));
                match devices {
                    Ok(devices) => populate(devices),
                    Err(e) => {
                        toasts.add_toast(adw::Toast::new(&format!("Import failed: {e}")));
                    }
                }
            });
        }
    });
}

// ---- finalize -------------------------------------------------------------

fn finalize_page(
    ui: &Rc<Ui>,
    nav: &adw::NavigationView,
    win: &adw::Window,
    toasts: &adw::ToastOverlay,
    imported: &ImportedIds,
) {
    let b = page(nav, "finalize", "Step 7 · Connect", None);
    b.append(&para(
        "Luxel is now connecting to your devices. Devices whose IP address \
         is still unknown can be found automatically: enter the subnet(s) \
         they live on and scan — each device is identified by its own key, \
         so this works even across VLANs.",
    ));

    let subnet_row = adw::EntryRow::builder().title("Subnets to scan (CIDR)").build();
    subnet_row.set_text(&ui.config.borrow().lan_subnets.join(", "));
    let subnet_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    subnet_list.append(&subnet_row);
    b.append(&subnet_list);

    let scan_btn = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("system-search-symbolic")
                .label("Scan Network for Devices")
                .build(),
        )
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    b.append(&scan_btn);

    let status_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    b.append(&status_list);

    let done_btn = gtk::Button::builder()
        .label("Finish")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    b.append(&done_btn);
    done_btn.connect_clicked({
        let win = win.clone();
        move |_| win.close()
    });

    // Live status rows for the imported devices, refreshed while the
    // window is open.
    let refresh = {
        let ui = ui.clone();
        let status_list = status_list.clone();
        let imported = imported.clone();
        move || {
            while let Some(child) = status_list.first_child() {
                status_list.remove(&child);
            }
            let cfg = ui.config.borrow();
            for id in imported.borrow().iter() {
                let Some(dev) = cfg.tuya_devices.iter().find(|d| d.id.trim() == id.trim())
                else {
                    continue;
                };
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&if dev.name.trim().is_empty() {
                        "Unnamed device".to_string()
                    } else {
                        dev.name.clone()
                    }))
                    .subtitle(glib::markup_escape_text(&tuya_status_for(&ui, dev)))
                    .build();
                status_list.append(&row);
            }
        }
    };
    refresh();
    glib::timeout_add_local(Duration::from_millis(700), {
        let win = win.clone();
        let refresh = refresh.clone();
        move || {
            if !win.is_visible() {
                return glib::ControlFlow::Break;
            }
            refresh();
            glib::ControlFlow::Continue
        }
    });

    scan_btn.connect_clicked({
        let ui = ui.clone();
        let toasts = toasts.clone();
        let subnet_row = subnet_row.clone();
        move |_| {
            let mut subnets = Vec::new();
            let mut entries = Vec::new();
            for part in subnet_row
                .text()
                .split([',', ' '])
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                match Subnet::parse(part) {
                    Some(s) => {
                        subnets.push(s);
                        entries.push(part.to_string());
                    }
                    None => {
                        subnet_row.add_css_class("error");
                        return;
                    }
                }
            }
            subnet_row.remove_css_class("error");
            if subnets.is_empty() {
                toasts.add_toast(adw::Toast::new(
                    "Enter the subnet the devices are on, e.g. 192.168.20.0/24",
                ));
                return;
            }
            // Newly entered subnets are useful for LIFX discovery too.
            {
                let mut cfg = ui.config.borrow_mut();
                for e in &entries {
                    if !cfg.lan_subnets.contains(e) {
                        cfg.lan_subnets.push(e.clone());
                    }
                }
                cfg.save();
            }
            let all: Vec<Subnet> = ui
                .config
                .borrow()
                .lan_subnets
                .iter()
                .filter_map(|s| Subnet::parse(s))
                .collect();
            let _ = ui.lan_tx.send(LanCommand::SetSubnets(all));

            let candidates: Vec<TuyaDevice> = {
                let merged = ui.merged.borrow();
                ui.config
                    .borrow()
                    .tuya_devices
                    .iter()
                    .filter(|d| !d.id.trim().is_empty() && d.key.trim().len() == 16)
                    .filter(|d| {
                        let connected = merged
                            .get(&format!("tuya:{}", d.id.trim()))
                            .is_some_and(|m| m.state.connected);
                        d.host.trim().is_empty() || !connected
                    })
                    .cloned()
                    .collect()
            };
            if candidates.is_empty() {
                toasts.add_toast(adw::Toast::new("All devices are already connected"));
                return;
            }
            let _ = ui.tuya_tx.send(TuyaCommand::Locate {
                devices: candidates,
                subnets,
            });
            toasts.add_toast(adw::Toast::new("Scanning the network…"));
        }
    });
}
