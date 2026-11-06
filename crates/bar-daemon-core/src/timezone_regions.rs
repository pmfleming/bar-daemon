use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;

// Canonical land regions emitted by timezone-boundary-builder 2026c's
// timezones-now dataset, excluding Antarctica outside the compact map viewport.
const REGION_TIMEZONES: &[&str] = &[
    "Africa/Abidjan",
    "Europe/Moscow",
    "Africa/Lagos",
    "Africa/Johannesburg",
    "Africa/Cairo",
    "Africa/Casablanca",
    "Europe/Paris",
    "America/Adak",
    "America/Anchorage",
    "America/Caracas",
    "America/Sao_Paulo",
    "America/Lima",
    "America/Mexico_City",
    "America/Denver",
    "America/Chicago",
    "America/Phoenix",
    "America/New_York",
    "America/Halifax",
    "America/Havana",
    "America/Los_Angeles",
    "America/Miquelon",
    "America/Noronha",
    "America/Nuuk",
    "America/Santiago",
    "America/St_Johns",
    "Asia/Manila",
    "Asia/Jakarta",
    "Australia/Brisbane",
    "Australia/Sydney",
    "Asia/Karachi",
    "Pacific/Auckland",
    "Pacific/Fiji",
    "Asia/Dubai",
    "Asia/Beirut",
    "Asia/Dhaka",
    "Asia/Tokyo",
    "Asia/Kolkata",
    "Europe/Athens",
    "Asia/Gaza",
    "Asia/Jerusalem",
    "Asia/Kabul",
    "Asia/Kathmandu",
    "Asia/Sakhalin",
    "Asia/Tehran",
    "Asia/Yangon",
    "Atlantic/Azores",
    "Europe/London",
    "Atlantic/Cape_Verde",
    "Australia/Adelaide",
    "Australia/Darwin",
    "Australia/Eucla",
    "Australia/Lord_Howe",
    "Pacific/Tongatapu",
    "Pacific/Chatham",
    "Pacific/Easter",
    "Pacific/Gambier",
    "Pacific/Honolulu",
    "Pacific/Kiritimati",
    "Pacific/Marquesas",
    "Pacific/Pago_Pago",
    "Pacific/Norfolk",
    "Pacific/Pitcairn",
];

pub(crate) fn ids_for_offset(offset_seconds: i32, at: DateTime<Utc>) -> Vec<String> {
    REGION_TIMEZONES
        .iter()
        .filter_map(|name| {
            let zone = name.parse::<Tz>().ok()?;
            (at.with_timezone(&zone).offset().fix().local_minus_utc() == offset_seconds)
                .then(|| name.replace('/', "-"))
        })
        .collect()
}
