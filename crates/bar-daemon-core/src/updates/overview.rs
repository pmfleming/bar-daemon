//! Read-only worker overview. No shell, sudo, caller-supplied command or deployment effect.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncReadExt;

const MAX_REPORT_BYTES: usize = 1024 * 1024;
const MAX_ISSUES: usize = 50;

struct ProcessGroup(Option<rustix::process::Pid>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // Retire descendants in the reader's group on cancellation. The
            // worker has a shorter internal deadline to drain its own groups.
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}
async fn read_worker(path: &Path) -> Result<Value> {
    read_worker_with_timeout(path, Duration::from_secs(5)).await
}
async fn read_worker_with_timeout(path: &Path, deadline: Duration) -> Result<Value> {
    ensure!(
        path.is_absolute(),
        "Update worker executable must be an absolute path"
    );
    let read = async {
        let mut child = tokio::process::Command::new(path)
            .args(["status", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .context("Open installed update-worker status")?;
        let mut group = ProcessGroup(
            child
                .id()
                .and_then(|id| rustix::process::Pid::from_raw(id as i32)),
        );
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .context("Worker status output missing")?
            .take(MAX_REPORT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= MAX_REPORT_BYTES,
            "Worker status exceeds 1 MiB"
        );
        let status = child.wait().await?;
        group.0 = None;
        ensure!(status.success(), "Worker status failed: {status}");
        normalize(&serde_json::from_slice(&bytes).context("Decode worker overview")?)
    };
    tokio::time::timeout(deadline, read)
        .await
        .context("Worker overview timed out")?
}
fn text(value: &Value) -> Option<String> {
    value.as_str().map(|s| s.chars().take(512).collect())
}
fn timestamp(at: u64) -> Option<u64> {
    (1..253402300800).contains(&at).then_some(at)
}
fn stamp(value: &Value) -> Option<u64> {
    timestamp(value.as_u64()?)
}
fn field(value: &Value, max: usize) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
fn git_overview(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    ensure!(
        value.is_object() && value["available"].is_boolean(),
        "Invalid repository overview"
    );
    if value["available"] == false {
        return Ok(
            json!({"available":false,"path":text(&value["path"]),"error":text(&value["error"])}),
        );
    }
    let count =
        |key| value[key].is_null() || value[key].as_u64().is_some_and(|n| n <= u32::MAX as u64);
    ensure!(
        count("ahead")
            && count("behind")
            && value["dirty"].is_boolean()
            && value["shallow"].is_boolean()
            && stamp(&value["observed_at"]).is_some(),
        "Invalid repository comparison"
    );
    Ok(
        json!({"available":true,"path":text(&value["path"]),"revision":text(&value["revision"]),
        "branch":text(&value["branch"]),"upstream":text(&value["upstream"]),"ahead":value["ahead"],"behind":value["behind"],
        "dirty":value["dirty"],"shallow":value["shallow"],"observed_at":stamp(&value["observed_at"]),
        "target_observed_at":stamp(&value["target_observed_at"]),"error":null}),
    )
}
fn applications_overview(value: &Value) -> Result<Value> {
    // Optional for older workers. Missing inventory is never an empty success.
    if value.is_null() {
        return Ok(Value::Null);
    }
    ensure!(
        value.is_object() && value["available"].is_boolean(),
        "Invalid application inventory"
    );
    if value["available"] == false {
        return Ok(
            json!({"available":false,"active_system":null,"observed_at":null,
            "packages":[],"total":0,"error":text(&value["error"])}),
        );
    }
    let packages = value["packages"]
        .as_array()
        .context("Invalid application packages")?;
    ensure!(
        packages.len() <= 512
            && value["total"]
                .as_u64()
                .is_some_and(|n| n >= packages.len() as u64 && n <= 4096)
            && field(&value["active_system"], 512)
            && stamp(&value["observed_at"]).is_some()
            && packages.iter().all(|p| field(&p["name"], 160)
                && field(&p["scope"], 80)
                && (p["version"].is_null() || field(&p["version"], 80))),
        "Invalid application package metadata"
    );
    let packages: Vec<_> = packages
        .iter()
        .map(|p| {
            Ok(json!({
                "name":p["name"],"scope":p["scope"],"version":p["version"],
                "release":application_release(&p["release"])?
            }))
        })
        .collect::<Result<_>>()?;
    Ok(
        json!({"available":true,"active_system":value["active_system"],
        "observed_at":stamp(&value["observed_at"]),"packages":packages,"total":value["total"],"error":null}),
    )
}
fn application_release(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    ensure!(
        value.is_object()
            && field(&value["source"], 512)
            && value["source"]
                .as_str()
                .is_some_and(|source| source.starts_with("https://"))
            && (value["version"].is_null() || field(&value["version"], 80))
            && (value["checked_at"].is_null() || stamp(&value["checked_at"]).is_some())
            && (value["attempted_at"].is_null() || stamp(&value["attempted_at"]).is_some())
            && value["version"].is_null() == value["checked_at"].is_null()
            && (value["checked_at"].is_null() || !value["attempted_at"].is_null())
            && value["stale"].is_boolean()
            && (value["error"].is_null()
                || value["error"].as_str().is_some_and(|s| s.len() <= 512))
            && (value["stale"] == true
                || (!value["version"].is_null() && value["error"].is_null())),
        "Invalid application release metadata"
    );
    Ok(json!({"version":value["version"],"source":value["source"],
        "checked_at":value["checked_at"],"attempted_at":value["attempted_at"],
        "stale":value["stale"],"error":value["error"]}))
}
fn system_overview(system: &Value, baseline: &Value) -> Value {
    let pending = system["pending_transaction"].as_bool();
    let staged = if pending == Some(false)
        && system["active"].is_string()
        && system["profile"].is_string()
    {
        system["staged_for_next_boot"].as_bool()
    } else {
        None
    };
    let system_errors: Vec<_> = ["active", "booted", "profile", "last_successful_check"]
        .iter()
        .filter_map(|key| text(&system[key]["error"]))
        .collect();
    json!({"active":text(&system["active"]),"booted":text(&system["booted"]),"profile":text(&system["profile"]),
        "staged_for_next_boot":staged,"staged_since": if staged == Some(true) { stamp(&system["staged_since"]).or_else(|| stamp(&baseline["approval"]["approved_at"])) } else { None },
        "pending_transaction":pending,"queued":system["queued"].as_bool(),
        "last_successful_check":stamp(&system["last_successful_check"]),"errors":system_errors})
}
fn baseline_overview(baseline: &Value) -> Value {
    let mut reasons: Vec<_> = baseline["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .take(32)
        .filter_map(text)
        .collect();
    if let Some(reason) = text(&baseline["reason"]) {
        reasons.push(reason);
    }
    json!({"valid":baseline["valid"].as_bool(),"approved_at":stamp(&baseline["approval"]["approved_at"]),
        "revision":text(&baseline["approval"]["revision"]),"reasons":reasons,"error":text(&baseline["error"])})
}
fn tool_overview(tool: &Value) -> Value {
    json!({
        "name": text(&tool["tool"]), "installed_version": text(&tool["installed_version"]),
        "phase": text(&tool["phase"]), "last_successful_check": stamp(&tool["last_successful_check"]),
        "last_activation": stamp(&tool["last_activation"]), "error": text(&tool["error"]),
        "freshness": {
            "latest_version":text(&tool["latest_version"]), "source":text(&tool["latest_release"]["source"]),
            "published_at":text(&tool["latest_release"]["published_at"]), "origin":text(&tool["origin"]),
            "comparison":tool["comparison"].as_str().filter(|value| ["current","newer","ahead","rebuild"].contains(value)),
            "stale":tool["stale"].as_bool(), "held": if tool.get("hold").is_some() { Some(tool["hold"].is_object()) } else { None },
            "hold_since":stamp(&tool["hold"]["since"]), "hold_reason":text(&tool["hold"]["reason"])
        }
    })
}
fn timer_overview(timer: &Value) -> Value {
    let time = |key| timestamp(timer[key].as_u64()? / 1_000_000);
    json!({"unit": text(&timer["unit"]), "next_at": time("next"), "last_at": time("last")})
}
fn normalize(report: &Value) -> Result<Value> {
    let tools = report["tools"]
        .as_array()
        .context("Invalid overview tools")?;
    ensure!(
        report["schema_version"] == 1
            && stamp(&report["observed_at"]).is_some()
            && report["system"].is_object()
            && report["baseline"].is_object()
            && tools.len() <= 32
            && (report["timers"].is_object()
                || report["timers"]
                    .as_array()
                    .is_some_and(|timers| timers.len() <= 64)),
        "Invalid or unsupported worker overview"
    );
    let tools: Vec<_> = tools.iter().map(tool_overview).collect();
    let timers: Vec<_> = report["timers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(timer_overview)
        .collect();
    Ok(json!({
        "schema_version":1,"available":true,"observed_at":stamp(&report["observed_at"]),"error":null,
        "system":system_overview(&report["system"], &report["baseline"]),
        "baseline":baseline_overview(&report["baseline"]),
        "tools":tools,"timers":timers,"timers_error":text(&report["timers"]["error"]),
        "git":git_overview(&report["git"])?,
        "applications":applications_overview(&report["applications"])?
    }))
}
pub(crate) async fn get(params: Value) -> Result<Value> {
    ensure!(
        params.as_object().is_some_and(|params| params.is_empty()),
        "Overview takes no parameters"
    );
    static READERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let read = async {
        // Bound worker processes independently; blocking catalog work keeps its
        // own shared permit even if this request is cancelled.
        let _permit = READERS.acquire().await?;
        let root = super::state_dir();
        let manual = super::manual::directory();
        let catalog =
            super::reader::run(move || Ok(super::history::load_catalog(&root, manual.as_deref())));
        let worker = std::env::var_os("BAR_DAEMON_UPDATE_WORKER")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/run/current-system/sw/bin/update-worker".into());
        let (report, catalog) = tokio::join!(read_worker(&worker), catalog);
        let catalog = catalog.context("Join overview history read")?;
        let mut report = report.unwrap_or_else(|error| json!({"schema_version":1,"available":false,"observed_at":null,
            "error":format!("{error:#}"),"system":null,"baseline":null,"tools":[],"timers":[],"timers_error":null}));
        report["history_available"] = json!(catalog.available);
        report["history_error"] =
            json!((!catalog.errors.is_empty()).then(|| catalog.errors.join("\n")));
        report["issue_total"] = json!(catalog.open_issues.len());
        report["issues"] = json!(&catalog.open_issues[..catalog.open_issues.len().min(MAX_ISSUES)]);
        Ok(json!({"overview": report}))
    };
    tokio::time::timeout(Duration::from_secs(6), read)
        .await
        .context("Update overview read timed out")?
}

#[cfg(test)]
mod tests {
    use super::{get, normalize, read_worker, read_worker_with_timeout};
    use serde_json::{Value, json};
    use std::{path::Path, time::Duration};
    fn report() -> Value {
        json!({"schema_version":1,"observed_at":100,
            "system":{"active":"/nix/store/current","booted":"/nix/store/booted","profile":"/nix/store/staged",
                "staged_for_next_boot":true,"staged_since":80,"pending_transaction":false,"queued":false,"last_successful_check":90},
            "baseline":{"valid":true,"approval":{"approved_at":80,"revision":"rev"}},
            "tools":[{"tool":"pi","installed_version":"1.0","last_successful_check":95,"last_activation":85}],
            "timers":[{"unit":"nixos-update.timer","next":120000000,"last":90000000},{"unit":"disabled.timer","next":u64::MAX}]})
    }
    #[test]
    fn overview_preserves_unknowns_and_does_not_promote_a_transaction_to_staged() {
        let mut value = report();
        let projected = normalize(&value).unwrap();
        assert_eq!(projected["system"]["staged_since"], 80);
        assert_eq!(projected["system"]["last_successful_check"], 90);
        assert_eq!(projected["timers"][0]["next_at"], 120);
        assert!(projected["timers"][1]["next_at"].is_null());
        assert_eq!(projected["tools"][0]["last_activation"], 85);
        value["system"]["pending_transaction"] = json!(true);
        assert!(normalize(&value).unwrap()["system"]["staged_for_next_boot"].is_null());
        value["baseline"] = json!({"valid":null,"error":"Permission denied"});
        assert!(normalize(&value).unwrap()["baseline"]["valid"].is_null());
        for (pointer, invalid) in [
            ("/schema_version", json!(2)),
            ("/observed_at", json!(0)),
            ("/observed_at", json!(253402300800_u64)),
            ("/system", Value::Null),
            ("/baseline", Value::Null),
            ("/tools", json!({})),
            ("/tools", json!(vec![Value::Null; 33])),
            ("/timers", json!(vec![Value::Null; 65])),
        ] {
            let mut candidate = report();
            *candidate.pointer_mut(pointer).unwrap() = invalid;
            assert!(normalize(&candidate).is_err(), "{pointer}");
        }
        for (micros, expected) in [
            (0, None),
            (999999, None),
            (1000000, Some(1)),
            (u64::MAX, None),
            (253402300800000000, None),
            (253402300799000000, Some(253402300799)),
        ] {
            let mut candidate = report();
            candidate["timers"][0]["next"] = json!(micros);
            assert_eq!(
                normalize(&candidate).unwrap()["timers"][0]["next_at"].as_u64(),
                expected
            );
        }
        value["timers"] = json!({"error":"No timer access"});
        value["system"]["pending_transaction"] = json!(false);
        value["system"]["staged_since"] = Value::Null;
        value["baseline"] = json!({"approval":{"approved_at":70}, "reasons":["a"], "reason":"b"});
        let projected = normalize(&value).unwrap();
        assert_eq!(projected["system"]["staged_since"], 70);
        assert_eq!(projected["timers"], json!([]));
        assert_eq!(projected["timers_error"], "No timer access");
        assert_eq!(projected["baseline"]["reasons"], json!(["a", "b"]));
    }
    #[test]
    fn freshness_retains_source_and_uncertainty_without_guessing_from_versions() {
        let mut value = report();
        value["tools"][0] = json!({"tool":"pi","installed_version":"1.0.0","latest_version":"1.0.0",
            "latest_release":{"source":"registry","published_at":"2026-10-01"},"comparison":"rebuild",
            "stale":true,"hold":{"since":90,"reason":"deliberate rollback"}});
        value["git"] = json!({"available":true,"path":"/configuration","revision":"abc","branch":"main",
            "upstream":"origin/main","ahead":2,"behind":4,"dirty":true,"shallow":false,"observed_at":100,"target_observed_at":null});
        let output = normalize(&value).unwrap();
        assert_eq!(output["tools"][0]["freshness"]["comparison"], "rebuild");
        assert_eq!(output["tools"][0]["freshness"]["latest_version"], "1.0.0");
        assert_eq!(output["tools"][0]["freshness"]["held"], true);
        assert_eq!(output["tools"][0]["freshness"]["stale"], true);
        assert!(output["git"]["target_observed_at"].is_null());
        assert_eq!(output["git"]["behind"], 4);
        value["git"]["behind"] = json!(-1);
        assert!(normalize(&value).is_err());
        let old = normalize(&report()).unwrap();
        assert!(old["tools"][0]["freshness"]["comparison"].is_null());
        assert!(old["git"].is_null());
    }
    #[test]
    fn applications_preserve_versions_scope_and_missing_evidence() {
        let mut report = report();
        assert!(normalize(&report).unwrap()["applications"].is_null());
        report["applications"] = json!({"available":true,"active_system":"/nix/store/active",
            "observed_at":100,"total":2,"packages":[
                {"name":"editor","scope":"system","version":"1.2"},
                {"name":"local-tool","scope":"user:test","version":null}]});
        let value = normalize(&report).unwrap();
        assert_eq!(value["applications"]["packages"][0]["version"], "1.2");
        assert!(value["applications"]["packages"][1]["version"].is_null());
        for (pointer, invalid) in [
            ("/total", json!(1)),
            ("/total", json!(4097)),
            ("/active_system", json!("")),
            ("/observed_at", json!(0)),
            ("/packages/0/name", json!("x".repeat(161))),
            ("/packages/0/scope", json!("bad\nlabel")),
            ("/packages/0/version", json!(12)),
            ("/packages/0/version", json!("")),
        ] {
            let mut invalid_report = report.clone();
            *invalid_report["applications"].pointer_mut(pointer).unwrap() = invalid;
            assert!(normalize(&invalid_report).is_err(), "{pointer}");
        }
        report["applications"]["total"] = json!(3); // bounded partial inventory
        assert!(normalize(&report).is_ok());
        report["applications"] = json!({"available":false,"error":"No generation inventory"});
        assert_eq!(
            normalize(&report).unwrap()["applications"]["available"],
            false
        );
    }
    #[test]
    fn application_releases_preserve_sources_staleness_and_failed_last_known_values() {
        let mut report = report();
        let release = json!({"version":"2.0","source":"https://api.github.com/repos/example/app/releases/latest",
            "checked_at":90,"attempted_at":100,"stale":true,"error":"HTTP 429"});
        report["applications"] = json!({"available":true,"active_system":"/nix/store/active",
            "observed_at":100,"total":1,"packages":[{"name":"app","scope":"system","version":"1.0","release":release}]});
        let output = normalize(&report).unwrap();
        assert_eq!(output["applications"]["packages"][0]["release"], release);
        assert_eq!(output["applications"]["packages"][0]["version"], "1.0");
        for (field, value) in [
            ("version", json!(12)),
            ("source", json!("file:///tmp/release")),
            ("source", json!(format!("https://{}", "x".repeat(505)))),
            ("version", json!("bad\nversion")),
            ("error", json!("x".repeat(513))),
            ("checked_at", json!(253402300800_u64)),
            ("attempted_at", json!(0)),
            ("checked_at", Value::Null),
            ("attempted_at", Value::Null),
            ("stale", json!(false)),
        ] {
            let mut invalid = report.clone();
            invalid["applications"]["packages"][0]["release"][field] = value;
            assert!(normalize(&invalid).is_err(), "{field}");
        }
        report["applications"]["packages"][0]["release"] = json!({"version":null,
            "source":"https://api.github.com/repos/example/app/releases/latest","checked_at":null,
            "attempted_at":null,"stale":true,"error":null});
        assert!(normalize(&report).is_ok());
    }
    #[tokio::test]
    async fn reader_runs_only_status_and_rejects_bad_output_and_nonzero_exit() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("worker");
        let shell = std::process::Command::new("sh")
            .args(["-c", "command -v sh"])
            .output()
            .unwrap();
        let shell = String::from_utf8(shell.stdout).unwrap();
        for body in [
            format!("printf '%s' '{}'", report()),
            "printf invalid".into(),
            "printf '%01048577d' 0".into(),
            "exit 1".into(),
        ] {
            std::fs::write(&program, format!("#!{}\n[ \"$1\" = status ] && [ \"$2\" = --json ] && [ $# = 2 ] || exit 9\n{body}\n", shell.trim())).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                read_worker(&program).await.is_ok(),
                body.starts_with("printf '%s'")
            );
        }
        std::fs::write(&program, format!("#!{}\nsleep 30\n", shell.trim())).unwrap();
        let error = read_worker_with_timeout(&program, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(read_worker(Path::new("relative")).await.is_err());
        assert!(get(json!({"command":"apply"})).await.is_err());
    }
}
