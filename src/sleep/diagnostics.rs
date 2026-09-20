//! Read-only evidence, not a substitute for logind's capability/policy checks.
use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SleepDiagnostics {
    pub hardware: Option<String>,
    pub suspend_mode: Option<String>,
    pub supported_suspend_modes: Vec<String>,
    pub disk_swap_available: Option<bool>,
    pub resume_device_configured: Option<bool>,
    pub hibernate_issues: Vec<String>,
    pub guidance: Vec<String>,
}

fn text(root: &Path, relative: &str) -> Option<String> {
    fs::read_to_string(root.join(relative))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub(crate) fn system() -> SleepDiagnostics {
    inspect(Path::new("/"))
}

fn inspect(root: &Path) -> SleepDiagnostics {
    let vendor = text(root, "sys/class/dmi/id/sys_vendor");
    let model = text(root, "sys/class/dmi/id/product_version")
        .or_else(|| text(root, "sys/class/dmi/id/product_name"));
    let mut result = SleepDiagnostics {
        disk_swap_available: text(root, "proc/swaps")
            .as_deref()
            .and_then(disk_swap_available),
        resume_device_configured: text(root, "sys/power/resume")
            .as_deref()
            .and_then(resume_device_configured),
        ..Default::default()
    };
    for mode in text(root, "sys/power/mem_sleep")
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
    {
        let name = mode.trim_matches(['[', ']']).to_owned();
        if mode.starts_with('[') && mode.ends_with(']') {
            result.suspend_mode = Some(name.clone());
        }
        result.supported_suspend_modes.push(name);
    }
    if text(root, "sys/power/state")
        .is_some_and(|states| !states.split_whitespace().any(|state| state == "disk"))
    {
        result
            .hibernate_issues
            .push("The running kernel does not expose disk hibernation.".into());
    }
    if result.disk_swap_available == Some(false) {
        result.hibernate_issues.push("No active disk-backed swap; zram alone cannot store a hibernation image. Configure persistent swap and resume support.".into());
    }
    if result.resume_device_configured == Some(false) {
        // systemd can select swap and record the resume target in EFI at sleep
        // entry. 0:0 alone is NOT proof of unsupported sleep.
        result.guidance.push("No kernel resume device is set. Verify initrd resume support or systemd EFI resume discovery; swapfiles also need a correct resume offset.".into());
    }
    if text(root, "sys/kernel/security/lockdown").is_some_and(|lockdown| {
        lockdown.contains("[integrity]") || lockdown.contains("[confidentiality]")
    }) {
        result.hibernate_issues.push("Kernel lockdown is enabled and may prohibit hibernation. Check the kernel's Secure Boot/hibernation policy; do not disable security automatically.".into());
    }
    let thinkpad = vendor
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("LENOVO"))
        && model
            .as_deref()
            .is_some_and(|s| s.to_ascii_lowercase().contains("thinkpad"));
    if thinkpad && result.supported_suspend_modes == ["s2idle"] {
        result.guidance.push("This ThinkPad exposes s2idle only. Do not force deep/S3; use Lenovo BIOS/EC updates and the current kernel for suspend fixes.".into());
        if model
            .as_deref()
            .is_some_and(|s| s.to_ascii_uppercase().contains("AMD"))
        {
            result.guidance.push("For AMD ThinkPad sleep drain, inspect amd_pmc S0ix residency and wake sources after a controlled suspend/resume test. Do not blindly disable ACPI or USB wake devices.".into());
        }
    }
    result.hardware = match (vendor, model) {
        (Some(vendor), Some(model)) => Some(format!("{vendor} {model}")),
        (vendor, model) => model.or(vendor),
    };
    result
}

fn resume_device_configured(resume: &str) -> Option<bool> {
    let (major, minor) = resume.split_once(':')?;
    let (major, minor) = (major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?);
    Some(major != 0 || minor != 0)
}

fn disk_swap_available(swaps: &str) -> Option<bool> {
    let mut lines = swaps.lines();
    if lines.next()?.split_whitespace().next()? != "Filename" {
        return None;
    }
    let mut disk = false;
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 5 || !matches!(fields[1], "file" | "partition") {
            return None;
        }
        let size = fields[2].parse::<u64>().ok()?;
        let zram = fields[0]
            .strip_prefix("/dev/zram")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
        if size > 0 && !zram {
            disk = true;
        }
    }
    Some(disk)
}

#[cfg(test)]
mod tests {
    use super::inspect;
    use std::{fs, path::Path};

    fn put(root: &Path, path: &str, value: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    #[test]
    fn amd_thinkpad_zram_only_reports_evidence_without_inventing_s3() {
        let root = tempfile::tempdir().unwrap();
        put(root.path(), "sys/class/dmi/id/sys_vendor", "LENOVO");
        put(
            root.path(),
            "sys/class/dmi/id/product_version",
            "ThinkPad P14s Gen 5 AMD",
        );
        put(root.path(), "sys/power/mem_sleep", "[s2idle]");
        put(root.path(), "sys/power/state", "freeze mem disk");
        put(root.path(), "sys/power/resume", "0:0");
        put(
            root.path(),
            "proc/swaps",
            "Filename Type Size Used Priority\n/dev/zram0 partition 32000000 0 5\n",
        );
        let diagnostics = inspect(root.path());
        assert_eq!(diagnostics.suspend_mode.as_deref(), Some("s2idle"));
        assert_eq!(diagnostics.disk_swap_available, Some(false));
        assert_eq!(diagnostics.resume_device_configured, Some(false));
        assert!(
            diagnostics
                .hibernate_issues
                .iter()
                .any(|s| s.contains("zram alone"))
        );
        assert!(
            diagnostics
                .guidance
                .iter()
                .any(|s| s.contains("Do not force deep/S3"))
        );
        assert!(diagnostics.guidance.iter().any(|s| s.contains("amd_pmc")));
    }

    #[test]
    fn disk_swap_with_no_fixed_resume_device_is_not_declared_unsupported() {
        let root = tempfile::tempdir().unwrap();
        put(root.path(), "sys/power/mem_sleep", "s2idle [deep]");
        put(root.path(), "sys/power/state", "freeze mem disk");
        put(root.path(), "sys/power/resume", "0:0");
        put(
            root.path(),
            "proc/swaps",
            "Filename Type Size Used Priority\n/dev/zram0 partition 100 0 5\n/swapfile file 64000000 0 -2\n",
        );
        let diagnostics = inspect(root.path());
        assert_eq!(diagnostics.disk_swap_available, Some(true));
        assert_eq!(diagnostics.suspend_mode.as_deref(), Some("deep"));
        assert!(diagnostics.hibernate_issues.is_empty());
        assert!(diagnostics.guidance.iter().any(|s| s.contains("EFI")));
    }

    #[test]
    fn missing_or_malformed_evidence_stays_unknown_and_lockdown_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let unknown = inspect(root.path());
        assert_eq!(unknown.disk_swap_available, None);
        assert_eq!(unknown.resume_device_configured, None);
        assert!(unknown.hibernate_issues.is_empty());
        put(root.path(), "proc/swaps", "unexpected output");
        put(root.path(), "sys/power/resume", "unknown");
        put(
            root.path(),
            "sys/kernel/security/lockdown",
            "none [integrity] confidentiality",
        );
        let diagnostics = inspect(root.path());
        assert_eq!(diagnostics.disk_swap_available, None);
        assert_eq!(diagnostics.resume_device_configured, None);
        assert!(
            diagnostics
                .hibernate_issues
                .iter()
                .any(|s| s.contains("lockdown"))
        );
    }
}
