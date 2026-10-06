//! Opt-in labels for isolated Chromium web apps. Metadata and control targets
//! remain MPRIS-owned; these hints only change presentation, never authorization.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::AsyncReadExt;

mod pwa;

const MAX_PROCESS_BYTES: u64 = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct BrowserIdentity {
    pub identity: String,
    pub desktop_entry: String,
}

pub(super) async fn read(connection: &zbus::Connection, name: &str) -> Option<BrowserIdentity> {
    if !is_chromium_instance(name) {
        return None;
    }
    // A missing /proc entry, restricted environment, or disappearing owner must
    // never hide a working player or delay all other players indefinitely.
    tokio::time::timeout(Duration::from_millis(500), read_owner(connection, name))
        .await
        .ok()?
        .ok()
}

fn is_chromium_instance(name: &str) -> bool {
    [
        "org.mpris.MediaPlayer2.chromium.instance",
        "org.mpris.MediaPlayer2.chrome.instance",
    ]
    .iter()
    .any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
    })
}

async fn read_owner(connection: &zbus::Connection, name: &str) -> Result<BrowserIdentity> {
    let bus = zbus::fdo::DBusProxy::new(connection).await?;
    let owner = bus.get_name_owner(name.try_into()?).await?;
    // Never trust the PID embedded in a caller-selected MPRIS service name.
    let pid = bus
        .get_connection_unix_process_id(owner.clone().into())
        .await?;
    let labels = read_process(&format!("/proc/{pid}")).await;
    ensure!(
        bus.get_name_owner(name.try_into()?).await? == owner,
        "owner changed"
    );
    labels.context("no valid browser presentation labels")
}

async fn read_bounded(path: &str) -> Option<Vec<u8>> {
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PROCESS_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    (bytes.len() as u64 <= MAX_PROCESS_BYTES).then_some(bytes)
}

async fn read_process(directory: &str) -> Option<BrowserIdentity> {
    // Never log or retain process bytes: both files may contain unrelated secrets.
    if let Some(environment) = read_bounded(&format!("{directory}/environ")).await
        && let Some(labels) = parse_environment(&environment)
    {
        return Some(labels);
    }
    // Chromium can rewrite its process environment/title, losing launcher hints.
    // Only a matching isolated profile + class + known app URL is a fallback.
    pwa::parse(&read_bounded(&format!("{directory}/cmdline")).await?)
}

