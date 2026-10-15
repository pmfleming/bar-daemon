# Embedded timezone boundaries

`bar-location-daemon` embeds the unmodified `tzf-dist` lite dataset pinned in
`Cargo.lock` (currently 0.0.2026-e), consumed by `tzf-rs` 2.1.3.

- Distribution/source: https://github.com/ringsaturn/tzf-dist
- Upstream dataset: https://github.com/evansiroky/timezone-boundary-builder
- Contains information from OpenStreetMap contributors: https://www.openstreetmap.org/copyright
- Data license: Open Database License 1.0 (ODbL), reproduced in `ODbL-1.0.txt`.
- Library code: MIT. This does not change the MIT license on bar-daemon's code.

The lite dataset has simplified boundaries (upstream reports about 111 metres
maximum deviation). The daemon adds a small boundary-ambiguity check, requires
independent Wi-Fi/IP agreement, and rejects country/timezone inconsistencies.
This is not exact geographic/country-border certification. No network downloads
of boundary data occur at runtime. Obtain the corresponding unmodified database
from the versioned tzf-dist crate/source above.
