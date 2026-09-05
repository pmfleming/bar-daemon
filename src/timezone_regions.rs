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

pub(crate) fn current_ids_for_offset(offset_seconds: i32) -> Vec<String> {
    ids_for_offset(offset_seconds, Utc::now())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::ids_for_offset;

    #[test]
    fn resolves_seasonal_and_fractional_offsets() {
        let january = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let july = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        let january_utc1 = ids_for_offset(3_600, january);
        let july_utc2 = ids_for_offset(7_200, july);
        assert!(january_utc1.contains(&"Europe-Paris".into()));
        assert!(july_utc2.contains(&"Europe-Paris".into()));
        assert!(july_utc2.contains(&"Africa-Johannesburg".into()));
        assert!(ids_for_offset(19_800, july).contains(&"Asia-Kolkata".into()));
        assert!(!ids_for_offset(18_000, july).contains(&"Asia-Kolkata".into()));
    }
}
