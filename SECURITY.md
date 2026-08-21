# Security Policy

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Contact the repository owner privately through the security-reporting channel on the repository hosting service. Include affected versions, reproduction steps, impact, and any suggested mitigation.

Reports will be acknowledged within seven days. A fix and disclosure timeline will be coordinated with the reporter after validation.

## Scope

Supported code is the current `main` branch. Security-sensitive areas include D-Bus request validation, notification persistence, external command execution, filesystem parsing, and compositor or PipeWire control boundaries.
