# Changelog

## 1.2.2 — 2026-09-01
- Room and All Lights color controls now slide down inline inside the room card (Colors/Whites panel on the header's color chip toggle) instead of opening a popover overlay.
- Device details popover: an info button beside each row's power switch shows locally sourced facts — LIFX serial/MAC/IP, product name and firmware; SmartLife device ID, address, protocol version, an ON/OFF state line, and the device's remaining raw data points (noise DPs hidden). Values are selectable for copying.
- Per-device subtitles show the connection and vendor (e.g. "Local · LIFX", "Cloud · SmartLife"); a new Settings → Interface switch hides them, except for offline devices which always say so. Room counts and the All Lights subtitle are unaffected.
- Room and All Lights color chips blend multiple colors with a 2D mesh gradient (cairo Coons patch) instead of a linear stripe.
- Sizing polish: 528px default window width with a 524px floor, 64px 3:2 color chips and matching plug ON/OFF pills.

## 1.2.0 — 2026-08-31
- The primary menu now follows the GNOME HIG: sectioned, ordered Settings → Keyboard Shortcuts → About Luxel. A standard Keyboard Shortcuts dialog (Ctrl+?) lists all shortcuts (libadwaita 1.8 ShortcutsDialog; the adw feature level moved from v1_5 to v1_8).
- Preferences is now called Settings throughout the app.
- App icon regenerated from the original 2048px source (all sizes).
- The Settings SmartLife section is split in two groups: the setup wizard row, then the configured devices with an "Add Device Manually" row at the end of the list.
- SmartLife setup wizard (in Settings): step-by-step guidance from creating a Tuya developer account and linking the SmartLife app, then a fully in-app device fetch — Luxel signs Tuya Cloud OpenAPI requests itself (HMAC-SHA256, validated against tinytuya; `/v1.0/token`, `/v1.0/iot-01/associated-users/devices` with a `/v1.3/iot-03/devices` per-user pass for local keys) so no terminal or Python is needed. A device checklist picks what to add, and a finalize page shows live per-device connection status. tinytuya's devices.json import remains as a fallback path. API credentials are stored in the config for later re-fetches; day-to-day control never touches the cloud. Includes a cross-subnet network locate: scans chosen subnets for TCP port 6668 and identifies which configured device answers at each address by trying its local key (a successful encrypted exchange proves identity), then fills in the IP and protocol version automatically — works where broadcast-based scanners can't reach.
- Tuya device fields in Preferences now save as they change (no Enter/apply step) and each device shows a live Status row (missing fields, connecting, connected).
- SmartLife/Tuya smart plug support: a new local backend speaks the Tuya LAN protocol (TCP port 6668, protocol versions 3.3/3.4/3.5 with automatic detection, validated against tinytuya). Plugs are configured in Preferences (IP, device ID, local key — Tuya encrypts local control per device, so the key must be fetched once from the Tuya cloud, e.g. with `tinytuya wizard`) and then work fully offline, across subnets/VLANs. UDP discovery broadcasts (ports 6666/6667) keep a configured device's IP and protocol version up to date automatically.
- Devices now have a kind: smart plugs show a power switch only (no color wheel, brightness or warmth), are excluded from room brightness averages and color chips, participate in room/house power switches and scenes, and room subtitles say "N devices" when a room mixes bulbs and plugs. Room headers (and the All Lights row) hide their color button and brightness slider when they contain only plugs.

## 0.1.2 — 2026-08-02
- Scene export/import: the Scenes tab menu writes all scenes to a versioned JSON file and reads them back (same-name scenes replaced, others added; a bare JSON scene array also parses). File dialogs use the desktop portal, so no new sandbox permissions. Results are reported via a new toast overlay.
- The Flatpak repo install files (.flatpakref/.flatpakrepo/public key) are now tracked in dist-files/.

## 0.1.1 — 2026-08-02
- New app icon (all sizes regenerated; also used by the website and the Flatpak repo metadata).
- Flatpak builds now publish on the `stable` branch and are distributed from the self-hosted signed OSTree repo at luxel.hyprlab.co/flatpak, giving installed users automatic updates.

## 0.1.0 — 2026-08-02
- Initial release.
- LAN backend speaking the LIFX UDP protocol (port 56700): broadcast discovery every 10 s, per-bulb state polling every 3 s, instant set-power/set-color with smooth transitions. Binds the well-known LIFX port when free so firewalled setups still receive broadcast replies.
- Configurable extra subnets (CIDR) probed with directed broadcast plus a unicast sweep, so bulbs on an isolated IoT VLAN are discovered across routed networks.
- Optional LIFX Cloud backend (api.lifx.com v1) with a personal access token. Bulbs seen by both backends merge by serial number; commands always prefer the local path.
- Rooms from the bulbs' LIFX groups, with per-bulb overrides to create custom rooms. Room cards match the All Lights card: color chip, brightness slider with percent field, and power switch, with bulb rows nested inside and animated expand/collapse (persisted per room).
- Scenes stored locally: save with a per-light include checklist, activate with a fade, update from current lights, rename with collision protection, and delete. Scene rows show a gradient chip previewing the scene's colors.
- Per-bulb controls: Colors/Whites mode toggle, hue/saturation color wheel, RGB hex entry, warmth slider with typed kelvin entry (1500–9000 K) and LIFX shade names, brightness slider with percent field.
- Dark-only purple theme and custom app icon.
