mod cloud;
mod config;
mod lan;
mod model;
mod tuya;
mod ui;

use gtk::glib;
use gtk::prelude::*;

pub const APP_ID: &str = "io.github.hyprlab.Luxel";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(ui::activate);
    app.run()
}
