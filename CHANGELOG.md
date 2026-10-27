# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The MQTT contract has its
own version, specified in [CONTRACT.md](CONTRACT.md).

## [Unreleased]

## [0.1.0] - 2026-10-27

The first release.

### Added

- Bridge between Nanoleaf light panels and MQTT over the local Nanoleaf OpenAPI (REST and the
  device's event stream), for any number of devices.
- State per device on MQTT: on/off, brightness, colour (hue/saturation or colour temperature),
  colour mode, the active effect and the device's model and firmware; only what the device
  reports is published, and after a command the state the device reports back.
- Commands per device over MQTT: on/off, brightness (with transition), colour, colour
  temperature, effect selection and identify; out-of-range values are clamped.
- The device's effect list and its panel layout.
- Touch gestures as MQTT events: tap, double tap, swipes in four directions and `long_press`.
- Device reachability per device in `status/<id>/online`; reconnects to the devices and the
  broker forever, with backoff.
- MQTT contract version 2, following mqtt-smarthome 2.0; deliberate deviations are listed in
  the contract.
- Home Assistant MQTT discovery, in addition to the plain topics and switchable.
- A configuration web UI with one admin password: broker, topic prefix, devices, pairing and the
  discovery switch; changes apply without a restart. The configuration is kept in a SQLite file.
- `nanoleaf2mqtt pair` to request a device token on the command line, and
  `nanoleaf2mqtt reset-password` to remove a forgotten admin password.
- A hardened systemd unit and an install script.
- Static Linux binaries for x86_64, aarch64 and armv7, with SHA-256 checksums.

[Unreleased]: https://github.com/boesec/nanoleaf2mqtt/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/boesec/nanoleaf2mqtt/releases/tag/v0.1.0
