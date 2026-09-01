//! Shared data model passed between the UI and the backend controller threads.

/// LIFX native color representation. All components are 16-bit.
///
/// When `saturation` is 0 the bulb renders white at `kelvin` temperature;
/// otherwise `hue`/`saturation` define the color and `kelvin` is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hsbk {
    pub hue: u16,
    pub saturation: u16,
    pub brightness: u16,
    pub kelvin: u16,
}

impl Default for Hsbk {
    fn default() -> Self {
        Hsbk {
            hue: 0,
            saturation: 0,
            brightness: 65535,
            kelvin: 3500,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Lan,
    Cloud,
    /// Tuya/SmartLife local protocol (TCP port 6668).
    Tuya,
}

/// What kind of device this is, which decides the controls the UI offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceKind {
    /// Full-color LIFX bulb: power, brightness, color, warmth.
    #[default]
    Bulb,
    /// On/off smart plug: power only.
    Plug,
}

/// Snapshot of a device's state as reported by one backend.
#[derive(Debug, Clone)]
pub struct BulbState {
    /// Canonical device id. For LIFX this is the 12-hex-digit serial,
    /// identical for the LAN and Cloud backends, which is what lets us merge
    /// the two views of a bulb. Tuya devices use "tuya:<device id>" so they
    /// can never collide with a LIFX serial.
    pub id: String,
    pub backend: Backend,
    pub kind: DeviceKind,
    pub label: String,
    pub group: Option<String>,
    pub powered: bool,
    pub color: Hsbk,
    pub connected: bool,
    /// LAN protocol target address (only set by the LIFX LAN backend).
    pub lan_target: Option<u64>,
    /// Vendor/device facts for the Details dialog, as ordered
    /// (label, value) pairs — MAC, IP, product, firmware, data points, …
    pub details: Vec<(String, String)>,
}

/// Events flowing from backend threads to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    Upsert(BulbState),
    /// `Some(message)` shows the error banner, `None` clears it.
    CloudError(Option<String>),
    /// A network scan located a configured Tuya device (raw device id,
    /// without the "tuya:" prefix) at `host`, speaking `version`.
    TuyaFound {
        id: String,
        host: String,
        version: String,
    },
    /// A Tuya network scan finished, having located `found` devices.
    TuyaLocateDone { found: usize },
}

/// An IPv4 subnet in CIDR form, used for cross-VLAN discovery probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subnet {
    network: u32,
    prefix: u8,
}

impl Subnet {
    /// Parse "a.b.c.d/nn" (or a bare address, treated as /32).
    pub fn parse(s: &str) -> Option<Subnet> {
        let s = s.trim();
        let (ip, prefix) = match s.split_once('/') {
            Some((ip, p)) => (ip, p.parse::<u8>().ok()?),
            None => (s, 32),
        };
        if prefix > 32 {
            return None;
        }
        let addr: std::net::Ipv4Addr = ip.parse().ok()?;
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        Some(Subnet {
            network: u32::from(addr) & mask,
            prefix,
        })
    }

    pub fn broadcast(&self) -> std::net::Ipv4Addr {
        let host_bits = 32 - self.prefix;
        let addr = if host_bits == 32 {
            u32::MAX
        } else {
            self.network | ((1u32 << host_bits) - 1)
        };
        std::net::Ipv4Addr::from(addr)
    }

    /// Host addresses to probe with unicast, or empty if the subnet is too
    /// large to sweep (larger than a /22, i.e. 1024 addresses).
    pub fn hosts(&self) -> Vec<std::net::Ipv4Addr> {
        let host_bits = 32 - self.prefix;
        if host_bits > 10 {
            return Vec::new();
        }
        if self.prefix >= 31 {
            return (self.network..=u32::from(self.broadcast()))
                .map(std::net::Ipv4Addr::from)
                .collect();
        }
        ((self.network + 1)..u32::from(self.broadcast()))
            .map(std::net::Ipv4Addr::from)
            .collect()
    }
}

/// Commands the UI sends to the LAN controller thread.
#[derive(Debug, Clone)]
pub enum LanCommand {
    Discover,
    /// Replace the set of extra subnets probed during discovery.
    SetSubnets(Vec<Subnet>),
    SetPower {
        target: u64,
        on: bool,
    },
    SetColor {
        target: u64,
        color: Hsbk,
        duration_ms: u32,
    },
}

/// Commands the UI sends to the Tuya controller thread.
#[derive(Debug, Clone)]
pub enum TuyaCommand {
    /// Replace the set of configured Tuya devices.
    Configure(Vec<crate::config::TuyaDevice>),
    Refresh,
    SetPower {
        /// Device id in "tuya:<device id>" form.
        id: String,
        on: bool,
    },
    /// Scan `subnets` for hosts listening on TCP 6668 and match them to
    /// `devices` by trying each device's local key; answers arrive as
    /// `Event::TuyaFound` / `Event::TuyaLocateDone`.
    Locate {
        devices: Vec<crate::config::TuyaDevice>,
        subnets: Vec<Subnet>,
    },
}

/// Commands the UI sends to the Cloud controller thread.
#[derive(Debug, Clone)]
pub enum CloudCommand {
    Configure {
        token: String,
        enabled: bool,
    },
    Refresh,
    SetPower {
        id: String,
        on: bool,
    },
    SetColor {
        id: String,
        color: Hsbk,
        duration_ms: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::Subnet;
    use std::net::Ipv4Addr;

    #[test]
    fn subnet_parse_and_sweep() {
        let s = Subnet::parse("192.168.20.0/24").unwrap();
        assert_eq!(s.broadcast(), Ipv4Addr::new(192, 168, 20, 255));
        let hosts = s.hosts();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 20, 1));
        assert_eq!(hosts[253], Ipv4Addr::new(192, 168, 20, 254));

        // Non-aligned address is masked down to the network.
        let s = Subnet::parse("10.0.5.77/16").unwrap();
        assert_eq!(s.broadcast(), Ipv4Addr::new(10, 0, 255, 255));
        assert!(s.hosts().is_empty(), "larger than /22 must not sweep");

        // Bare IP acts as /32.
        let s = Subnet::parse("192.168.20.15").unwrap();
        assert_eq!(s.hosts(), vec![Ipv4Addr::new(192, 168, 20, 15)]);

        assert!(Subnet::parse("192.168.20.0/33").is_none());
        assert!(Subnet::parse("not-a-subnet").is_none());
        assert!(Subnet::parse("192.168.20/24").is_none());
    }
}
