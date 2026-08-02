# Changelog

## 0.1.0 — 2026-08-02
- Initial release.
- LAN backend speaking the LIFX UDP protocol (port 56700): broadcast discovery every 10 s, per-bulb state polling every 3 s, instant set-power/set-color with smooth transitions. Binds the well-known LIFX port when free so firewalled setups still receive broadcast replies.
- Configurable extra subnets (CIDR) probed with directed broadcast plus a unicast sweep, so bulbs on an isolated IoT VLAN are discovered across routed networks.
- Optional LIFX Cloud backend (api.lifx.com v1) with a personal access token. Bulbs seen by both backends merge by serial number; commands always prefer the local path.
- Rooms from the bulbs' LIFX groups, with per-bulb overrides to create custom rooms. Room cards match the All Lights card: color chip, brightness slider with percent field, and power switch, with bulb rows nested inside and animated expand/collapse (persisted per room).
- Scenes stored locally: save with a per-light include checklist, activate with a fade, update from current lights, rename with collision protection, and delete. Scene rows show a gradient chip previewing the scene's colors.
- Per-bulb controls: Colors/Whites mode toggle, hue/saturation color wheel, RGB hex entry, warmth slider with typed kelvin entry (1500–9000 K) and LIFX shade names, brightness slider with percent field.
- Dark-only purple theme and custom app icon.
