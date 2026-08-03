# Luxel

Luxel is a native GNOME app for controlling LIFX smart bulbs from the Linux desktop — locally over your network, no phone or cloud account required.

## What's new in 0.1.2
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
