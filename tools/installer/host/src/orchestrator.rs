//! Native fresh-Android installer. Every mutation follows durable enrollment;
//! the Python worker is restricted to MTK and bounded USB setup transport.
use crate::{
    adapter::{self, UsbLease, Worker},
    android, assembly, dependencies, enrollment, enrollment_sources,
    frontend::{Choice, Ui},
    network,
    public_inputs::{self, create, decode, digest, hex},
    saved_enrollment,
    session::{self, Phase, SessionGuard},
    stage::Channel,
    stage_tls,
    transaction::{self, OriginalOs},
    vendor_transfer,
};
use anyhow::{ensure, Context, Result};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
fn random() -> Result<[u8; 32]> {
    let mut value = [0; 32];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
    Ok(value)
}
fn choice(label: &str, detail: &str) -> Choice {
    Choice {
        label: label.into(),
        detail: detail.into(),
    }
}
fn state_root() -> Result<PathBuf> {
    #[cfg(windows)]
    let path = PathBuf::from(
        std::env::var_os("LOCALAPPDATA").context("missing local application data directory")?,
    )
    .join("CouchInstaller");
    #[cfg(not(windows))]
    let path = PathBuf::from(std::env::var_os("HOME").context("missing home directory")?)
        .join(".couch-installer");
    if !path.exists() {
        session::create_private_parent(&path)?;
    }
    Ok(path)
}
fn simple(
    worker: &mut Worker,
    command: Value,
    event: &str,
    budget: u64,
    ui: &mut Ui,
    phase: usize,
) -> Result<Value> {
    worker.operation(Duration::from_secs(budget), |w| {
        w.send(&command)?;
        let result = enrollment::event(w, ui, phase)?;
        ensure!(result["event"] == event, "unexpected USB operation result");
        Ok(result)
    })
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    ensure!(
        digest(path)? == format!("{:x}", Sha256::digest(bytes)),
        "private artifact readback differs"
    );
    Ok(())
}
fn input_path(ui: &mut Ui, title: &str, body: &str) -> Result<PathBuf> {
    let value = ui.input(title, body, false)?;
    if let Some(rest) = value.strip_prefix("~/") {
        Ok(PathBuf::from(std::env::var_os("HOME").context("missing home directory")?).join(rest))
    } else {
        Ok(PathBuf::from(value.as_str()))
    }
}
fn enrollment_source(ui: &mut Ui, state_root: &Path) -> Result<PathBuf> {
    const TITLE: &str = "Saved Android enrollment directory";
    const BODY: &str =
        "Choose the retained native enrollment or older verified Python backup folder.";
    let candidates =
        enrollment_sources::discover(state_root, enrollment_sources::remembered(state_root));
    if candidates.is_empty() {
        return input_path(ui, TITLE, BODY);
    }
    let mut options: Vec<Choice> = candidates
        .iter()
        .map(|found| choice(&found.path.display().to_string(), found.detail()))
        .collect();
    options.push(choice("Enter a folder path", "Type the location yourself."));
    // The manual option is always last, so a single candidate is still an
    // explicit selection rather than something applied on the operator's behalf.
    let picked = ui.choose(
        TITLE,
        "Confirm which retained enrollment to import, or enter another folder. Whichever you pick is verified in full before use.",
        &options,
    )?;
    match enrollment_sources::resolve(&candidates, picked) {
        Some(path) => Ok(path),
        None => input_path(ui, TITLE, BODY),
    }
}
fn identity_input(
    ui: &mut Ui,
    label: &str,
    available: Option<String>,
    mac: bool,
) -> Result<String> {
    if let Some(value) = available {
        return Ok(value);
    }
    loop {
        let value = ui
            .input(
                label,
                "ADB could not read this value. Read it from Android device information before the remote restarts.",
                false,
            )?
            .to_string();
        let valid = if mac {
            let bytes = decode(&value.to_ascii_lowercase().replace(':', ""));
            !value.eq_ignore_ascii_case("02:00:00:00:00:00")
                && value.len() == 17
                && value.as_bytes().iter().enumerate().all(|(i, c)| {
                    if i % 3 == 2 {
                        *c == b':'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
                && bytes.is_ok_and(|b| {
                    b.len() == 6
                        && b[0] & 1 == 0
                        && b.iter().any(|c| *c != 0)
                        && b.iter().any(|c| *c != 255)
                })
        } else {
            (8..=64).contains(&value.len()) && value.bytes().all(|c| c.is_ascii_alphanumeric())
        };
        if valid {
            return Ok(if mac {
                value.to_ascii_lowercase()
            } else {
                value
            });
        }
        ui.progress(
            1,
            "That device-information value is not valid. Check Android and try again.",
            0,
            0,
        )?;
    }
}
fn plan_images(paths: &BTreeMap<String, PathBuf>, device: &Value, ui: &mut Ui) -> Result<Value> {
    let mut images = serde_json::Map::new();
    for (name, path) in paths {
        let size = fs::metadata(path)?.len();
        ensure!(
            size > 0
                && size.is_multiple_of(512)
                && size
                    <= device["partitions"][name]["size"]
                        .as_u64()
                        .context("missing target partition")?,
            "assembled image does not fit"
        );
        let mut file = fs::File::open(path)?;
        let mut chunks = Vec::new();
        let mut full = Sha256::new();
        let mut buffer = vec![0; crate::stage::CHUNK];
        let mut done = 0;
        while done < size {
            let n = (size - done).min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..n])?;
            full.update(&buffer[..n]);
            chunks.push(format!("{:x}", Sha256::digest(&buffer[..n])));
            done += n as u64;
            ui.progress(4, &format!("Verifying {name} image"), done, size)?;
        }
        ensure!(
            file.read(&mut [0u8; 1])? == 0 && file.metadata()?.len() == size,
            "image changed during hashing"
        );
        images.insert(
            name.clone(),
            json!({"size":size,"sha256":format!("{:x}",full.finalize()),"chunks":chunks}),
        );
    }
    Ok(Value::Object(images))
}

pub fn run(ui: &mut Ui, config: Option<&Path>, local_payload: Option<&Path>) -> Result<()> {
    ui.set_steps(
        [
            "Prepare",
            "Device",
            "Originals",
            "Start installer",
            "Wi-Fi",
            "Backups",
            "Install",
            "Finish",
        ]
        .map(String::from)
        .to_vec(),
    )?;
    let mode = ui.choose(
        "Install Couch",
        "For a fresh installation, start Android, enable USB debugging and connect USB. To reinstall Couch, keep it running and have your saved Android enrollment ready.",
        &[
            choice(
                "Install with Android backup",
                "Preserve Android data and device calibration before installing.",
            ),
            choice(
                "YOLO — skip Android data backup",
                "Device calibration and recovery originals are still preserved.",
            ),
            choice(
                "Reinstall existing Couch",
                "Import retained Android enrollment and back up the current Couch installation.",
            ),
            choice(
                "Restore stock Android",
                "Write your saved Android originals and a fresh stock filesystem. Couch is replaced.",
            ),
            choice("Cancel", "Leave the device unchanged."),
        ],
    )?;
    if mode == 4 {
        return Ok(());
    }
    // Restore shares the reinstall bootstrap path: the device currently runs Couch
    // and a saved Android enrollment is imported and re-bound before any write.
    let reinstall = mode == 2 || mode == 3;
    let restore = mode == 3;
    #[cfg(windows)]
    ui.choose("Windows USB driver setup", "The selected remote's MediaTek download interface and Couch installer interface (VID 0e8d, PID 201c) need usable WinUSB bindings. ADB working alone does not verify those drivers. Configure only this remote's interfaces; the installer will stop on a claim failure and will not replace drivers automatically.", &[choice("Continue with drivers prepared", "See the Windows USB notes in the installer guide.")])?;
    let release = public_inputs::release(
        config.context("This installer requires its verified release configuration")?,
    )?;
    let parent = state_root()?;
    let mut session =
        SessionGuard::create(&parent.join(format!("install-{}", &hex(&random()?)[..16])))?;
    ui.set_log_path(
        session
            .path()
            .to_str()
            .context("invalid private session path")?,
    )?;
    let skip_userdata = if reinstall {
        ui.choose(
            "Current Couch data backup",
            "The imported Android originals remain separate from this operation's backups.",
            &[
                choice(
                    "Back up current Couch data",
                    "Preserve current data before replacing the OS.",
                ),
                choice(
                    "YOLO — skip current Couch data",
                    "Calibration and boot/recovery originals are still saved.",
                ),
            ],
        )? == 1
    } else {
        mode == 1
    };
    let result = install(
        ui,
        &release,
        skip_userdata,
        reinstall,
        restore,
        &mut session,
        local_payload,
    );
    if let Err(error) = &result {
        if !matches!(session.phase(), Phase::Failed | Phase::Complete) {
            let _ = session.transition(
                Phase::Failed,
                &json!({"event":"installation_stopped","preserve_originals":true}),
            );
        }
        let _ = ui.error(&format!(
            "{error}. Keep saved originals at {}. No automatic retry or restore was attempted.",
            session.path().display()
        ));
    }
    result
}
#[allow(clippy::too_many_arguments)]
fn install(
    ui: &mut Ui,
    release: &public_inputs::Release,
    skip_userdata: bool,
    reinstall: bool,
    restore: bool,
    session: &mut SessionGuard,
    local_payload: Option<&Path>,
) -> Result<()> {
    let _usb_lease = UsbLease::acquire(session)?;
    // macOS: Apple's serial driver owns the preloader and only root can hand it
    // to libusb. Hold that right now, before the long downloads, and refresh it
    // until the worker has started. Everything else stays unprivileged.
    let _privilege = adapter::Privilege::acquire()?;
    let dependencies = dependencies::prepare(
        session,
        dependencies::host_platform()?,
        |label, done, total, unit| ui.progress_with_unit(0, label, done, total, unit),
    )?;
    let ota = session.path().join("official-ota.zip");
    public_inputs::official(&ota, |done, total| {
        ui.progress(0, "Downloading verified official owner inputs", done, total)
    })?;
    let prepared = session.path().join("owner-inputs");
    ui.progress(0, "Reconstructing verified owner inputs", 0, 0)?;
    crate::prepare(&ota, &prepared)?;
    let public = public_inputs::payload(release, session.path(), local_payload, |done, total| {
        ui.progress(
            0,
            if local_payload.is_some() {
                "Verifying local Couch OS package"
            } else {
                "Downloading Couch OS"
            },
            done,
            total,
        )
    })?;
    // The installer stage is always assembled from the Couch payload to bootstrap
    // the RAM Wi-Fi installer. A restore writes no Couch OS images, so it builds
    // only the stage; a normal install also builds Couch boot/recovery/userdata.
    let mut images = BTreeMap::new();
    let ramdisks: &[(&str, &str)] = if restore {
        &[("installer", "installer.cpio.gz")]
    } else {
        &[
            ("boot", "boot.cpio.gz"),
            ("recovery", "recovery.cpio.gz"),
            ("installer", "installer.cpio.gz"),
        ]
    };
    for (name, ramdisk) in ramdisks {
        let ram = assembly::owner_ramdisk(&fs::read(&public[*ramdisk])?, &prepared)?;
        let kernel = fs::read(&public["zImage"])?;
        let bytes = assembly::boot_image(
            &prepared,
            if *name == "recovery" {
                None
            } else {
                Some(&kernel)
            },
            &ram,
        )?;
        let path = session.path().join(format!("{name}.img"));
        write(&path, &bytes)?;
        images.insert(name.to_string(), path);
    }
    let stage = images.remove("installer").unwrap();
    let stage_hash = digest(&stage)?;
    if !restore {
        images.insert("userdata".into(), public["userdata.ext4"].clone());
    }
    // Owner vendor runtime is a Couch-install concept; a stock restore carries none.
    let vendor = if restore {
        None
    } else {
        Some(vendor_transfer::prepare(&prepared)?)
    };
    session.transition(Phase::InputsVerified,&json!({"event":"inputs_verified","release":release.version,"payload_sha256":release.payload.sha256,"stage_sha256":stage_hash}))?;
    let (saved, serial, expected_cid, identity) = if reinstall {
        let state_root = session
            .path()
            .parent()
            .context("session needs a parent")?
            .to_path_buf();
        let source = enrollment_source(ui, &state_root)?;
        let imported = if source.join("enrollment.json").is_file() {
            saved_enrollment::import(&source, session)?
        } else {
            let profile = input_path(
                ui,
                "Trusted stock manifest",
                "Select the independently retained stock manifest used with these originals.",
            )?;
            let hash=ui.input("Trusted stock manifest SHA-256","Enter its independently recorded SHA-256. Do not take a new trust pin from the backup being imported.",false)?;
            saved_enrollment::import_legacy(
                &source,
                &profile,
                &hash,
                session,
                |name, done, total| {
                    ui.progress(1, &format!("Verifying retained {name}"), done, total)
                },
            )?
        };
        // Only now, past full admission, is the folder worth offering again.
        enrollment_sources::remember(&state_root, &source);
        let cid = imported.record().cid.clone();
        let identity = serde_json::to_value(&imported.record().android_identity)?;
        (Some(imported), None, cid, identity)
    } else {
        ui.progress(
        1,
        "Enable USB debugging, connect the remote, and accept Android's USB authorization prompt.",
        0,
        0,
    )?;
        dependencies.verify()?;
        let devices = android::discover(&dependencies.adb)?;
        ensure!(
            !devices.is_empty(),
            "No Android remote found. Enable USB debugging and connect USB"
        );
        let selected = ui.choose(
            "Choose your Android remote",
            "Only USB-connected Android devices are listed.",
            &devices
                .iter()
                .map(|d| {
                    choice(
                        &d.serial,
                        &format!("{} · {}", d.model.as_deref().unwrap_or("Android"), d.state),
                    )
                })
                .collect::<Vec<_>>(),
        )?;
        let android = android::capture(&dependencies.adb, &devices[selected].serial)?;
        let cid = android
            .cid
            .as_deref()
            .context("Android did not expose a usable eMMC identity; no write is permitted")?;
        let identity = json!({"device_id":identity_input(ui,"Android Device ID",android.device_id,false)?,"wifi_mac":identity_input(ui,"Android Wi-Fi MAC",android.wifi_mac,true)?,"bt_mac":identity_input(ui,"Android Bluetooth MAC",android.bt_mac,true)?});
        (None, Some(android.serial), cid.to_string(), identity)
    };
    dependencies.verify()?;
    let script = adapter::materialize(session)?;
    let mut command = adapter::mtk_worker(&dependencies.python);
    command
        .arg("-I")
        .arg("-B")
        .arg(dependencies::python_path(&script)?)
        .arg("--events-stdio")
        .current_dir(session.path());
    let mut worker = Worker::spawn(&mut command)?;
    simple(
        &mut worker,
        json!({"op":"prepare","checkout":dependencies::python_path(&dependencies.mtk_root)?,"loader":dependencies::python_path(&dependencies.owner_da)?,"loader_sha256":dependencies.owner_da_sha256,"preloader":dependencies::python_path(&prepared.join("bootstrap/preloader.img"))?,"preloader_sha256":"0ad0d14b7203d98a6567af7a022cfe5df5b6fcbba60cb4e9b4bc2ee569cf1069","libusb":dependencies::python_path(&dependencies.libusb)?}),
        "prepared",
        60,
        ui,
        1,
    )?;
    let bound = if let Some(serial) = &serial {
        // The ADB server must not hold the remote while the worker reads its
        // USB serial descriptor (issue #89, Windows). Reboot restarts it.
        android::stop_server(&dependencies.adb)?;
        let bound = simple(
            &mut worker,
            json!({"op":"android_bind","serial":serial}),
            "android_bound",
            30,
            ui,
            1,
        )?;
        ensure!(
            bound["serial_sha256"] == format!("{:x}", Sha256::digest(serial.as_bytes())),
            "Android USB serial binding differs"
        );
        session.transition(Phase::AndroidBound,&json!({"event":"android_bound","cid":expected_cid,"usb":bound,"android_identity":identity}))?;
        ui.progress(1, "Restarting the selected remote through USB", 0, 0)?;
        dependencies.verify()?;
        android::reboot(&dependencies.adb, serial)?;
        bound
    } else {
        let bound = loop {
            let result = simple(
                &mut worker,
                json!({"op":"enumerate"}),
                "candidates",
                20,
                ui,
                1,
            )?;
            let candidates = result["devices"]
                .as_array()
                .context("invalid USB inventory")?
                .iter()
                .filter(|d| d["pid"] == 0x201c)
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                ui.choose("Connect the Couch remote", "Keep Couch powered on and connected through USB so its physical port can be selected.",&[choice("Check USB again","No write or reboot has been requested.")])?;
                continue;
            }
            let options = candidates
                .iter()
                .map(|d| {
                    choice(
                        &format!("USB bus {} · ports {}", d["bus"], d["ports"]),
                        "Select the connected Couch remote.",
                    )
                })
                .collect::<Vec<_>>();
            let selected=ui.choose("Select the connected Couch remote","Its stored CID and calibration must match the imported enrollment before any write.",&options)?;
            break candidates[selected].clone();
        };
        ui.progress(
            1,
            "Checking Couch identity and requesting USB restart",
            0,
            0,
        )?;
        let restart = simple(
            &mut worker,
            json!({"op":"couch_reboot","candidate":bound,"cid":expected_cid}),
            "couch_reboot",
            20,
            ui,
            1,
        ).context("Couch USB identity/restart failed or its delivery is ambiguous; no automatic retry was attempted")?;
        session
            .checkpoint(&json!({"event":"couch_restart","usb":bound,"result":restart["result"]}))?;
        match restart["result"].as_str() {
            Some("requested") => {}
            Some("unavailable") => {
                ui.choose("Restart the selected remote", "USB serial restart is unavailable; no reboot command was sent. Keep USB connected. After Continue, hold the side Power button until the remote turns off, then release it. If needed, hold Power until it starts again.",&[choice("Continue and watch USB","Only the selected physical USB port can be captured.")])?;
            }
            _ => anyhow::bail!("Invalid Couch restart result"),
        }
        bound
    };
    let start = Instant::now();
    let candidate = loop {
        ensure!(start.elapsed()<Duration::from_secs(120),"Remote did not enter download mode. No boot write occurred; check USB driver binding and power");
        let result = simple(
            &mut worker,
            json!({"op":"enumerate"}),
            "candidates",
            20,
            ui,
            3,
        )?;
        let found = result["devices"]
            .as_array()
            .context("invalid USB inventory")?
            .iter()
            .filter(|d| {
                d["bus"] == bound["bus"]
                    && d["ports"] == bound["ports"]
                    && d["pid"].as_u64().is_some_and(|pid| {
                        [0x0003, 0x6000, 0x2000, 0x2001, 0x20ff, 0x3000].contains(&pid)
                    })
            })
            .collect::<Vec<_>>();
        ensure!(found.len() <= 1, "ambiguous selected USB port");
        if let Some(device) = found.first() {
            break (*device).clone();
        }
        ui.progress_with_unit(
            3,
            "Waiting for the selected remote on USB",
            start.elapsed().as_secs(),
            120,
            crate::frontend::ProgressUnit::Seconds,
        )?;
        thread::sleep(Duration::from_millis(20));
    };
    let connected = simple(
        &mut worker,
        json!({"op":"start","candidate":candidate}),
        "connected",
        120,
        ui,
        3,
    )
    .map_err(|error| anyhow::anyhow!("USB download startup failed: {error:#}"))?;
    let cid = connected["cid"]
        .as_str()
        .context("missing observed canonical CID")?;
    ensure!(
        cid == expected_cid,
        "Canonical download-agent CID differs from enrollment"
    );
    let device = &connected["device"];
    enrollment::admit_layout(device, cid, &prepared)?;
    let mut required = 256 * 1024 * 1024u64;
    for name in enrollment::IDENTITY
        .into_iter()
        .chain(enrollment::ORIGINALS)
    {
        let size = device["partitions"][name]["size"]
            .as_u64()
            .context("missing original size")?;
        required = required
            .checked_add(size.checked_mul(2).context("backup size overflow")?)
            .context("backup size overflow")?;
    }
    if !skip_userdata {
        required = required
            .checked_add(
                device["partitions"]["userdata"]["size"]
                    .as_u64()
                    .context("missing userdata size")?,
            )
            .context("backup size overflow")?;
    }
    ensure!(crate::space::available(session.path())? >= required, "Not enough free space for selected original backups and safety margin; no boot write occurred");

    let originals = enrollment::capture(&mut worker, device, session, ui)?;
    let imported_proof = if let Some(saved) = saved {
        let observation = saved_enrollment::ObservedHardware {
            cid: cid.to_string(),
            capacity: device["capacity"].as_u64().context("missing capacity")?,
            hwcode: device["hwcode"]
                .as_u64()
                .context("missing hardware code")?
                .try_into()?,
            cid_encoding: device["cid_encoding"]
                .as_str()
                .context("missing CID encoding")?
                .into(),
            partitions: serde_json::from_value(device["partitions"].clone())?,
            identity_sha256: enrollment::IDENTITY
                .into_iter()
                .map(|name| (name.to_string(), originals[name].clone()))
                .collect(),
            retained_sha256: originals.clone(),
        };
        let proof = saved.rebind_mode(&observation, session, restore)?;
        session.transition(Phase::AndroidBound,&json!({"event":"retained_enrollment_bound","cid":cid,"original_os":"Couch","enrollment_sha256":proof.enrollment().sha256(),"usb":bound,"restore":restore}))?;
        Some(proof)
    } else {
        let profile = enrollment::android_originals(session.path())?;
        session.checkpoint(&json!({"event":"android_stock_profile_verified","profile":profile}))?;
        None
    };
    ensure!(
        imported_proof.is_some() == reinstall,
        "missing retained enrollment admission"
    );
    if restore {
        // Assemble the restore set from the re-bound Android enrollment (recovery,
        // logo, odmdtbo, boot) plus a fresh stock F2FS userdata the owner prepared
        // offline with the firmware's make_f2fs. assemble re-hashes every original
        // against the imported record and refuses a non-Android enrollment.
        let proof = imported_proof
            .as_ref()
            .context("restore requires a re-bound Android enrollment")?;
        let enrollment = proof.enrollment();
        let userdata = input_path(
            ui,
            "Stock Android userdata image",
            "Select the full F2FS userdata image made offline by the firmware's make_f2fs. See docs/installer-android-restore.md.",
        )?;
        let receipt = input_path(
            ui,
            "Stock userdata receipt",
            "Select the couch-stock-userdata receipt recorded beside that image.",
        )?;
        ui.progress(
            2,
            "Verifying saved Android originals and stock userdata",
            0,
            0,
        )?;
        images = crate::android_restore::assemble(
            enrollment.directory(),
            enrollment.record(),
            &userdata,
            &receipt,
        )?;
    } else {
        let logo = assembly::logo_image(
            &fs::read(session.path().join("bootstrap-logo.img"))?,
            &fs::read(
                public
                    .get("logo.bgra")
                    .context("public Couch logo frame missing")?,
            )?,
        )?;
        let logo_path = session.path().join("logo.img");
        write(&logo_path, &logo)?;
        images.insert("logo".into(), logo_path);
    }
    let identity_hashes: BTreeMap<_, _> = enrollment::IDENTITY
        .into_iter()
        .map(|n| (n, originals[n].clone()))
        .collect();
    let original_hashes: BTreeMap<_, _> = enrollment::ORIGINALS
        .into_iter()
        .map(|n| (n, originals[n].clone()))
        .collect();
    let entries:BTreeMap<_,_>=originals.iter().map(|(name,sha)|(name,json!({"file":format!("bootstrap-{name}.img"),"size":device["partitions"][name]["size"],"sha256":sha}))).collect();
    let enrollment_path = session.path().join(if reinstall {
        "current-couch-snapshot.json"
    } else {
        "enrollment.json"
    });
    write(
        &enrollment_path,
        &serde_json::to_vec(
            &json!({"schema":1,"kind":"couch-device-enrollment","model":"sanytron-ha100","cid":cid,"capacity":device["capacity"],"partitions":device["partitions"],"identity_sha256":identity_hashes,"android_identity":identity,"original_os":if reinstall {"Couch"} else {"Android"},"originals":entries}),
        )?,
    )?;
    session.transition(
        Phase::OriginalsSaved,
        &json!({"event":"enrollment_complete","enrollment_sha256":digest(&enrollment_path)?}),
    )?;
    simple(
        &mut worker,
        json!({"op":"authorize_boot","cid_sha256":device["runtime_cid_sha256"],"partitions":device["partitions"],"identity_sha256":identity_hashes,"original_sha256":original_hashes,"stage":dependencies::python_path(&stage)?,"stage_sha256":stage_hash}),
        "boot_authorized",
        1800,
        ui,
        2,
    )?;
    session.transition(
        Phase::StageBootPending,
        &json!({"event":"bootstrap_write_admitted","stage_sha256":stage_hash}),
    )?;
    simple(
        &mut worker,
        json!({"op":"write_boot"}),
        "boot_verified",
        1800,
        ui,
        3,
    )?;
    session
        .checkpoint(&json!({"event":"bootstrap_readback_verified","stage_sha256":stage_hash}))?;
    simple(
        &mut worker,
        json!({"op":"boot"}),
        "boot_requested",
        30,
        ui,
        3,
    )?;
    let start = Instant::now();
    loop {
        ensure!(
            start.elapsed() < Duration::from_secs(120),
            "Installer stage did not appear. Keep the saved originals"
        );
        if simple(
            &mut worker,
            json!({"op":"stage_present"}),
            "stage_present",
            20,
            ui,
            3,
        )?["present"]
            == true
        {
            break;
        }
        ui.progress_with_unit(
            3,
            "Waiting for the installer USB stage",
            start.elapsed().as_secs(),
            120,
            crate::frontend::ProgressUnit::Seconds,
        )?;
        thread::sleep(Duration::from_millis(250));
    }
    simple(
        &mut worker,
        json!({"op":"stage_open"}),
        "stage_open",
        60,
        ui,
        3,
    )?;
    network::ready(&mut worker, ui)?;
    let network = network::select(&mut worker, ui)?;
    // Wi-Fi is always provisioned over USB for the TLS transfer. A restore writes
    // raw full-partition images, so the plan carries no stage-side network/vendor
    // personalization; a normal install personalizes its compact userdata.
    let mut plan = json!({"schema":1,"nonce":hex(&random()?),"manifest_sha256":release.payload.sha256,"stage_sha256":stage_hash,"original_boot_sha256":originals["boot"],"cid":cid,"capacity":device["capacity"],"partitions":device["partitions"],"identity_sha256":identity_hashes,"images":plan_images(&images,device,ui)?,"skip_userdata_backup":skip_userdata});
    // Stage probes parse the plan with deny_unknown_fields, and images built
    // before the restore mode existed do not know this key. Send it only for a
    // restore, which needs a stage that understands it; a normal install keeps
    // the exact plan shape every shipped stage accepts.
    if restore {
        plan["restore"] = json!(true);
    } else {
        plan["network"] = network.clone();
        plan["vendor_source_sha256"] = json!(vendor
            .as_ref()
            .context("owner vendor runtime missing for install")?
            .source_sha256());
    }
    let plan_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&plan)?);
    network::rpc(
        &mut worker,
        "stage_bind",
        json!({"plan_sha256":format!("{:x}",Sha256::digest(plan_bytes.as_slice())),"nonce":plan["nonce"]}),
    )?;
    let cert = rcgen::generate_simple_self_signed(vec!["couch-probe".into()])?;
    let der = cert.cert.der().to_vec();
    let key = zeroize::Zeroizing::new(cert.signing_key.serialize_der());
    let token = zeroize::Zeroizing::new(random()?);
    let mut provision = network.clone();
    let object = provision.as_object_mut().unwrap();
    object.insert("certificate_hex".into(), json!(hex(&der)));
    object.insert("private_key_hex".into(), json!(hex(&key)));
    object.insert("token_hex".into(), json!(hex(token.as_ref())));
    network::rpc(&mut worker, "stage_provision", provision)?;
    let start = Instant::now();
    let address = loop {
        ensure!(
            start.elapsed() < Duration::from_secs(120),
            "Remote Wi-Fi connection timed out"
        );
        let status = network::rpc(&mut worker, "stage_status", Value::Null)?;
        ensure!(
            status["status"] != "failed",
            "Remote Wi-Fi connection failed; preserve originals"
        );
        if status["status"] == "connected" {
            break status["ip"]
                .as_str()
                .context("missing Wi-Fi address")?
                .parse::<std::net::Ipv4Addr>()?;
        }
        ui.progress_with_unit(
            4,
            "Connecting the remote to Wi-Fi",
            start.elapsed().as_secs(),
            120,
            crate::frontend::ProgressUnit::Seconds,
        )?;
        thread::sleep(Duration::from_millis(500));
    };
    simple(
        &mut worker,
        json!({"op":"stage_close"}),
        "stage_closed",
        30,
        ui,
        4,
    )?;
    drop(worker);
    let mut connection = stage_tls::connect((address, 8443).into(), &der, &token)?;
    connection
        .sock
        .set_phase_timeout(Duration::from_secs(1800))?;
    session.transition(Phase::StageConnected,&json!({"event":"stage_authenticated","plan_sha256":format!("{:x}",Sha256::digest(plan_bytes.as_slice()))}))?;
    let mut channel = Channel::authenticated(connection);
    transaction::run_with_vendor(
        &mut channel,
        &plan,
        &images,
        &enrollment::original_boot(session),
        if reinstall {
            OriginalOs::Couch
        } else {
            OriginalOs::Android
        },
        session,
        vendor,
        |phase, target, done, total| {
            ui.progress(
                if phase.contains("Backup") { 5 } else { 6 },
                &format!("{phase} {target}"),
                done,
                total,
            )
        },
    )?;
    let reboot = ui.choose(
        if restore {
            "Stock Android restore verified"
        } else {
            "Couch installation verified"
        },
        "Backups and the journal are saved on this computer.",
        &[
            choice(
                if restore {
                    "Restart into stock Android"
                } else {
                    "Restart into Couch"
                },
                "Start the newly written OS.",
            ),
            choice(
                "Leave the installer running",
                "Keep the current verified stage available.",
            ),
        ],
    )?;
    channel.send_json(&json!({"action":if reboot==0{"reboot"}else{"leave"}}))?;
    if reboot == 0 {
        channel.expect(&json!({"event":"rebooting"}))?;
    }
    ui.progress(
        7,
        if restore {
            "Stock Android restored. Check first boot on the remote, then re-enroll from Android before any future Couch install."
        } else {
            "Installation verified. First normal boot still needs to be checked on the remote."
        },
        0,
        0,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn cancel_never_requires_release_config_or_creates_session() {
        let mut ui = Ui::new(
            Box::new(Cursor::new(b"{\"id\":1,\"value\":\"4\"}\n".to_vec())),
            Box::new(Vec::new()),
        );
        run(&mut ui, None, None).unwrap();
    }
    #[test]
    fn images_use_fixed_wire_chunks_and_full_digest() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("image");
        let data = vec![42; crate::stage::CHUNK + 512];
        fs::write(&path, &data).unwrap();
        let paths = BTreeMap::from([("userdata".into(), path)]);
        let device = json!({"partitions":{"userdata":{"size":data.len()}}});
        let mut ui = Ui::new(Box::new(Cursor::new(Vec::new())), Box::new(Vec::new()));
        ui.set_steps(vec!["step".into(); 8]).unwrap();
        let result = plan_images(&paths, &device, &mut ui).unwrap();
        assert_eq!(
            result["userdata"]["chunks"],
            json!([
                format!("{:x}", Sha256::digest(&data[..crate::stage::CHUNK])),
                format!("{:x}", Sha256::digest(&data[crate::stage::CHUNK..]))
            ])
        );
        assert_eq!(
            result["userdata"]["sha256"],
            format!("{:x}", Sha256::digest(&data))
        );
    }
}
