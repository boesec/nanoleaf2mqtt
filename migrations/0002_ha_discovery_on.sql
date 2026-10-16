-- Schema version 2: Home Assistant discovery is on by default (mqtt-smarthome 2.0, section 8).
-- The settings table holds exactly one row, created by the first migration with discovery off;
-- switching it on here is the default of every new store, and turns it on in an existing one.
-- The column default of the first migration stays as it is: no second row is ever inserted.
UPDATE settings SET ha_discovery = 1 WHERE id = 1;
