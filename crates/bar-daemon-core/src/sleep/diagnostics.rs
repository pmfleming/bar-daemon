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
    result.hibernate_issues = messages(&[
        (
            text(root, "sys/power/state")
                .is_some_and(|states| !states.split_whitespace().any(|state| state == "disk")),
            "The running kernel does not expose disk hibernation.",
        ),
        (
            result.disk_swap_available == Some(false),
            "No active disk-backed swap; zram alone cannot store a hibernation image. Configure persistent swap and resume support.",
        ),
        (
            text(root, "sys/kernel/security/lockdown").is_some_and(|lockdown| {
                lockdown.contains("[integrity]") || lockdown.contains("[confidentiality]")
            }),
            "Kernel lockdown is enabled and may prohibit hibernation. Check the kernel's Secure Boot/hibernation policy; do not disable security automatically.",
        ),
    ]);
    let normalized_model = model.as_deref().unwrap_or_default().to_ascii_lowercase();
    let thinkpad_s2idle = vendor
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("LENOVO"))
        && normalized_model.contains("thinkpad")
        && result.supported_suspend_modes == ["s2idle"];
    result.guidance = messages(&[
        // systemd may record the resume target in EFI at sleep entry: 0:0 is
        // guidance, not proof of unsupported sleep. Unknown evidence stays unknown.
        (
            result.resume_device_configured == Some(false),
            "No kernel resume device is set. Verify initrd resume support or systemd EFI resume discovery; swapfiles also need a correct resume offset.",
        ),
        (
            thinkpad_s2idle,
            "This ThinkPad exposes s2idle only. Do not force deep/S3; use Lenovo BIOS/EC updates and the current kernel for suspend fixes.",
        ),
        (
            thinkpad_s2idle && normalized_model.contains("amd"),
            "For AMD ThinkPad sleep drain, inspect amd_pmc S0ix residency and wake sources after a controlled suspend/resume test. Do not blindly disable ACPI or USB wake devices.",
        ),
    ]);
    result.hardware = match (vendor, model) {
        (Some(vendor), Some(model)) => Some(format!("{vendor} {model}")),
        (vendor, model) => model.or(vendor),
    };
    result
}

fn messages(rules: &[(bool, &str)]) -> Vec<String> {
    rules
        .iter()
        .filter(|(applies, _)| *applies)
        .map(|(_, message)| (*message).into())
        .collect()
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
        let mut fields = line.split_whitespace();
        let [
            Some(device),
            Some("file" | "partition"),
            Some(size),
            Some(_),
            Some(_),
            None,
        ] = std::array::from_fn(|_| fields.next())
        else {
            return None;
        };
        let size = size.parse::<u64>().ok()?;
        let zram = device
            .strip_prefix("/dev/zram")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
        disk |= size > 0 && !zram;
    }
    Some(disk)
}
