pub(crate) fn unix_ms() -> u64 {
    chrono::Utc::now()
        .timestamp_millis()
        .try_into()
        .unwrap_or(0)
}

pub(crate) fn unix_ms_i64() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
