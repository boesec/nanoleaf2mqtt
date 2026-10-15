-- Schema version 1: the configuration of the bridge.
-- Booleans are INTEGER 0/1, timestamps RFC 3339 text in UTC, `lc` milliseconds since the epoch.
-- Secrets (broker password, device tokens) are stored as plain text: the database file is
-- readable by the service user only.

-- Broker, topic prefix and Home Assistant discovery. Exactly one row.
CREATE TABLE settings (
    id                  INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
    -- `mqtt://host[:port]`; NULL while no broker is configured.
    broker_url          TEXT,
    broker_username     TEXT,
    broker_password     TEXT,
    topic_prefix        TEXT NOT NULL DEFAULT 'nanoleaf',
    ha_discovery        INTEGER NOT NULL DEFAULT 0 CHECK (ha_discovery IN (0, 1)),
    -- Topic prefix Home Assistant listens on for discovery messages.
    ha_discovery_prefix TEXT NOT NULL DEFAULT 'homeassistant',
    updated_at          TEXT NOT NULL
);

INSERT INTO settings (id, updated_at) VALUES (1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));

-- One row per device. The id is the topic level below the prefix and never changes.
CREATE TABLE devices (
    id          TEXT PRIMARY KEY NOT NULL
                CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9-]*'),
    name        TEXT NOT NULL,
    host        TEXT NOT NULL,
    port        INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    -- Auth token of the device's OpenAPI; NULL until paired or entered.
    token       TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- The admin password of the configuration UI (PHC string). No row until it is set.
CREATE TABLE admin (
    id             INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
    password_hash  TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

-- When each status value last changed, so `lc` survives a restart. `path` is the status path
-- below the device (`on`, `brightness`, ...), `val` the last value as JSON.
CREATE TABLE last_change (
    device_id  TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    path       TEXT NOT NULL,
    val        TEXT NOT NULL,
    lc         INTEGER NOT NULL,
    PRIMARY KEY (device_id, path)
) WITHOUT ROWID;
