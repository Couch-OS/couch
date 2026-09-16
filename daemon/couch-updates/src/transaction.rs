//! A durable selection prevents a half-downloaded pair becoming installable
//! after a service restart. Existing signed payload formats remain unchanged.
use crate::{baseline, boot, staging, Manifest, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

const PLAN: &str = "updates/transaction.json";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub runtime: Option<Manifest>,
    pub boot: Option<Manifest>,
    pub ready: bool,
    #[serde(default)]
    finished: bool,
}
impl Plan {
    pub fn new(runtime: Option<Manifest>, boot: Option<Manifest>) -> Result<Self> {
        if runtime.is_none() && boot.is_none() {
            return Err("No update selected".into());
        }
        if runtime.as_ref().is_some_and(|m| m.kind != "runtime")
            || boot.as_ref().is_some_and(|m| m.kind != "boot")
        {
            return Err("Wrong payload type in update selection".into());
        }
        if let (Some(r), Some(b)) = (&runtime, &boot) {
            if r.version != b.version || r.required_os_baseline != b.required_os_baseline {
                return Err(
                    "Runtime and boot payload must belong to the same release and OS baseline"
                        .into(),
                );
            }
        }
        Ok(Self {
            runtime,
            boot,
            ready: false,
            finished: false,
        })
    }
    pub fn primary(&self) -> &Manifest {
        self.runtime.as_ref().or(self.boot.as_ref()).unwrap()
    }
    pub fn kind(&self) -> &str {
        if self.runtime.is_some() && self.boot.is_some() {
            "combined"
        } else {
            &self.primary().kind
        }
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        staging::atomic(&root.join(PLAN), &serde_json::to_vec(self).unwrap())
    }
    pub fn load(root: &Path) -> Result<Option<Self>> {
        let bytes = match fs::read(root.join(PLAN)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Could not read update transaction".into()),
        };
        let plan: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid update transaction")?;
        Self::new(plan.runtime.clone(), plan.boot.clone())?;
        Ok(Some(plan))
    }
    pub fn recover(root: &Path) -> Result<Option<Self>> {
        let plan = Self::load(root)?;
        if plan.as_ref().is_some_and(|p| p.finished) {
            cleanup(root)?;
            return Ok(None);
        }
        Ok(plan)
    }
    pub fn stage(&mut self, root: &Path, progress: impl Fn(&str)) -> Result<()> {
        // Persist the incomplete state BEFORE either individual stage marker.
        self.ready = false;
        self.save(root)?;
        clear(root, "runtime/staged")?;
        clear(root, "boot/staged")?;
        for m in [&self.runtime, &self.boot].into_iter().flatten() {
            baseline::check(root, m)?;
            if m.kind == "runtime" {
                staging::inventory(m)?;
            } else {
                boot::inventory(m)?;
            }
        }
        if let Some(m) = &self.runtime {
            staging::stage(root, m, &progress)?;
        }
        if let Some(m) = &self.boot {
            progress("downloading");
            boot::stage(root, m, &progress)?;
        }
        self.ready = true;
        self.save(root)
    }
    pub fn can_resume(&self, root: &Path) -> bool {
        self.ready
            && [&self.runtime, &self.boot].into_iter().flatten().all(|m| {
                let staged = fs::read_to_string(root.join(format!("{}/staged", m.kind))).ok();
                staged.as_deref() == Some(&m.sha256)
                    || (m.kind == "runtime"
                        && fs::read_link(root.join("runtime/current")).ok()
                            == Some(format!("slots/{}", m.sha256).into()))
            })
    }
    pub fn activate(&self, root: &Path, device: &Path) -> Result<()> {
        if self.finished {
            return cleanup(root);
        }
        if !self.ready {
            return Err("Update download was interrupted; check and download again".into());
        }
        // Recheck BOTH payloads before the first partition write. Retain the
        // boot stage marker until the runtime switch has also completed so a
        // failed switch can be retried without losing the saved boot image.
        let switched = self.runtime.as_ref().is_some_and(|m| {
            fs::read_link(root.join("runtime/current")).ok()
                == Some(format!("slots/{}", m.sha256).into())
        });
        if let Some(m) = &self.runtime {
            if !switched {
                same(m, &staging::validate_staged(root)?)?;
                if root.join("runtime/pending").exists() {
                    return Err("An update already awaits boot confirmation".into());
                }
                // Catch a malformed current pointer before touching boot.
                match fs::read_link(root.join("runtime/current")) {
                    Ok(p)
                        if p.to_str()
                            .and_then(|p| p.strip_prefix("slots/"))
                            .is_some_and(staging::id) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err("Invalid active runtime pointer".into()),
                }
                if fs::symlink_metadata(root.join("runtime/next"))
                    .is_ok_and(|m| !m.file_type().is_symlink())
                {
                    return Err("Unexpected runtime switch path".into());
                }
            }
        }
        if let Some(m) = &self.boot {
            same(m, &boot::validate_staged(root)?)?;
            boot::activate_staged(root, device, false)?;
        }
        if self.runtime.is_some() && !switched {
            staging::activate(root)?;
        }
        let mut completed = self.clone();
        completed.finished = true;
        completed.save(root)?;
        cleanup(root)?;
        Ok(())
    }
}
fn cleanup(root: &Path) -> Result<()> {
    clear(root, crate::PENDING)?;
    clear(root, "boot/staged")?;
    clear(root, "runtime/staged")?;
    clear(root, PLAN)
}
fn same(expected: &Manifest, staged: &Manifest) -> Result<()> {
    if serde_json::to_vec(expected).unwrap() != serde_json::to_vec(staged).unwrap() {
        return Err("Staged payload does not match the selected update".into());
    }
    Ok(())
}
fn clear(root: &Path, name: &str) -> Result<()> {
    let path = root.join(name);
    match fs::remove_file(&path) {
        Ok(()) => fs::File::open(path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|_| "Could not flush update state".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Could not clear update state".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{activate_with, boot::tests as fixture, Updater};

    fn pair() -> (std::path::PathBuf, std::path::PathBuf, Plan) {
        let (root, runtime, bytes) = staging::tests::fixture();
        staging::unpack(&root, &runtime, &bytes).unwrap();
        staging::atomic(&root.join("runtime/staged"), runtime.sha256.as_bytes()).unwrap();
        let device = root.join("boot-device");
        fs::write(
            &device,
            fixture::image(&fixture::zimage(1, 100), &fixture::ramdisk(2, 100)),
        )
        .unwrap();
        let z = fixture::zimage(3, 100);
        let r = fixture::ramdisk(4, 100);
        let boot = fixture::manifest(&z, &r);
        fixture::stage_fixture(&root, &boot, &z, &r);
        let mut plan = Plan::new(Some(runtime), Some(boot)).unwrap();
        plan.ready = true;
        plan.save(&root).unwrap();
        (root, device, plan)
    }

    #[test]
    fn completed_activation_can_retry_cleanup_without_staged_payloads() {
        let (root, device, mut plan) = pair();
        plan.finished = true;
        plan.save(&root).unwrap();
        fs::remove_file(root.join("boot/staged")).unwrap();
        fs::remove_file(root.join("runtime/staged")).unwrap();
        activate_with(&root, &device).unwrap();
        assert!(Plan::load(&root).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_rolled_back_runtime_does_not_trap_the_ui_in_ready() {
        let (root, device, _) = pair();
        boot::activate_staged(&root, &device, false).unwrap();
        staging::activate(&root).unwrap();
        // Model the stable bootstrap rejecting this candidate and selecting base.
        fs::remove_file(root.join("runtime/current")).unwrap();
        fs::remove_file(root.join("runtime/pending")).unwrap();
        let updater = Updater::with_boot_device(root.clone(), device);
        assert!(!updater.ready());
        assert_eq!(updater.status().phase, "error");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_activation_installs_both_and_preserves_runtime_health_journal_and_boot_backup() {
        let (root, device, plan) = pair();
        let original = fs::read(&device).unwrap();
        let restarted = Updater::with_boot_device(root.clone(), device.clone()).status();
        assert_eq!(restarted.kind, "combined");
        assert_eq!(restarted.available.as_deref(), Some("v1.2.3"));
        assert_eq!(restarted.steps, 1);
        assert_eq!(restarted.phase, "ready");
        activate_with(&root, &device).unwrap();
        assert!(boot::installed(&device, plan.boot.as_ref().unwrap()).unwrap());
        assert_eq!(
            fs::read_link(root.join("runtime/current")).unwrap(),
            std::path::PathBuf::from(format!("slots/{}", plan.runtime.unwrap().sha256))
        );
        assert!(fs::read_to_string(root.join("runtime/pending"))
            .unwrap()
            .starts_with("base "));
        assert_eq!(fs::read(root.join("boot/previous.img")).unwrap(), original);
        assert!(Plan::load(&root).unwrap().is_none());
        assert!(!root.join("boot/staged").exists());
        assert!(!root.join("runtime/staged").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_interrupted_pair_download_never_becomes_ready_after_restart() {
        let (root, device, mut plan) = pair();
        let original = fs::read(&device).unwrap();
        plan.ready = false;
        plan.save(&root).unwrap();
        fs::remove_file(root.join("boot/staged")).unwrap();
        let updater = Updater::with_boot_device(root.clone(), device.clone());
        assert!(!updater.ready());
        assert_eq!(updater.status().phase, "error");
        assert!(activate_with(&root, &device).is_err());
        assert_eq!(fs::read(&device).unwrap(), original);
        assert!(!root.join("runtime/current").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tampered_or_missing_either_half_prevents_all_activation() {
        for kind in ["runtime", "boot"] {
            let (root, device, _) = pair();
            let original = fs::read(&device).unwrap();
            fs::remove_file(root.join(format!("{kind}/staged"))).unwrap();
            assert!(activate_with(&root, &device).is_err());
            assert_eq!(fs::read(&device).unwrap(), original);
            assert!(!root.join("runtime/current").exists());
            fs::remove_dir_all(root).unwrap();
        }
        let (root, device, plan) = pair();
        let original = fs::read(&device).unwrap();
        fs::write(
            root.join("runtime/slots")
                .join(&plan.runtime.unwrap().sha256)
                .join("couch-system"),
            b"bad",
        )
        .unwrap();
        assert!(activate_with(&root, &device).is_err());
        assert_eq!(fs::read(&device).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interruption_after_boot_write_retries_without_replacing_original_backup() {
        let (root, device, _) = pair();
        let original = fs::read(&device).unwrap();
        boot::activate_staged(&root, &device, false).unwrap();
        activate_with(&root, &device).unwrap();
        assert_eq!(fs::read(root.join("boot/previous.img")).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interruption_after_runtime_switch_finishes_without_a_second_activation() {
        let (root, device, _) = pair();
        boot::activate_staged(&root, &device, false).unwrap();
        staging::activate(&root).unwrap();
        let journal = fs::read(root.join("runtime/pending")).unwrap();
        activate_with(&root, &device).unwrap();
        assert_eq!(fs::read(root.join("runtime/pending")).unwrap(), journal);
        assert!(Plan::load(&root).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failure_to_write_boot_never_switches_runtime() {
        let (root, device, _) = pair();
        fs::write(&device, vec![0; boot::PARTITION]).unwrap();
        assert!(activate_with(&root, &device).is_err());
        assert!(!root.join("runtime/current").exists());
        assert!(root.join("runtime/staged").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mismatched_release_or_baseline_cannot_form_a_pair() {
        let (root, runtime, _) = staging::tests::fixture();
        let mut boot = fixture::manifest(&fixture::zimage(1, 100), &fixture::ramdisk(2, 100));
        boot.version = "v1.2.4".into();
        assert!(Plan::new(Some(runtime.clone()), Some(boot.clone())).is_err());
        boot.version = runtime.version.clone();
        boot.required_os_baseline = Some("another-baseline".into());
        assert!(Plan::new(Some(runtime), Some(boot)).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_transaction_cleanup_survives_a_service_restart() {
        let (root, device, mut plan) = pair();
        plan.finished = true;
        plan.save(&root).unwrap();
        fs::remove_file(root.join("boot/staged")).unwrap();
        let updater = Updater::with_boot_device(root.clone(), device);
        assert_eq!(updater.status().phase, "idle");
        assert!(!root.join("runtime/staged").exists());
        assert!(!root.join(PLAN).exists());
        fs::remove_dir_all(root).unwrap();
    }
}
