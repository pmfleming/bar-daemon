//! Logind capability values combine support, authorization and inhibition.
//! Never bypass inhibitors or prompt implicitly from an automatic action.
use anyhow::{Result, bail};

pub(crate) fn configurable(value: &str) -> bool {
    matches!(
        value,
        "yes" | "challenge" | "inhibited" | "inhibitor-blocked" | "challenge-inhibitor-blocked"
    )
}

pub(crate) fn limitation(value: &str) -> Option<&'static str> {
    match value {
        "yes" => None,
        "challenge" => Some(
            "Authentication is required; non-interactive sleep is not authorized. Configure system policy before using this action.",
        ),
        "inhibited" | "inhibitor-blocked" => Some(
            "Sleep is temporarily inhibited; close or release the blocking application's inhibitor.",
        ),
        "challenge-inhibitor-blocked" => {
            Some("Sleep is inhibited and also requires authentication.")
        }
        "no" => Some("Sleep is disabled or denied by system policy."),
        "na" => Some("Sleep is not supported by the current system configuration."),
        _ => Some("Sleep capability is unknown; refusing an unconfirmed action."),
    }
}

pub(crate) fn require_authorized(value: &str) -> Result<()> {
    if let Some(reason) = limitation(value) {
        bail!("{value}: {reason}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_blockers_and_auth_do_not_remove_configuration_support() {
        for value in [
            "yes",
            "challenge",
            "inhibited",
            "inhibitor-blocked",
            "challenge-inhibitor-blocked",
        ] {
            assert!(configurable(value));
            assert_eq!(require_authorized(value).is_ok(), value == "yes");
        }
        for value in ["no", "na", "future-value", ""] {
            assert!(!configurable(value));
            assert!(require_authorized(value).is_err());
        }
    }
}