fn parse_environment(environment: &[u8]) -> Option<BrowserIdentity> {
    fn value<'a>(environment: &'a [u8], prefix: &[u8]) -> Option<&'a str> {
        let mut values = environment
            .split(|byte| *byte == 0)
            .filter_map(|entry| entry.strip_prefix(prefix));
        let value = std::str::from_utf8(values.next()?).ok()?;
        if values.next().is_some() {
            return None;
        }
        Some(value)
    }

    let identity = value(environment, b"SHELLLIST_MEDIA_IDENTITY=")?;
    let desktop_entry = value(environment, b"SHELLLIST_MEDIA_DESKTOP_ENTRY=")?;
    if identity.is_empty()
        || identity.len() > 128
        || identity.trim() != identity
        || identity.chars().any(char::is_control)
        || desktop_entry.len() > 128
        || !desktop_entry.as_bytes().first()?.is_ascii_alphanumeric()
        || !desktop_entry
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || desktop_entry.ends_with(".desktop")
    {
        return None;
    }
    Some(BrowserIdentity {
        identity: identity.to_owned(),
        desktop_entry: desktop_entry.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_explicit_paired_and_do_not_consume_unrelated_environment() {
        for (name, desktop) in [
            ("Audible", "com.laufan.audible"),
            ("Pocket Casts", "com.laufan.pocketcasts"),
        ] {
            let environment = format!(
                "OTHER=private\0SHELLLIST_MEDIA_IDENTITY={name}\0SHELLLIST_MEDIA_DESKTOP_ENTRY={desktop}\0"
            );
            assert_eq!(
                parse_environment(environment.as_bytes()),
                Some(BrowserIdentity {
                    identity: name.into(),
                    desktop_entry: desktop.into(),
                })
            );
        }
        for environment in [
            &b"CHROME_DESKTOP=google-chrome.desktop\0"[..],
            b"SHELLLIST_MEDIA_IDENTITY=Audible\0",
            b"SHELLLIST_MEDIA_IDENTITY=Audible\0SHELLLIST_MEDIA_DESKTOP_ENTRY=../../file\0",
            b"SHELLLIST_MEDIA_IDENTITY=Bad\nName\0SHELLLIST_MEDIA_DESKTOP_ENTRY=app\0",
            b"SHELLLIST_MEDIA_IDENTITY=Audible\0SHELLLIST_MEDIA_DESKTOP_ENTRY=\0",
            b"SHELLLIST_MEDIA_IDENTITY=Audible\0SHELLLIST_MEDIA_DESKTOP_ENTRY=app.desktop\0",
            b"SHELLLIST_MEDIA_IDENTITY=Audible\0SHELLLIST_MEDIA_IDENTITY=Other\0SHELLLIST_MEDIA_DESKTOP_ENTRY=app\0",
        ] {
            assert_eq!(parse_environment(environment), None);
        }
    }

    struct Browser;

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl Browser {
        #[zbus(property)]
        fn metadata(&self) -> std::collections::HashMap<String, zvariant::OwnedValue> {
            [(
                "xesam:title".into(),
                zvariant::Value::from("Now playing").try_to_owned().unwrap(),
            )]
            .into_iter()
            .collect()
        }
    }

    #[test]
    fn dbus_owner_labels_preserve_player_id_and_metadata() {
        const TEST: &str =
            "media::browser_identity::tests::dbus_owner_labels_preserve_player_id_and_metadata";
        if std::env::var_os("BAR_DAEMON_BROWSER_IDENTITY_TEST").is_none() {
            let directory = tempfile::tempdir().unwrap();
            let bus_config = directory.path().join("bus.conf");
            std::fs::write(
                &bus_config,
                include_str!("../../test_support/dbus-session.conf"),
            )
            .unwrap();
            // Give only the child process presentation hints, never mutate the
            // multithreaded test runner's environment or use the desktop bus.
            for (identity, desktop) in [
                ("Audible", "com.laufan.audible"),
                ("Pocket Casts", "com.laufan.pocketcasts"),
            ] {
                let status = std::process::Command::new("dbus-run-session")
                    .arg(format!("--config-file={}", bus_config.display()))
                    .arg("--")
                    .arg(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env("BAR_DAEMON_BROWSER_IDENTITY_TEST", "1")
                    .env("SHELLLIST_MEDIA_IDENTITY", identity)
                    .env("SHELLLIST_MEDIA_DESKTOP_ENTRY", desktop)
                    .status()
                    .expect("dbus-run-session must be available in the test environment");
                assert!(status.success());
            }
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            // The suffix deliberately is NOT this process's PID.
            let name = "org.mpris.MediaPlayer2.chromium.instance1";
            let server = zbus::connection::Builder::session()
                .unwrap()
                .name(name)
                .unwrap()
                .serve_at(super::super::PATH, Browser)
                .unwrap()
                .build()
                .await
                .unwrap();
            let client = zbus::Connection::session().await.unwrap();
            let player = super::super::read_player(&client, name).await.unwrap();
            assert_eq!(
                player.identity,
                std::env::var("SHELLLIST_MEDIA_IDENTITY").unwrap()
            );
            assert_eq!(
                player.desktop_entry,
                std::env::var("SHELLLIST_MEDIA_DESKTOP_ENTRY").unwrap()
            );
            assert_eq!(player.id, name);
            assert_eq!(player.title, "Now playing");
            server.release_name(name).await.unwrap();
            assert!(read(&client, name).await.is_none());
        });
    }

    #[test]
    fn only_chromium_instances_opt_in() {
        assert!(is_chromium_instance(
            "org.mpris.MediaPlayer2.chromium.instance123"
        ));
        assert!(is_chromium_instance(
            "org.mpris.MediaPlayer2.chrome.instance456"
        ));
        for name in [
            "org.mpris.MediaPlayer2.spotify",
            "org.mpris.MediaPlayer2.chromium.instance",
            "org.mpris.MediaPlayer2.chromium.instance123.fake",
            "org.mpris.MediaPlayer2.chromium.instance/../../1",
        ] {
            assert!(!is_chromium_instance(name));
        }
    }
}
