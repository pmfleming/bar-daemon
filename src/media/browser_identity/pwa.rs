//! Conservative fallback for the explicitly isolated PWA launcher convention.
//! This is presentation only, not proof of origin or permission to control media.

use std::path::{Component, Path};

use super::BrowserIdentity;
use crate::media::source;

fn launch_flags(command_line: &[u8]) -> Option<(&str, &str, &str)> {
    let text = std::str::from_utf8(command_line.strip_suffix(b"\0")?).ok()?;
    // Linux argv is normally NUL-delimited. Chromium can replace it with one
    // flattened process title. Accept only unambiguous whitespace-free values
    // in that form; never shell-evaluate it or read a browser profile.
    let args: Vec<&str> = if text.contains('\0') {
        text.split('\0').collect()
    } else {
        text.split_ascii_whitespace().collect()
    };
    if args.first()?.is_empty() || args.iter().any(|arg| arg.chars().any(char::is_control)) {
        return None;
    }
    let mut class = None;
    let mut profile = None;
    let mut app = None;
    for arg in args.iter().skip(1) {
        let (key, value) = arg.split_once('=').unwrap_or((arg, ""));
        let slot = match key {
            "--class" => &mut class,
            "--user-data-dir" => &mut profile,
            "--app" => &mut app,
            // Shared-profile installed apps and subprocesses aren't isolated PWAs.
            "--app-id" | "--type" | "--" => return None,
            _ => continue,
        };
        if value.is_empty() || slot.replace(value).is_some() {
            return None;
        }
    }
    Some((class?, profile?, app?))
}

pub(super) fn parse(command_line: &[u8]) -> Option<BrowserIdentity> {
    let (class, profile, app) = launch_flags(command_line)?;
    let profile = Path::new(profile);
    if !profile.is_absolute()
        || profile
            .components()
            .any(|part| part == Component::ParentDir)
        || !profile.ends_with(Path::new("chrome-web-apps").join(class))
    {
        return None;
    }
    let url = source::parse_url(app)?;
    if url.scheme() != "https" || url.port().is_some() {
        return None;
    }
    let host = url.host_str()?;
    let identity = match class {
        "com.laufan.pocketcasts" if host == "play.pocketcasts.com" => "Pocket Casts",
        "com.laufan.audible"
            if source::audible_domain(host.strip_prefix("www.").unwrap_or(host)) =>
        {
            "Audible"
        }
        _ => return None,
    };
    Some(BrowserIdentity {
        identity: identity.into(),
        desktop_entry: class.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::{BrowserIdentity, parse};

    fn command(class: &str, url: &str, separator: &str) -> Vec<u8> {
        format!(
            "/opt/chrome{separator}--user-data-dir=/home/test/.local/share/chrome-web-apps/{class}{separator}--class={class}{separator}--no-first-run{separator}--app={url}\0"
        ).into_bytes()
    }

    #[test]
    fn isolated_pwas_survive_chromium_process_title_rewriting() {
        for (class, url, identity) in [
            (
                "com.laufan.pocketcasts",
                "https://play.pocketcasts.com/",
                "Pocket Casts",
            ),
            (
                "com.laufan.audible",
                "https://www.audible.co.uk/library/titles",
                "Audible",
            ),
        ] {
            for separator in ["\0", " "] {
                assert_eq!(
                    parse(&command(class, url, separator)),
                    Some(BrowserIdentity {
                        identity: identity.into(),
                        desktop_entry: class.into(),
                    })
                );
            }
        }
    }

    #[test]
    fn ambiguous_shared_or_spoofed_pwas_remain_unlabelled() {
        let valid = String::from_utf8(command(
            "com.laufan.pocketcasts",
            "https://play.pocketcasts.com/",
            "\0",
        ))
        .unwrap();
        for invalid in [
            valid.replace("--class=com.laufan.pocketcasts\0", ""),
            valid.replace(
                "--class=com.laufan.pocketcasts",
                "--class=com.laufan.audible",
            ),
            valid.replace("chrome-web-apps/com.laufan.pocketcasts", "google-chrome"),
            valid.replace("/home/test/", "relative/"),
            valid.replace("/home/test/", "/home/test/../"),
            valid.replace("--no-first-run", "--class=com.laufan.pocketcasts"),
            valid.replace("--no-first-run", "--app=https://play.pocketcasts.com/"),
            valid.replace("--no-first-run", "--user-data-dir=/tmp/other"),
            valid.replace("--no-first-run", "--type=renderer"),
            valid.replace("--no-first-run", "--app-id=installed-app"),
            valid.replace("--no-first-run", "--"),
            valid.replace(
                "https://play.pocketcasts.com/",
                "https://play.pocketcasts.com.evil.test/",
            ),
            valid.replace(
                "https://play.pocketcasts.com/",
                "https://evil.test/play.pocketcasts.com/",
            ),
            valid.replace(
                "https://play.pocketcasts.com/",
                "https://user@play.pocketcasts.com/",
            ),
            valid.replace(
                "https://play.pocketcasts.com/",
                "https://play.pocketcasts.com:1234/",
            ),
            valid.replace("https://", "http://"),
            valid.replace("--class=com.laufan.pocketcasts", "--class="),
            valid.replace("--no-first-run", "--app"),
            valid.replace("--no-first-run", "--user-data-dir"),
            valid.replace("--no-first-run", "--class"),
            valid.replace("--no-first-run", "--flag=bad\nvalue"),
            valid.replace("https://", "file://"),
        ] {
            assert_eq!(parse(invalid.as_bytes()), None, "{invalid:?}");
        }
        assert_eq!(
            parse(b"chrome\0--app=https://play.pocketcasts.com/\0"),
            None
        );
        assert_eq!(parse(b"chrome --class=com.laufan.audible"), None);
        assert_eq!(parse(b"\xff\0"), None);
    }

    #[tokio::test]
    async fn process_reads_are_bounded_optional_and_prefer_explicit_labels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_str().unwrap();
        let command_line = command(
            "com.laufan.audible",
            "https://www.audible.co.uk/library/titles",
            " ",
        );
        std::fs::write(directory.path().join("cmdline"), command_line).unwrap();
        assert_eq!(
            super::super::read_process(path).await.unwrap().identity,
            "Audible"
        );
        std::fs::write(
            directory.path().join("environ"),
            b"SHELLLIST_MEDIA_IDENTITY=Custom app\0SHELLLIST_MEDIA_DESKTOP_ENTRY=custom.app\0",
        )
        .unwrap();
        assert_eq!(
            super::super::read_process(path).await.unwrap().identity,
            "Custom app"
        );
        std::fs::write(directory.path().join("environ"), b"UNRELATED=secret\0").unwrap();
        assert_eq!(
            super::super::read_process(path).await.unwrap().identity,
            "Audible"
        );
        std::fs::write(
            directory.path().join("cmdline"),
            vec![b'x'; super::super::MAX_PROCESS_BYTES as usize + 1],
        )
        .unwrap();
        assert!(super::super::read_process(path).await.is_none());
        std::fs::remove_file(directory.path().join("cmdline")).unwrap();
        assert!(super::super::read_process(path).await.is_none());
    }
}
