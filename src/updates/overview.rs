//! Read-only worker overview. No shell, sudo, caller-supplied command or deployment effect.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncReadExt;

const MAX_REPORT_BYTES: usize = 256 * 1024;
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
            "Worker status exceeds 256 KiB"
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
fn stamp(value: &Value) -> Option<u64> {
    value.as_u64().filter(|at| (1..253402300800).contains(at))
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
    let system = &report["system"];
    let baseline = &report["baseline"];
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
    let tools: Vec<_> = tools.iter().map(|tool| json!({
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
    })).collect();
    let timers: Vec<_> = report["timers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|timer| {
            let time = |key| {
                timer[key]
                    .as_u64()
                    .filter(|at| *at != u64::MAX)
                    .and_then(|at| stamp(&json!(at / 1_000_000)))
            };
            json!({"unit": text(&timer["unit"]), "next_at": time("next"), "last_at": time("last")})
        })
        .collect();
    Ok(json!({
        "schema_version":1,"available":true,"observed_at":stamp(&report["observed_at"]),"error":null,
        "system": {"active":text(&system["active"]),"booted":text(&system["booted"]),"profile":text(&system["profile"]),
            "staged_for_next_boot":staged,"staged_since": if staged == Some(true) { stamp(&system["staged_since"]).or_else(|| stamp(&baseline["approval"]["approved_at"])) } else { None },
            "pending_transaction":pending,"queued":system["queued"].as_bool(),
            "last_successful_check":stamp(&system["last_successful_check"]),"errors":system_errors},
        "baseline":{"valid":baseline["valid"].as_bool(),"approved_at":stamp(&baseline["approval"]["approved_at"]),
            "revision":text(&baseline["approval"]["revision"]),"reasons":reasons,"error":text(&baseline["error"])},
        "tools":tools,"timers":timers,"timers_error":text(&report["timers"]["error"]),
        "git":git_overview(&report["git"])?
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
        report["issues"] = json!(
            catalog
                .open_issues
                .iter()
                .take(MAX_ISSUES)
                .collect::<Vec<_>>()
        );
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
        value["schema_version"] = json!(2);
        assert!(normalize(&value).is_err());
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
            "printf '%0262145d' 0".into(),
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
