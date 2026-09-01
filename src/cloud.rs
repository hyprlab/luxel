//! Cloud backend: optional control through the LIFX HTTP API
//! (<https://api.lifx.com/v1>) using a personal access token.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::model::{Backend, BulbState, CloudCommand, DeviceKind, Event, Hsbk};

const API: &str = "https://api.lifx.com/v1";
const POLL_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
struct CloudLight {
    id: String,
    label: String,
    connected: bool,
    power: String,
    brightness: f64,
    color: CloudColor,
    group: Option<CloudGroup>,
    product: Option<CloudProduct>,
}

#[derive(Debug, Deserialize)]
struct CloudProduct {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CloudColor {
    hue: f64,
    saturation: f64,
    kelvin: u16,
}

#[derive(Debug, Deserialize)]
struct CloudGroup {
    name: String,
}

pub fn spawn(events: async_channel::Sender<Event>, commands: Receiver<CloudCommand>) {
    std::thread::Builder::new()
        .name("lifx-cloud".into())
        .spawn(move || run(events, commands))
        .expect("failed to spawn cloud thread");
}

fn run(events: async_channel::Sender<Event>, commands: Receiver<CloudCommand>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();

    let mut token = String::new();
    let mut enabled = false;
    let mut last_poll: Option<Instant> = None;

    loop {
        match commands.recv_timeout(Duration::from_millis(500)) {
            Ok(cmd) => match cmd {
                CloudCommand::Configure {
                    token: t,
                    enabled: e,
                } => {
                    token = t;
                    enabled = e;
                    last_poll = None;
                    if !enabled {
                        let _ = events.send_blocking(Event::CloudError(None));
                    }
                }
                CloudCommand::Refresh => last_poll = None,
                CloudCommand::SetPower { id, on } => {
                    if enabled {
                        set_state(
                            &agent,
                            &token,
                            &id,
                            json!({
                                "power": if on { "on" } else { "off" },
                                "duration": 0.3,
                            }),
                            &events,
                        );
                    }
                }
                CloudCommand::SetColor {
                    id,
                    color,
                    duration_ms,
                } => {
                    if enabled {
                        set_state(
                            &agent,
                            &token,
                            &id,
                            json!({
                                "color": color_string(color),
                                "duration": duration_ms as f64 / 1000.0,
                            }),
                            &events,
                        );
                    }
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if enabled
            && !token.is_empty()
            && last_poll.is_none_or(|t| t.elapsed() >= POLL_INTERVAL)
        {
            poll(&agent, &token, &events);
            last_poll = Some(Instant::now());
        }
    }
}

fn poll(agent: &ureq::Agent, token: &str, events: &async_channel::Sender<Event>) {
    let result = agent
        .get(&format!("{API}/lights/all"))
        .header("Authorization", &format!("Bearer {token}"))
        .call();

    match result {
        Ok(mut resp) => match resp.body_mut().read_json::<Vec<CloudLight>>() {
            Ok(lights) => {
                let _ = events.send_blocking(Event::CloudError(None));
                for light in lights {
                    let _ = events.send_blocking(Event::Upsert(to_state(light)));
                }
            }
            Err(e) => {
                let _ = events.send_blocking(Event::CloudError(Some(format!(
                    "Unexpected reply from LIFX Cloud: {e}"
                ))));
            }
        },
        Err(e) => {
            let _ = events.send_blocking(Event::CloudError(Some(describe_error(&e))));
        }
    }
}

fn set_state(
    agent: &ureq::Agent,
    token: &str,
    id: &str,
    body: serde_json::Value,
    events: &async_channel::Sender<Event>,
) {
    let result = agent
        .put(&format!("{API}/lights/id:{id}/state"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(body);
    if let Err(e) = result {
        let _ = events.send_blocking(Event::CloudError(Some(describe_error(&e))));
    }
}

fn describe_error(e: &ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(401) => {
            "LIFX Cloud rejected the access token. Check it in Settings.".into()
        }
        ureq::Error::StatusCode(code) => format!("LIFX Cloud returned an error (HTTP {code})."),
        other => format!("Could not reach the LIFX Cloud: {other}"),
    }
}

fn to_state(light: CloudLight) -> BulbState {
    BulbState {
        id: light.id.to_lowercase(),
        backend: Backend::Cloud,
        kind: DeviceKind::Bulb,
        label: light.label,
        group: light.group.map(|g| g.name),
        powered: light.power == "on",
        color: Hsbk {
            hue: ((light.color.hue / 360.0) * 65535.0).round() as u16,
            saturation: (light.color.saturation.clamp(0.0, 1.0) * 65535.0).round() as u16,
            brightness: (light.brightness.clamp(0.0, 1.0) * 65535.0).round() as u16,
            kelvin: light.color.kelvin,
        },
        connected: light.connected,
        lan_target: None,
        details: {
            let mut details = vec![("Serial".to_string(), light.id.to_lowercase())];
            if let Some(product) = light.product {
                details.push(("Product".to_string(), product.name));
            }
            details
        },
    }
}

fn color_string(c: Hsbk) -> String {
    let brightness = c.brightness as f64 / 65535.0;
    if c.saturation == 0 {
        format!("kelvin:{} brightness:{brightness:.4}", c.kelvin)
    } else {
        format!(
            "hue:{:.2} saturation:{:.4} brightness:{brightness:.4}",
            (c.hue as f64 / 65535.0) * 360.0,
            c.saturation as f64 / 65535.0,
        )
    }
}
