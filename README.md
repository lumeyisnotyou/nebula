# Nebula

A camera app for the Light L16 on Linux (postmarketOS, Phosh): a pro-camera instrument display.
It began as a fork of l16-camera and keeps its hardware paths (the light-ccb driver, the photo
transfers, gyro, proximity sensors, geotagging); the interface is new.

- Dot-matrix encoders for ISO, shutter and EV (drag up for more), an adjust panel with a number
  line, and a focal-length panel that snaps to the primes.
- Mode picker, a grid of keys (tap to cycle, hold to pin), a lens strip, focus peaking and zebras.
- The touch strip zooms; a double tap cycles what it adjusts (zoom, ISO, shutter, EV).
- Swipe right on the keys to stow them (pinned keys stay); swipe in from the left edge for the
  sidebar (settings, quick toggles, about). Accent colour and high contrast are settings.
- A beat of light from the status LED when a photo is taken.

## Building

`cargo build --release` needs GTK 4.12+, GStreamer and the Symbols Nerd Font; on the L16 build it
there (`apk add cargo gtk4.0-dev gstreamer-dev build-base`). On a desktop, `L16_DEMO=1 nebula`
shows a drawn scene with no camera; `L16_SHOT=file.png` saves a screenshot and exits,
`L16_DEMO_VIEW=manual,iso` opens a screen (see `src/dev.rs`).

Run it inside the user's session (so the sensor and location services accept it), e.g. through its
D-Bus activation: `org.l16linux.Nebula`.

Settings live in `~/.config/nebula/settings`; the log is `~/.cache/nebula.log`. Manual focus needs
a lens-position control in the light-ccb driver, which does not exist yet.
