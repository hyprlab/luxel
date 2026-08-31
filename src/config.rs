use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One bulb's saved state inside a scene.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneBulb {
    pub id: String,
    pub powered: bool,
    pub hue: u16,
    pub saturation: u16,
    pub brightness: u16,
    pub kelvin: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    pub bulbs: Vec<SceneBulb>,
}

/// On-disk format for scene export/import.
#[derive(Debug, Serialize, Deserialize)]
pub struct ScenesFile {
    pub app: String,
    pub version: u32,
    pub scenes: Vec<Scene>,
}

/// One SmartLife/Tuya device controlled over the LAN. Tuya's local protocol
/// encrypts every exchange with a per-device key that only the Tuya cloud
/// knows, so unlike LIFX bulbs these must be configured by hand.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TuyaDevice {
    /// Display name shown in the lights list.
    pub name: String,
    /// IP address (Tuya devices are probed by unicast, so cross-subnet works).
    pub host: String,
    /// Tuya device id (20+ characters, from the Tuya cloud / tinytuya wizard).
    pub id: String,
    /// Per-device local key (16 characters, from the Tuya cloud).
    pub key: String,
    /// Protocol version: "auto", "3.3", "3.4" or "3.5".
    pub version: String,
}

impl Default for TuyaDevice {
    fn default() -> Self {
        TuyaDevice {
            name: String::new(),
            host: String::new(),
            id: String::new(),
            key: String::new(),
            version: "auto".to_string(),
        }
    }
}

impl TuyaDevice {
    /// A device can only be reached once all connection fields are set.
    pub fn is_complete(&self) -> bool {
        !self.host.trim().is_empty()
            && !self.id.trim().is_empty()
            && self.key.trim().len() == 16
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub cloud_token: String,
    pub cloud_enabled: bool,
    /// Extra subnets to probe during discovery, in CIDR form
    /// (e.g. "192.168.20.0/24").
    pub lan_subnets: Vec<String>,
    /// Per-bulb room overrides (bulb id → room name). Bulbs without an
    /// override use the group reported by the bulb itself.
    pub rooms: HashMap<String, String>,
    pub scenes: Vec<Scene>,
    /// Rooms whose bulb list is collapsed in the Lights view.
    pub collapsed_rooms: Vec<String>,
    /// SmartLife/Tuya devices controlled over the LAN.
    pub tuya_devices: Vec<TuyaDevice>,
    /// Tuya Cloud API credentials (used only by the setup wizard to fetch
    /// device local keys; day-to-day control never touches the cloud).
    pub tuya_api_id: String,
    pub tuya_api_secret: String,
    /// Data center region code ("us", "eu", …); empty until chosen.
    pub tuya_api_region: String,
}

fn config_path() -> PathBuf {
    gtk::glib::user_config_dir().join("luxel").join("config.json")
}

/// Config location from before the app was renamed to Luxel.
fn legacy_config_path() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("lifx-panel")
        .join("config.json")
}

impl Config {
    pub fn load() -> Config {
        let read = |path: PathBuf| {
            fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        };
        read(config_path())
            .or_else(|| read(legacy_config_path()))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes_file_roundtrip() {
        let file = ScenesFile {
            app: "luxel".into(),
            version: 1,
            scenes: vec![Scene {
                name: "Movie Night".into(),
                bulbs: vec![SceneBulb {
                    id: "d073d5000001".into(),
                    powered: true,
                    hue: 100,
                    saturation: 200,
                    brightness: 300,
                    kelvin: 3500,
                }],
            }],
        };
        let json = serde_json::to_string_pretty(&file).unwrap();
        let back: ScenesFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.scenes.len(), 1);
        assert_eq!(back.scenes[0].name, "Movie Night");
        assert_eq!(back.scenes[0].bulbs[0].kelvin, 3500);

        // A bare scene array (the import fallback) also parses.
        let bare = r#"[{"name":"A","bulbs":[]}]"#;
        let scenes: Vec<Scene> = serde_json::from_str(bare).unwrap();
        assert_eq!(scenes[0].name, "A");
    }
}
