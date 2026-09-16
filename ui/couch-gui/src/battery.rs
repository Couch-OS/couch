//! Battery percentage is the kernel's estimate. External power and charge state
//! are separate: a full battery or inhibited charger can still power the dock.
use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Charging,
    Full,
    Discharging,
    NotCharging,
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: Option<i32>,
    pub status: Status,
    pub external_power: bool,
}

fn percent(value: Option<&str>) -> Option<i32> {
    value?.trim().parse().ok().filter(|n| (0..=100).contains(n))
}

impl Battery {
    fn parse(capacity: Option<&str>, status: Option<&str>, online: &[Option<bool>]) -> Self {
        let mut status = match status.map(str::trim) {
            Some("Charging") => Status::Charging,
            Some("Full") => Status::Full,
            Some("Discharging") => Status::Discharging,
            Some("Not charging" | "Cmd discharging") => Status::NotCharging,
            _ => Status::Unknown,
        };
        let external_power = if online.iter().any(Option::is_some) {
            online.contains(&Some(true))
        } else {
            // Older kernels can omit the supply nodes. Only then use status.
            matches!(status, Status::Charging | Status::Full)
        };
        if !external_power && matches!(status, Status::Charging | Status::Full) {
            status = Status::Discharging;
        }
        Self {
            percent: percent(capacity),
            status,
            external_power,
        }
    }

    pub fn read(root: &Path) -> Self {
        let capacity = fs::read_to_string(root.join("battery/capacity")).ok();
        let status = fs::read_to_string(root.join("battery/status")).ok();
        let online = ["usb", "ac", "wireless"].map(|supply| {
            match fs::read_to_string(root.join(supply).join("online"))
                .ok()
                .as_deref()
                .map(str::trim)
            {
                Some("1") => Some(true),
                Some("0") => Some(false),
                _ => None,
            }
        });
        Self::parse(capacity.as_deref(), status.as_deref(), &online)
    }

    pub fn charging(&self) -> bool {
        self.external_power && self.status == Status::Charging
    }

    pub fn caption(&self) -> String {
        let label = match (self.external_power, self.status) {
            (true, Status::Charging) => "Charging",
            (true, Status::Full) => "Full",
            (true, _) => "Plugged in",
            _ => "On battery",
        };
        match self.percent {
            Some(percent) => format!("{label} · {percent}%"),
            None => format!("{label} · Battery unavailable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_invalid_capacity_never_becomes_zero_or_full() {
        for raw in [None, Some(""), Some("oops"), Some("-1"), Some("101")] {
            let b = Battery::parse(raw, Some("Charging"), &[Some(true)]);
            assert_eq!(b.percent, None);
            assert!(b.external_power);
            assert_eq!(b.caption(), "Charging · Battery unavailable");
        }
        assert_eq!(percent(Some("0\n")), Some(0));
        assert_eq!(percent(Some("100\n")), Some(100));
    }

    #[test]
    fn full_and_inhibited_charging_keep_dock_power_without_saying_charging() {
        let full = Battery::parse(Some("100"), Some("Full"), &[Some(false), Some(true)]);
        assert!(full.external_power);
        assert!(!full.charging());
        assert_eq!(full.caption(), "Full · 100%");
        let stopped = Battery::parse(Some("83"), Some("Not charging"), &[Some(true)]);
        assert!(stopped.external_power);
        assert!(!stopped.charging());
        assert_eq!(stopped.caption(), "Plugged in · 83%");
    }

    #[test]
    fn supply_disconnect_overrides_stale_full_status() {
        let b = Battery::parse(Some("100"), Some("Full"), &[Some(false), Some(false)]);
        assert!(!b.external_power);
        assert_eq!(b.status, Status::Discharging);
        assert!(!b.charging());
        assert_eq!(b.caption(), "On battery · 100%");
    }

    #[test]
    fn legacy_status_fallback_and_missing_gauge_are_supported() {
        let b = Battery::parse(None, None, &[None, None]);
        assert_eq!(b.percent, None);
        assert!(!b.external_power);
        assert_eq!(b.status, Status::Unknown);
        let b = Battery::parse(Some("42"), Some("Charging"), &[None]);
        assert!(b.external_power);
        assert!(b.charging());
    }
}
