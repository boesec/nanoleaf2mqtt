# nanoleaf2mqtt

Bridge between Nanoleaf light panels and MQTT, written in Rust.

It reads the panels' state through the local Nanoleaf OpenAPI, follows their event stream,
publishes state and touch gestures following the
[mqtt-smarthome](https://github.com/mqtt-smarthome/mqtt-smarthome) conventions and executes
commands received over MQTT.

Status: in development.
