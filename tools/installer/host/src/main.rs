use anyhow::{bail, Result};
use std::path::PathBuf;
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args
        .first()
        .is_some_and(|value| value == "prepare-dependencies" || value == "--dependency-smoke")
    {
        anyhow::ensure!(
            args.len() == 2,
            "usage: couch-installer-host prepare-dependencies NEW_PRIVATE_PARENT"
        );
        use couch_installer_host::{dependencies, session};
        let parent = PathBuf::from(&args[1]);
        session::create_private_parent(&parent)?;
        let guard = session::SessionGuard::create(&parent.join("run"))?;
        let mut last = String::new();
        let prepared =
            dependencies::prepare(&guard, dependencies::host_platform()?, |label, _, _, _| {
                if label != last {
                    eprintln!("{label}…");
                    last = label.into();
                }
                Ok(())
            })?;
        if args[0] == "--dependency-smoke" {
            println!("{}", dependencies::smoke(&prepared)?);
        } else {
            println!(
                "{}",
                serde_json::json!({"runtime":prepared.runtime_root,"runtime_receipt_sha256":prepared.runtime_receipt_sha256,"adb":prepared.adb,"owner_da":prepared.owner_da,"usb_opened":false})
            );
        }
        return Ok(());
    }
    if args.first().is_some_and(|value| value == "verify-public") {
        anyhow::ensure!(
            args.len() == 4,
            "usage: couch-installer-host verify-public CONFIG ARCHIVE NEW_OUTPUT"
        );
        let release = couch_installer_host::public_inputs::release(&PathBuf::from(&args[1]))?;
        let files = couch_installer_host::public_inputs::extract(
            &release,
            &PathBuf::from(&args[2]),
            &PathBuf::from(&args[3]),
        )?;
        println!(
            "{}",
            serde_json::json!({"verified_public_files":files.len(),"device_access":false})
        );
        return Ok(());
    }
    if args.first().is_some_and(|value| value == "--ui-smoke") {
        return ui_smoke(&args[1..]);
    }
    if args
        .iter()
        .any(|v| v == "--events-fd" || v == "--events-stdio")
    {
        return installer(&args);
    }
    if args.len() != 3 || args[0] != "prepare-official" {
        bail!("usage: couch-installer-host prepare-official OFFICIAL_ZIP NEW_PRIVATE_DIRECTORY");
    }
    couch_installer_host::prepare(&PathBuf::from(&args[1]), &PathBuf::from(&args[2]))?;
    println!("Official owner inputs verified with native Rust. No USB or installation performed.");
    Ok(())
}

/// Explicitly device-free transport fixture. It cannot enter installation code.
fn ui_smoke(args: &[std::ffi::OsString]) -> Result<()> {
    use couch_installer_host::frontend::{Choice, Ui};
    #[cfg(unix)]
    let mut ui = {
        anyhow::ensure!(
            args == ["--events-fd", "3"],
            "expected private event socket"
        );
        Ui::inherited_socket(3)?
    };
    #[cfg(windows)]
    let mut ui = {
        anyhow::ensure!(args == ["--events-stdio"], "expected private event pipes");
        Ui::stdio()
    };
    ui.set_steps(vec!["Interface check".into()])?;
    ui.set_version(concat!(
        "host ",
        env!("CARGO_PKG_VERSION"),
        " (interface check)"
    ))?;
    ui.choose(
        "Device-free interface check",
        "This checks the terminal channel only.",
        &[Choice {
            label: "Continue".into(),
            detail: "No device access".into(),
        }],
    )?;
    let _secret = ui.input(
        "Test masked input",
        "Enter any fixture text, not a real password.",
        true,
    )?;
    ui.progress(
        0,
        "Native interface verified. No USB session started.",
        1,
        1,
    )?;
    ui.finish(0)
}

fn installer(args: &[std::ffi::OsString]) -> Result<()> {
    use couch_installer_host::frontend::Ui;
    let mut config = None;
    let mut local_payload = None;
    let mut wifi_debug = None;
    let mut events = false;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--wifi-debug" && wifi_debug.is_none() && index + 1 < args.len() {
            wifi_debug = Some(PathBuf::from(&args[index + 1]));
            index += 2;
        } else if args[index] == "--config" && config.is_none() && index + 1 < args.len() {
            config = Some(PathBuf::from(&args[index + 1]));
            index += 2;
        } else if args[index] == "--local-payload"
            && local_payload.is_none()
            && index + 1 < args.len()
        {
            local_payload = Some(PathBuf::from(&args[index + 1]));
            index += 2;
        } else if cfg!(unix)
            && args[index] == "--events-fd"
            && !events
            && args.get(index + 1).is_some_and(|v| v == "3")
        {
            events = true;
            index += 2;
        } else if cfg!(windows) && args[index] == "--events-stdio" && !events {
            events = true;
            index += 1;
        } else {
            bail!("unsupported native installer arguments");
        }
    }
    anyhow::ensure!(events, "private installer event channel missing");
    anyhow::ensure!(
        wifi_debug.is_none() || (config.is_none() && local_payload.is_none()),
        "debug attachment cannot be combined with installer inputs"
    );
    #[cfg(unix)]
    let mut ui = Ui::inherited_socket(3)?;
    #[cfg(windows)]
    let mut ui = Ui::stdio();
    let result = if let Some(path) = wifi_debug {
        couch_installer_host::wifi_debug::run(&mut ui, &path)
    } else {
        couch_installer_host::orchestrator::run(
            &mut ui,
            config.as_deref(),
            local_payload.as_deref(),
        )
    };
    if let Err(error) = &result {
        let _ = ui.error(&error.to_string());
    }
    let _ = ui.finish(if result.is_ok() { 0 } else { 1 });
    result
}

#[cfg(test)]
mod debug_argument_tests {
    #[test]
    fn debug_and_installer_inputs_are_rejected_before_opening_event_channel() {
        for installer_option in ["--config", "--local-payload"] {
            let mut args = vec!["--wifi-debug", "debug.json", installer_option, "unused"];
            if cfg!(unix) {
                args.extend(["--events-fd", "3"]);
            } else {
                args.push("--events-stdio");
            }
            let args = args
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            assert!(super::installer(&args)
                .unwrap_err()
                .to_string()
                .contains("cannot be combined"));
        }
    }
}
