//! Conservative local extraction. Targets never come back from the UI and never
//! pass through a shell; sender-defined actions retain their original semantics.
use super::model::ActiveNotification;
use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::LazyLock, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextAction {
    pub key: String,
    pub label: String,
    pub icon: String,
}

pub(super) enum Target {
    Code(String),
    Number(String),
    Link(String),
    Join(String),
}
impl Target {
    pub(super) fn descriptor(&self) -> ContextAction {
        let (key, label, icon) = match self {
            Self::Code(_) => ("copy-code", "Copy code", "content_copy"),
            Self::Number(_) => ("copy-number", "Copy number", "content_copy"),
            Self::Link(_) => ("open-link", "Open link", "open_in_new"),
            Self::Join(_) => ("join", "Join", "videocam"),
        };
        ContextAction {
            key: key.into(),
            label: label.into(),
            icon: icon.into(),
        }
    }
}
static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9]{4,8}\b").unwrap());
static PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+[0-9][0-9 ()-]{6,24}[0-9]").unwrap());

pub(super) fn extract(n: &ActiveNotification) -> Vec<Target> {
    let text = format!("{}\n{}", n.summary, n.body);
    let lower = text.to_ascii_lowercase();
    let mut result = Vec::new();
    let mut urls = BTreeSet::new();
    for word in text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'')) {
        let candidate = word.trim_end_matches(['.', ',', ';', '!', ')', ']']);
        if candidate.len() > 4096
            || !candidate.starts_with("https://") && !candidate.starts_with("http://")
        {
            continue;
        }
        if let Ok(url) = reqwest::Url::parse(candidate) {
            if url.host_str().is_some() && url.username().is_empty() && url.password().is_none() {
                urls.insert(url.to_string());
            }
        }
    }
    if urls.len() == 1 {
        let url = urls.into_iter().next().unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        let host = parsed.host_str().unwrap_or_default();
        let meeting = parsed.scheme() == "https"
            && (host == "meet.google.com" && parsed.path().len() > 1
                || (host == "zoom.us" || host.ends_with(".zoom.us"))
                    && (parsed.path().starts_with("/j/") || parsed.path().starts_with("/my/"))
                || ["teams.microsoft.com", "teams.live.com"].contains(&host)
                    && parsed.path().starts_with("/l/meetup-join/")
                || host == "meet.jit.si" && parsed.path().len() > 1);
        result.push(if meeting {
            Target::Join(url)
        } else {
            Target::Link(url)
        });
    }
    let code_context = [
        "verification code",
        "login code",
        "security code",
        "authentication code",
        "one-time",
        "one time",
        "passcode",
        "otp",
    ]
    .iter()
    .any(|s| lower.contains(s));
    // Links/numbers in a code message are not guessed as additional clipboard targets.
    if code_context && !lower.contains("https://") && !lower.contains("http://") {
        let candidates: BTreeSet<_> = CODE.find_iter(&text).map(|m| m.as_str()).collect();
        if candidates.len() == 1 {
            result.push(Target::Code((*candidates.first().unwrap()).into()));
        }
    } else if !code_context {
        let numbers: BTreeSet<_> = PHONE
            .find_iter(&text)
            .map(|m| m.as_str())
            .filter(|s| (8..=15).contains(&s.bytes().filter(u8::is_ascii_digit).count()))
            .collect();
        if numbers.len() == 1 {
            result.push(Target::Number((*numbers.first().unwrap()).into()));
        }
    }
    result
}

async fn daemon_call(
    name: &str,
    path: &str,
    interface: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let connection = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(&connection, name, path, interface).await?;
        let response: String = proxy.call("Call", &(method, params.to_string())).await?;
        let value: Value = serde_json::from_str(&response)?;
        ensure!(
            value["ok"] == true,
            "{}",
            value["error"]["message"]
                .as_str()
                .unwrap_or("Notification action failed")
        );
        Ok(value["data"].clone())
    })
    .await
    .context("Notification action outcome unknown; do not automatically retry")?
}

pub(super) async fn execute(target: Target, seconds: u16) -> Result<bool> {
    let seconds = if matches!(&target, Target::Number(_)) {
        0
    } else {
        seconds
    };
    match target {
        Target::Code(text) | Target::Number(text) => {
            // Numbers also use the memory-only publisher, with no automatic expiry.
            let value = daemon_call(
                "org.laufan.ClipDaemon",
                "/org/laufan/ClipDaemon",
                "org.laufan.ClipDaemon2",
                "clipboard.selection.publishSensitive",
                json!({"text": text, "expiry_seconds": seconds}),
            )
            .await?;
            ensure!(
                value["operation"]["status"] == "completed",
                "Clipboard ownership not confirmed"
            );
            Ok(true)
        }
        Target::Link(url) | Target::Join(url) => {
            // Portal owns user consent and launch. Request admission is not a
            // claim that the remote meeting has connected.
            let connection = zbus::Connection::session().await?;
            let proxy = zbus::Proxy::new(
                &connection,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.OpenURI",
            )
            .await?;
            let options = std::collections::HashMap::<String, zvariant::OwnedValue>::new();
            let _: zvariant::OwnedObjectPath = tokio::time::timeout(
                Duration::from_secs(3),
                proxy.call("OpenURI", &("", url, options)),
            )
            .await
            .context("Link handoff timed out")??;
            Ok(false)
        }
    }
}

pub(super) async fn focus(desktop: &str) -> Result<()> {
    ensure!(
        !desktop.is_empty()
            && desktop.len() <= 1024
            && !desktop.contains(['/', '\\'])
            && !desktop.chars().any(char::is_control),
        "No trusted application identity"
    );
    let target = format!("{}.desktop", desktop.trim_end_matches(".desktop"));
    // app-daemon validates installed identity and owns activation/launch outcome.
    daemon_call(
        "org.laufan.AppDaemon",
        "/org/laufan/AppDaemon",
        "org.laufan.AppDaemon2",
        "applications.execute",
        json!({"target_id": target, "action": "activate"}),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::model::incoming;
    use super::*;
    fn actions(body: &str) -> Vec<ContextAction> {
        extract(&ActiveNotification::from_incoming(
            1,
            incoming("Chat", "", body),
            1,
        ))
        .iter()
        .map(Target::descriptor)
        .collect()
    }
    #[test]
    fn extracts_only_unambiguous_targets_and_exact_meeting_hosts() {
        assert_eq!(
            actions("Your verification code is 001234")[0].key,
            "copy-code"
        );
        assert!(actions("Verification code 123456 or 654321").is_empty());
        assert!(actions("Order 123456").is_empty());
        assert_eq!(actions("Call +31 20 123 4567")[0].key, "copy-number");
        assert_eq!(
            actions("https://meet.google.com/abc-defg-hij")[0].key,
            "join"
        );
        assert_eq!(
            actions("https://meet.google.com.evil.test/abc")[0].key,
            "open-link"
        );
        assert!(actions("https://user:pass@example.com").is_empty());
        assert!(actions("javascript:alert(1) file:///etc/passwd").is_empty());
        assert!(actions("https://one.test https://two.test").is_empty());
    }
}
