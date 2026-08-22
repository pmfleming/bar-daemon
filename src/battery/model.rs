#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct NativeBattery {
    pub identity: BatteryIdentity,
    pub telemetry: BatteryTelemetry,
    pub protection: NativeProtection,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct BatteryIdentity {
    pub id: String,
    pub vendor: String,
    pub model: String,
    pub serial: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct BatteryTelemetry {
    pub present: bool,
    pub status: String,
    pub percentage: u8,
    pub energy_now_uwh: Option<u64>,
    pub energy_full_uwh: Option<u64>,
    pub energy_full_design_uwh: Option<u64>,
    pub power_uw: u64,
    pub voltage_uv: Option<u64>,
    pub cycles: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct NativeProtection {
    pub start_threshold: Option<u8>,
    pub end_threshold: Option<u8>,
    pub charge_behaviour: Option<String>,
    pub available_behaviours: Vec<String>,
}

impl NativeBattery {
    pub(crate) fn charging(&self) -> bool {
        self.telemetry.status.eq_ignore_ascii_case("charging")
    }

    pub(crate) fn discharging(&self) -> bool {
        self.telemetry.status.eq_ignore_ascii_case("discharging")
    }

    pub(crate) fn fully_charged(&self) -> bool {
        self.telemetry.status.eq_ignore_ascii_case("full")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct NativeSnapshot {
    pub batteries: Vec<NativeBattery>,
    pub plugged: bool,
}
