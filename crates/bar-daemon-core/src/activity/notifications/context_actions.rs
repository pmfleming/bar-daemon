//! Conservative local extraction. Targets never come back from the UI and never
//! pass through a shell; sender-defined actions retain their original semantics.
use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::LazyLock, time::Duration};

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
    Focus(String),
}
impl Target {
    pub(super) fn key(&self) -> &'static str {
        self.metadata().0
    }

    fn metadata(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Code(_) => ("copy-code", "Copy code", "content_copy"),
            Self::Number(_) => ("copy-number", "Copy number", "content_copy"),
            Self::Link(_) => ("open-link", "Open link", "open_in_new"),
            Self::Join(_) => ("join", "Join", "videocam"),
            Self::Focus(_) => ("focus", "Open application", "open_in_new"),
        }
    }

    pub(super) fn descriptor(&self) -> ContextAction {
        let (key, label, icon) = self.metadata();
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

// Repeated identical targets are unambiguous; stop at the first distinct one.
fn unique<T: PartialEq>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    values.all(|value| value == first).then_some(first)
}

fn link(word: &str) -> Option<reqwest::Url> {
    let candidate = word.trim_end_matches(['.', ',', ';', '!', ')', ']']);
    if candidate.len() > 4096
        || !candidate.starts_with("https://") && !candidate.starts_with("http://")
    {
        return None;
    }
    let url = reqwest::Url::parse(candidate).ok()?;
    (url.host_str().is_some() && url.username().is_empty() && url.password().is_none())
        .then_some(url)
}

fn meeting(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    match url.host_str().unwrap_or_default() {
        "meet.google.com" | "meet.jit.si" => url.path().len() > 1,
        "teams.microsoft.com" | "teams.live.com" => url.path().starts_with("/l/meetup-join/"),
        host if host == "zoom.us" || host.ends_with(".zoom.us") => {
            url.path().starts_with("/j/") || url.path().starts_with("/my/")
        }
        _ => false,
    }
}

pub(super) fn extract(summary: &str, body: &str) -> Vec<Target> {
    let text = format!("{summary}\n{body}");
    let lower = text.to_ascii_lowercase();
    let words = text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''));
    let mut result: Vec<_> = unique(words.filter_map(link))
        .map(|url| {
            if meeting(&url) {
                Target::Join(url.into())
            } else {
                Target::Link(url.into())
            }
        })
        .into_iter()
        .collect();
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
    let clipboard = if !code_context {
        unique(
            PHONE
                .find_iter(&text)
                .map(|m| m.as_str())
                .filter(|s| (8..=15).contains(&s.bytes().filter(u8::is_ascii_digit).count())),
        )
        .map(|number| Target::Number(number.into()))
    } else if lower.contains("https://") || lower.contains("http://") {
        None
    } else {
        unique(CODE.find_iter(&text).map(|m| m.as_str())).map(|code| Target::Code(code.into()))
    };
    result.extend(clipboard);
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
        let mut value: Value = serde_json::from_str(&response)?;
        ensure!(
            value["ok"] == true,
            "{}",
            value["error"]["message"]
                .as_str()
                .unwrap_or("Notification action failed")
        );
        Ok(value["data"].take())
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
        Target::Focus(desktop) => {
            focus(&desktop).await?;
            Ok(false)
        }
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

async fn focus(desktop: &str) -> Result<()> {
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
    use super::{Target, extract};

    #[test]
    fn extracts_only_unambiguous_targets_and_exact_meeting_hosts() {
        for (body, expected) in [
            ("Your verification code is 001234", &["copy-code"][..]),
            ("OTP 001234 repeated 001234", &["copy-code"]),
            ("Verification code 123456 or 654321", &[]),
            ("Order 123456", &[]),
            ("Call +31 20 123 4567", &["copy-number"]),
            ("+31 20 123 4567 or +31 20 765 4321", &[]),
            ("+1234567890123456", &[]),
            ("https://meet.google.com/abc-defg-hij", &["join"]),
            ("https://meet.google.com.evil.test/abc", &["open-link"]),
            ("http://meet.google.com/abc", &["open-link"]),
            ("https://meet.google.com/", &["open-link"]),
            ("https://us02.zoom.us/j/123", &["join"]),
            ("https://zoom.us/my/room", &["join"]),
            ("https://notzoom.us/j/123", &["open-link"]),
            ("https://teams.live.com/l/meetup-join/123", &["join"]),
            ("https://meet.jit.si/room", &["join"]),
            ("https://user:pass@example.com", &[]),
            ("javascript:alert(1) file:///etc/passwd", &[]),
            ("https://one.test https://two.test", &[]),
            ("<https://ONE.test> https://one.test/", &["open-link"]),
            ("OTP 123456 https://example.com", &["open-link"]),
            ("OTP 123456 HTTPS://example.com", &[]),
            (
                "https://one.test +31 20 123 4567",
                &["open-link", "copy-number"],
            ),
        ] {
            let actions = extract("", body);
            assert_eq!(
                actions.iter().map(Target::key).collect::<Vec<_>>(),
                expected,
                "{body}"
            );
            assert!(actions.iter().all(|t| t.descriptor().key == t.key()));
        }
        assert!(extract("", &format!("https://example.com/{}", "a".repeat(4096))).is_empty());
        assert!(
            matches!(extract("Verification code", "001234").as_slice(), [Target::Code(code)] if code == "001234")
        );
    }
}
