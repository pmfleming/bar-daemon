//! Notification grouping is descriptive, never application authorization.
pub(crate) use crate::desktop_identity::{icon, refresh};

// Artwork is not sender identity; unnamed senders must remain distinct.
pub(super) fn app_key(desktop: &str, name: &str, id: u32, created: u64) -> String {
    if desktop.len() > 1024 {
        format!("unknown:{created}:{id}")
    } else if !desktop.trim().is_empty() {
        format!("desktop:{}", desktop.trim())
    } else if name.len() > 1024 || name.trim().is_empty() {
        format!("unknown:{created}:{id}")
    } else {
        format!("named:{}", name.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::app_key;
    #[test]
    fn grouping_is_bounded_and_keeps_unnamed_senders_distinct() {
        assert_eq!(app_key(&"d".repeat(1025), "App", 7, 8), "unknown:8:7");
        assert_ne!(app_key("", "a:b", 1, 2), app_key("", "a", 1, 2));
    }
}
