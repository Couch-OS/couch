//! Opt-in boot-only transition preceding debug attachment; no original capture.
use crate::{
    adapter::{self, Worker},
    dependencies, enrollment,
    frontend::{Choice, Ui},
    public_inputs::create,
    session::SessionGuard,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_COMPLETED_CHAIN_DEPTH: usize = 4;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    source: PathBuf,
    temporary_boot_sha256: String,
    original_boot_sha256: String,
    snapshot_sha256: String,
    image: PathBuf,
    image_sha256: String,
    metadata: PathBuf,
    metadata_sha256: String,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            cfg!(target_os = "linux"),
            "debug boot transitions currently require Linux"
        );
        self.validate_inputs()
    }
    fn validate_inputs(&self) -> Result<()> {
        ensure!(
            [&self.source, &self.image, &self.metadata]
                .iter()
                .all(|p| p.is_absolute()),
            "transition paths must be absolute"
        );
        for pin in [
            &self.temporary_boot_sha256,
            &self.original_boot_sha256,
            &self.snapshot_sha256,
            &self.image_sha256,
            &self.metadata_sha256,
        ] {
            ensure!(
                pin.len() == 64
                    && pin
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "transition requires explicit SHA-256 pins"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChainedConfig {
    parent: Box<CompletedTransitionConfig>,
    candidate_receipt: PathBuf,
    candidate_receipt_sha256: String,
    source_commit: String,
    stage_base_commit: String,
    probe_sha256: String,
    image: PathBuf,
    image_sha256: String,
    metadata: PathBuf,
    metadata_sha256: String,
}
impl ChainedConfig {
    fn validate(&self) -> Result<()> {
        self.validate_at(0)
    }
    fn validate_at(&self, depth: usize) -> Result<()> {
        ensure!(
            depth < MAX_COMPLETED_CHAIN_DEPTH,
            "completed debug receipt chain exceeds bounded depth"
        );
        self.parent.validate_at(depth + 1)?;
        ensure!(
            [&self.candidate_receipt, &self.image, &self.metadata]
                .iter()
                .all(|p| p.is_absolute()),
            "chained transition paths must be absolute"
        );
        for pin in [
            &self.candidate_receipt_sha256,
            &self.probe_sha256,
            &self.image_sha256,
            &self.metadata_sha256,
        ] {
            ensure!(
                pin.len() == 64
                    && pin
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "chained transition requires explicit SHA-256 pins"
            );
        }
        for source in [&self.source_commit, &self.stage_base_commit] {
            ensure!(
                source.len() == 40
                    && source
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "chained transition requires explicit 40-hex source pins"
            );
        }
        Ok(())
    }
    fn current_boot(&self) -> &str {
        self.parent.current_boot()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TransitionConfig {
    Legacy(Config),
    Chained(ChainedConfig),
}
impl TransitionConfig {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Legacy(config) => config.validate(),
            Self::Chained(config) => config.validate(),
        }
    }
    fn current_boot(&self) -> &str {
        match self {
            Self::Legacy(config) => &config.temporary_boot_sha256,
            Self::Chained(config) => config.current_boot(),
        }
    }
    fn target_boot(&self) -> &str {
        match self {
            Self::Legacy(config) => &config.image_sha256,
            Self::Chained(config) => &config.image_sha256,
        }
    }
    fn admission_payload(&self, bus: u8, ports: &[u8]) -> Result<Value> {
        match self {
            Self::Legacy(config) => serde_json::to_value(config).map_err(Into::into),
            Self::Chained(config) => Ok(json!({"chain": config, "bus": bus, "ports": ports})),
        }
    }
    fn completed(&self, receipt: PathBuf, sha256: String) -> CompletedTransitionConfig {
        match self {
            Self::Legacy(config) => CompletedTransitionConfig::Legacy(ReceiptConfig {
                receipt,
                sha256,
                inputs: config.clone(),
            }),
            Self::Chained(config) => CompletedTransitionConfig::Chained(ChainedReceiptConfig {
                chain: true,
                receipt,
                sha256,
                inputs: Box::new(config.clone()),
            }),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptConfig {
    receipt: PathBuf,
    sha256: String,
    inputs: Config,
}
impl ReceiptConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.receipt.is_absolute()
                && self.sha256.len() == 64
                && self
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "completed transition requires absolute receipt and independent SHA-256 pin"
        );
        self.inputs.validate_inputs()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChainedReceiptConfig {
    chain: bool,
    receipt: PathBuf,
    sha256: String,
    inputs: Box<ChainedConfig>,
}
impl ChainedReceiptConfig {
    fn validate_at(&self, depth: usize) -> Result<()> {
        ensure!(self.chain, "chained completed receipt marker required");
        ensure!(
            self.receipt.is_absolute()
                && self.sha256.len() == 64
                && self
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "chained completed transition requires absolute receipt and SHA-256 pin"
        );
        self.inputs.validate_at(depth)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CompletedTransitionConfig {
    Legacy(ReceiptConfig),
    Chained(ChainedReceiptConfig),
}
impl CompletedTransitionConfig {
    pub fn validate(&self) -> Result<()> {
        self.validate_at(0)
    }
    fn validate_at(&self, depth: usize) -> Result<()> {
        match self {
            Self::Legacy(config) => config.validate(),
            Self::Chained(config) => config.validate_at(depth),
        }
    }
    fn current_boot(&self) -> &str {
        match self {
            Self::Legacy(config) => &config.inputs.image_sha256,
            Self::Chained(config) => &config.inputs.image_sha256,
        }
    }
    fn payload(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(Into::into)
    }
    fn sha256(&self) -> &str {
        match self {
            Self::Legacy(config) => &config.sha256,
            Self::Chained(config) => &config.sha256,
        }
    }
}

pub fn materialize(session: &SessionGuard) -> Result<PathBuf> {
    let adapter = adapter::materialize(session)?;
    let directory = adapter.parent().context("missing worker directory")?;
    let release = session.path().join("release");
    fs::create_dir(&release)?;
    for (root, name, bytes) in [
        (
            directory,
            "wifi_debug_transition_worker.py",
            include_bytes!("../../wifi_debug_transition_worker.py").as_slice(),
        ),
        (
            directory,
            "wifi_debug_transition.py",
            include_bytes!("../../wifi_debug_transition.py").as_slice(),
        ),
        (
            directory,
            "recover_native_bootstrap.py",
            include_bytes!("../../recover_native_bootstrap.py").as_slice(),
        ),
        (
            directory,
            "enroll_android.py",
            include_bytes!("../../enroll_android.py").as_slice(),
        ),
        (
            directory,
            "capture_readonly.py",
            include_bytes!("../../capture_readonly.py").as_slice(),
        ),
        (
            release.as_path(),
            "prepare_official_inputs.py",
            include_bytes!("../../../release/prepare_official_inputs.py").as_slice(),
        ),
        (
            release.as_path(),
            "official_runtime.py",
            include_bytes!("../../../release/official_runtime.py").as_slice(),
        ),
        (
            release.as_path(),
            "private_vendor.py",
            include_bytes!("../../../release/private_vendor.py").as_slice(),
        ),
        (
            release.as_path(),
            "ha100_official_runtime.json",
            include_bytes!("../../../release/ha100_official_runtime.json").as_slice(),
        ),
    ] {
        let mut file = create(&root.join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    Ok(directory.join("wifi_debug_transition_worker.py"))
}

fn rpc(
    worker: &mut Worker,
    ui: &mut Ui,
    operation: &str,
    payload: Value,
    seconds: u64,
) -> Result<Value> {
    worker.operation(Duration::from_secs(seconds), |w| {
        w.send(&json!({"op":operation,"payload":payload}))?;
        // Reuse only the bounded event decoder; never enrollment/capture logic.
        let event = enrollment::event(w, ui, 1)?;
        ensure!(
            event["event"] == operation,
            "unexpected transition response"
        );
        Ok(event["result"].clone())
    })
}

pub struct Environment<'a> {
    pub python: &'a Path,
    pub runtime: &'a Path,
    pub libusb: &'a Path,
    pub script: &'a Path,
}

pub fn run(
    ui: &mut Ui,
    session: &mut SessionGuard,
    config: &TransitionConfig,
    environment: Environment<'_>,
    bus: u8,
    ports: &[u8],
) -> Result<Option<CompletedTransitionConfig>> {
    let Environment {
        python,
        runtime,
        libusb,
        script,
    } = environment;
    config.validate()?;
    let _privilege = crate::adapter::Privilege::acquire()?;
    let mut command = crate::adapter::mtk_worker(python);
    command
        .args(["-I", "-B"])
        .arg(dependencies::python_path(script)?)
        .arg("--events-stdio")
        .current_dir(session.path());
    let mut worker = Worker::spawn(&mut command)?;
    ensure!(
        rpc(&mut worker, ui, "transition_check", Value::Null, 10)?
            == json!({"device_access":false,"ready":true}),
        "transition worker failed offline check"
    );
    let admitted = rpc(
        &mut worker,
        ui,
        "transition_admit",
        config.admission_payload(bus, ports)?,
        120,
    )?;
    ensure!(
        admitted == json!({"admitted":true,"device_access":false}),
        "invalid offline admission result"
    );
    session.checkpoint(&json!({"event":"debug_transition_inputs_verified","pins":config}))?;
    let choice = ui.choose("Start the dedicated Wi-Fi debug stage", &format!(
        "Verified retained originals and failed-session journal.\nUSB bus {bus}, ports {ports:?}.\nCurrent debug boot: {}\nDebug boot: {}\nOnly boot will be written and independently verified. Existing originals remain the baseline.",
        config.current_boot(), config.target_boot()), &[
        Choice {label:"Cancel".into(),detail:"No USB transition".into()},
        Choice {label:"Perform one debug boot transition".into(),detail:"Wait up to 180 seconds for this remote in preloader mode".into()},
    ])?;
    if choice == 0 {
        rpc(&mut worker, ui, "transition_close", Value::Null, 10)?;
        return Ok(None);
    }
    ui.progress(
        1,
        "Waiting for the selected preloader; then verifying current boot and retained identity",
        0,
        0,
    )?;
    session.checkpoint(&json!({"event":"debug_transition_requested","bus":bus,"ports":ports}))?;
    let result = rpc(
        &mut worker,
        ui,
        "transition_execute",
        json!({"bus":bus,"ports":ports,
        "runtime":dependencies::python_path(runtime)?,"libusb":dependencies::python_path(libusb)?,
        "output":dependencies::python_path(&session.path().join("transition"))?}),
        900,
    )?;
    ensure!(
        result
            == json!({"written":["boot"],"boot_sha256":config.target_boot(),
        "verified":true,"restart_requested":false}),
        "unverified debug transition result"
    );
    session.checkpoint(&json!({"event":"debug_boot_readback_verified","result":result}))?;
    let boot = rpc(&mut worker, ui, "transition_boot", Value::Null, 30)?;
    ensure!(
        boot["acknowledged"] == true,
        "debug boot request was not acknowledged"
    );
    let receipt = config.completed(
        session.path().join("transition/completed.json"),
        boot["receipt_sha256"]
            .as_str()
            .context("missing completed receipt pin")?
            .into(),
    );
    receipt.validate()?;
    session
        .checkpoint(&json!({"event":"debug_boot_acknowledged", "completed_transition":receipt}))?;
    Ok(Some(receipt))
}

pub fn validate_receipt(
    ui: &mut Ui,
    session: &mut SessionGuard,
    config: &CompletedTransitionConfig,
    python: &Path,
    script: &Path,
    bus: u8,
    ports: &[u8],
) -> Result<()> {
    config.validate()?;
    let _privilege = crate::adapter::Privilege::acquire()?;
    let mut command = crate::adapter::mtk_worker(python);
    command
        .args(["-I", "-B"])
        .arg(dependencies::python_path(script)?)
        .arg("--events-stdio")
        .current_dir(session.path());
    let mut worker = Worker::spawn(&mut command)?;
    let result = rpc(
        &mut worker,
        ui,
        "transition_validate_receipt",
        json!({"config":config.payload()?,"bus":bus,"ports":ports}),
        120,
    )?;
    ensure!(
        result == json!({"validated":true,"device_access":false,"receipt_sha256":config.sha256()}),
        "completed transition receipt was not verified"
    );
    rpc(&mut worker, ui, "transition_close", Value::Null, 10)?;
    session.checkpoint(
        &json!({"event":"debug_completed_transition_verified", "completed_transition":config}),
    )?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;
    #[test]
    fn transition_configuration_requires_explicit_pins_and_absolute_paths() {
        let valid = json!({"source":"/retained", "temporary_boot_sha256":"a".repeat(64),
            "original_boot_sha256":"b".repeat(64), "snapshot_sha256":"c".repeat(64),
            "image":"/debug.img", "image_sha256":"d".repeat(64),
            "metadata":"/debug.json", "metadata_sha256":"e".repeat(64)});
        assert_eq!(
            serde_json::from_value::<Config>(valid.clone())
                .unwrap()
                .validate()
                .is_ok(),
            cfg!(target_os = "linux")
        );
        for (key, replacement) in [
            ("source", "relative-session"),
            ("image", "relative.img"),
            ("metadata", "relative.json"),
            ("original_boot_sha256", "ea75"),
            ("temporary_boot_sha256", "222d"),
            ("snapshot_sha256", ""),
            ("image_sha256", "AUTO"),
            ("metadata_sha256", "AUTO"),
        ] {
            let mut value = valid.clone();
            value[key] = json!(replacement);
            assert!(serde_json::from_value::<Config>(value)
                .unwrap()
                .validate()
                .is_err());
        }
        let mut value = valid;
        value["capture_originals"] = json!(true);
        assert!(serde_json::from_value::<Config>(value).is_err());
    }
    #[test]
    fn chained_configuration_keeps_completed_parent_shape_and_bounds_depth() {
        let legacy = json!({"receipt":"/legacy/completed.json", "sha256":"a".repeat(64),
            "inputs":{"source":"/retained", "temporary_boot_sha256":"b".repeat(64),
            "original_boot_sha256":"c".repeat(64), "snapshot_sha256":"d".repeat(64),
            "image":"/b13.img", "image_sha256":"e".repeat(64),
            "metadata":"/b13.json", "metadata_sha256":"f".repeat(64)}});
        let first = json!({"parent":legacy, "candidate_receipt":"/first/receipt.json",
            "candidate_receipt_sha256":"1".repeat(64), "source_commit":"a".repeat(40),
            "stage_base_commit":"b".repeat(40), "probe_sha256":"2".repeat(64),
            "image":"/first.img", "image_sha256":"3".repeat(64),
            "metadata":"/first.json", "metadata_sha256":"4".repeat(64)});
        let completed_first = json!({"chain":true, "receipt":"/first/completed.json",
            "sha256":"5".repeat(64), "inputs":first});
        let second = json!({"parent":completed_first, "candidate_receipt":"/second/receipt.json",
            "candidate_receipt_sha256":"6".repeat(64), "source_commit":"c".repeat(40),
            "stage_base_commit":"d".repeat(40), "probe_sha256":"7".repeat(64),
            "image":"/second.img", "image_sha256":"8".repeat(64),
            "metadata":"/second.json", "metadata_sha256":"9".repeat(64)});
        let parsed = serde_json::from_value::<TransitionConfig>(second.clone()).unwrap();
        assert!(parsed.validate().is_ok());
        assert_eq!(parsed.current_boot(), "3".repeat(64));
        let payload = parsed.admission_payload(1, &[1]).unwrap();
        assert_eq!(payload["chain"]["parent"]["chain"], true);
        assert_eq!(
            payload["chain"]["parent"]["inputs"]["image_sha256"],
            "3".repeat(64)
        );

        let mut too_deep = second;
        for number in 0..MAX_COMPLETED_CHAIN_DEPTH {
            too_deep = json!({"parent":{"chain":true, "receipt":format!("/nested-{number}.json"),
                "sha256":"a".repeat(64), "inputs":too_deep},
                "candidate_receipt":format!("/candidate-{number}.json"),
                "candidate_receipt_sha256":"b".repeat(64), "source_commit":"c".repeat(40),
                "stage_base_commit":"d".repeat(40), "probe_sha256":"e".repeat(64),
                "image":format!("/image-{number}.img"), "image_sha256":"f".repeat(64),
                "metadata":format!("/metadata-{number}.json"), "metadata_sha256":"0".repeat(64)});
        }
        let parsed = serde_json::from_value::<TransitionConfig>(too_deep).unwrap();
        assert!(parsed.validate().is_err());
    }
    #[test]
    fn embedded_transition_worker_imports_and_checks_without_usb() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let session = SessionGuard::create(&root.path().join("run")).unwrap();
        let script = materialize(&session).unwrap();
        let mut command = Command::new("python3");
        command.args(["-I", "-B"]).arg(script).arg("--events-stdio");
        let mut worker = Worker::spawn(&mut command).unwrap();
        worker.operation(Duration::from_secs(10), |w| {
            w.send(&json!({"op":"transition_check","payload":null}))?;
            ensure!(w.event()? == json!({"event":"transition_check","result":{"device_access":false,"ready":true}}));
            w.send(&json!({"op":"transition_close","payload":null}))?;
            ensure!(w.event()?["result"]["closed"] == true);
            Ok(())
        }).unwrap();
    }
}
