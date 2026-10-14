# Location-authority feedback and effective timezone

`timezone.rs` remains a read-only observer of `org.freedesktop.timedate1`.
PropertiesChanged refreshes the timezone/offset immediately, with the existing
five-minute recovery timer. It never chooses a location or sets the system clock.

The optional NixOS `location-timezone` service owns network/resume invalidation,
GeoClue Wi-Fi plus HTTPS IP consensus and authorized timezone application. Its
public `/run/location-timezone/status.json` has just `{ "message": "..." }`.
A two-second status recovery timer refreshes the projection only when that message
changes; it does not call timedated on every tick. Reads are bounded to 4097 bytes
and public messages to 300 characters. The service's private coordinates are not
read by bar-daemon.

When the status file exists, its message is exposed through the existing
`timezone.error` field. `available` stays true if the effective system timezone
was read successfully: disagreement means the system zone was retained, not that
its offset is unknown. Empty messages clear the warning. Malformed/oversized
status produces a concise unavailable warning. A missing optional service/file
preserves the previous system-timezone-only behavior.

Shelllist displays the warning once in Time & Weather's existing status bar and
uses the observed offset for its visible clock and accessible clock description.
The city in this domain remains the timezone's representative name, not an
independently verified precise locality. Home/weather configuration is untouched.
