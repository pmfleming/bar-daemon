use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub countdown: bool,
    /// Zero means no automatic clearing. Code copies still bypass history.
    pub code_expiry_seconds: u16,
    pub history_days: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            countdown: true,
            code_expiry_seconds: 0,
            history_days: 30,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingsPatch {
    pub countdown: Option<bool>,
    pub code_expiry_seconds: Option<u16>,
    pub history_days: Option<u8>,
}

impl Settings {
    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=30).contains(&self.history_days),
            "History retention must be 1–30 days"
        );
        anyhow::ensure!(
            self.code_expiry_seconds == 0 || (10..=300).contains(&self.code_expiry_seconds),
            "Code expiry must be Never or 10–300 seconds"
        );
        Ok(())
    }
    pub(crate) fn patched(&self, patch: SettingsPatch) -> Result<Self> {
        let next = Self {
            countdown: patch.countdown.unwrap_or(self.countdown),
            code_expiry_seconds: patch
                .code_expiry_seconds
                .unwrap_or(self.code_expiry_seconds),
            history_days: patch.history_days.unwrap_or(self.history_days),
        };
        next.validate()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patches_validate_bounds_without_overwriting_other_fields() {
        let defaults = Settings::default();
        for days in [0, 31] {
            assert!(
                defaults
                    .patched(
                        serde_json::from_value(serde_json::json!({"history_days": days})).unwrap()
                    )
                    .is_err()
            );
        }
        for seconds in [1, 9, 301] {
            assert!(
                defaults
                    .patched(
                        serde_json::from_value(serde_json::json!({"code_expiry_seconds": seconds}))
                            .unwrap()
                    )
                    .is_err()
            );
        }
        let next = defaults
            .patched(serde_json::from_value(serde_json::json!({"history_days": 1})).unwrap())
            .unwrap();
        assert_eq!(next.history_days, 1);
        assert!(next.countdown);
        assert_eq!(next.code_expiry_seconds, 0);
    }
}
