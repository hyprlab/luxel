# Luxel

## What's new in 1.2.0
- SmartLife/Tuya smart plug support: plugs are controlled entirely over your local network (works across subnets and VLANs), show a power switch in the lights list, join rooms and scenes, and count into the room and All Lights switches.
- A guided SmartLife Setup wizard (in Settings) does the whole onboarding: it walks you through creating the free Tuya developer account and linking the SmartLife app, then fetches every device's keys straight from the Tuya cloud and finds the devices on your network automatically — no terminal, no files. After setup, control never touches the cloud again.
- Preferences is now called Settings, with the SmartLife section split into a setup group and a device list, and live per-device connection status.
- The primary menu follows the GNOME HIG, including a new Keyboard Shortcuts dialog (Ctrl+?).
- Refreshed app icon.

## In 0.1.2
- Scenes can now be exported to a JSON file and imported back — share them between machines or keep a backup. Find both in the new menu next to "Save Current" on the Scenes tab. Importing replaces scenes with matching names and adds the rest, so re-importing is always safe.

## In 0.1.1
- A refreshed app icon.
- Installs now come from Luxel's own Flatpak repository at luxel.hyprlab.co, so the app updates automatically with `flatpak update` (or through GNOME Software).

## In 0.1.0
- Local control over your Wi-Fi/LAN using the LIFX UDP protocol: bulbs are discovered automatically and respond instantly, with no cloud round-trip.
- Rooms: lights group into room cards with their own color, brightness, and power controls, plus an All Lights card for the whole house. Rooms collapse and expand with a smooth animation.
- Scenes: capture the current look of your lights (choosing exactly which to include), then restore it with one click. Scenes can be updated, renamed, and deleted.
- A LIFX-style color wheel with hue around the rim and white at the center, plus RGB hex entry, warmth sliders with kelvin values and shade names, and brightness percent fields.
- Bulbs on an isolated IoT subnet/VLAN can be reached by listing their subnet in Preferences.
- Optional LIFX Cloud support with a personal access token, used automatically when a bulb isn't reachable locally.
