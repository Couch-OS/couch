use couch_system::{
    network::{self, Event},
    protocol::{self, Reply, Request},
};
use std::{
    fs, io,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

pub fn helper(name: &str) -> Result<(), String> {
    // Fixed internal names only; never construct a shell command from a request.
    use std::os::unix::process::CommandExt;
    let mut child = Command::new("/bin/busybox")
        .args([
            "sh",
            std::env::current_exe()
                .map_err(|_| "Could not locate system runtime")?
                .parent()
                .ok_or("Could not locate system runtime")?
                .join(name)
                .to_str()
                .ok_or("Invalid runtime path")?,
        ])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "System helper could not start")?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err("System helper failed; inspect device diagnostics".into())
                }
            }
            Ok(None) if start.elapsed() < Duration::from_secs(45) => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                return Err("System helper timed out or became unavailable".into());
            }
        }
    }
}

pub fn alpine(binary: &str) -> Command {
    let mut command = Command::new("/bin/busybox");
    command.args(["chroot", "/mnt/alpine", binary]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
fn root_peer(stream: &UnixStream) -> bool {
    #[cfg(target_os = "linux")]
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut size = std::mem::size_of_val(&cred) as libc::socklen_t;
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut _,
            &mut size,
        ) == 0
            && cred.uid == 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        false
    }
}
pub fn serve() -> Result<(), String> {
    let dir = "/tmp/couch-system";
    fs::DirBuilder::new()
        .recursive(false)
        .create(dir)
        .or_else(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(e)
            }
        })
        .map_err(|_| "Could not create service directory")?;
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(dir).map_err(|_| "Could not inspect service directory")?;
    if !meta.is_dir() || meta.uid() != 0 {
        return Err("Invalid service directory owner/type".into());
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| "Could not secure service directory")?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(format!("{dir}/lock"))
        .map_err(|_| "Could not lock system service")?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("System service already running".into());
    }
    match fs::remove_file(protocol::SOCKET) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not replace stale socket".into()),
    }
    let listener =
        UnixListener::bind(protocol::SOCKET).map_err(|_| "Could not bind system socket")?;
    fs::set_permissions(protocol::SOCKET, fs::Permissions::from_mode(0o600))
        .map_err(|_| "Could not secure system socket")?;
    let updates = couch_updates::Updater::new(std::path::PathBuf::from("/mnt/alpine/opt/couch"));
    let gate = Arc::new(Mutex::new(()));
    let count = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if !root_peer(&stream) || count.load(Ordering::Relaxed) >= 8 {
            continue;
        }
        let gate = gate.clone();
        let updates = updates.clone();
        let count = count.clone();
        count.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let _ = handle_updates(stream, &gate, &updates);
            count.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}
#[cfg(test)]
fn handle(stream: UnixStream, gate: &Mutex<()>) -> io::Result<()> {
    handle_updates(
        stream,
        gate,
        &couch_updates::Updater::new(std::path::PathBuf::from(
            "/nonexistent-couch-system-fixture",
        )),
    )
}
fn handle_updates(
    mut stream: UnixStream,
    gate: &Mutex<()>,
    updates: &couch_updates::Updater,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let request: Request = protocol::read(&mut stream)?;
    if matches!(request, Request::Health) {
        return protocol::write(&Reply::Ready, &mut stream);
    }
    if matches!(request, Request::UpdateStatus) {
        return protocol::write(&Reply::Update(updates.status()), &mut stream);
    }
    if matches!(request, Request::SshAvailable) {
        return protocol::write(&Reply::Available(crate::access::available()), &mut stream);
    }
    if matches!(
        request,
        Request::EnrollKey { .. } | Request::SetPassword { .. } | Request::PortalJoin { .. }
    ) && !std::path::Path::new("/tmp/couch.setup").exists()
    {
        return protocol::write(
            &Reply::Done(Err("Open the recovery hotspot on the remote first".into())),
            &mut stream,
        );
    }
    let Ok(_guard) = gate.try_lock() else {
        return protocol::write(
            &Reply::Done(Err(
                "Another system operation is active; finish or cancel it first".into(),
            )),
            &mut stream,
        );
    };
    match request {
        Request::UpdateCheck { automatic } => {
            protocol::write(&Reply::Done(updates.check(automatic)), &mut stream)
        }
        Request::UpdateSettings {
            channel,
            automatic_checks,
        } => protocol::write(
            &Reply::Done(updates.settings(channel, automatic_checks)),
            &mut stream,
        ),
        Request::UpdateInstall { version } => {
            protocol::write(&Reply::Done(updates.install(&version)), &mut stream)
        }
        Request::UpdateRestart => {
            let result = if updates.ready() {
                couch_updates::activate(std::path::Path::new("/mnt/alpine/opt/couch"))
            } else {
                Err("No verified update is ready".into())
            };
            let reboot = result.is_ok();
            let reply = protocol::write(&Reply::Done(result), &mut stream);
            if reboot {
                std::thread::sleep(Duration::from_secs(2));
                let _ = Command::new("/bin/busybox").args(["reboot", "-f"]).status();
            }
            reply
        }
        Request::Network => network_session(stream),
        Request::Hotspot => protocol::write(&Reply::Done(helper("portal.sh")), &mut stream),
        Request::SshAuto => protocol::write(&Reply::Done(crate::access::ssh_auto()), &mut stream),
        Request::Ssh { enabled } => {
            protocol::write(&Reply::Done(crate::access::ssh(enabled)), &mut stream)
        }
        Request::EnrollKey { key } => {
            protocol::write(&Reply::Done(crate::access::enroll(&key)), &mut stream)
        }
        Request::SetPassword { password } => protocol::write(
            &Reply::Done(crate::access::password(&password)),
            &mut stream,
        ),
        Request::Scan => {
            let result = fs::read_to_string("/tmp/scan.raw")
                .map(|s| network::parse_scan(&s))
                .map_err(|_| "No cached scan available".into());
            protocol::write(&Reply::Networks(result), &mut stream)
        }
        Request::PortalJoin { ssid, password } => {
            // Reply before switching off the AP that carries this HTTP response.
            // Inputs are validated before promising the background operation.
            if let Err(error) = network::validate(&ssid, &password) {
                return protocol::write(&Reply::Done(Err(error)), &mut stream);
            }
            protocol::write(&Reply::Done(Ok(())), &mut stream)?;
            std::thread::sleep(Duration::from_secs(2));
            let result = portal_join(ssid, password);
            if result.is_err() {
                let _ = helper("portal.sh");
            }
            // No reboot on failed association; keep recovery reachable.
            Ok(())
        }
        Request::SshAvailable | Request::Health | Request::UpdateStatus => unreachable!(),
    }
}
fn network_session(mut stream: UnixStream) -> io::Result<()> {
    protocol::write(&Reply::Ready, &mut stream)?;
    let mut reader = stream.try_clone()?;
    let worker = network::Worker::start();
    let tx = worker.tx.clone();
    let cancel = worker.cancel.clone();
    let input = std::thread::spawn(move || {
        while let Ok(request) = protocol::read::<network::Request>(&mut reader) {
            cancel.store(
                matches!(request, network::Request::Cancel),
                Ordering::Relaxed,
            );
            if tx.send(request).is_err() {
                break;
            }
        }
        cancel.store(true, Ordering::Relaxed);
    });
    loop {
        match worker.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if protocol::write(&event, &mut stream).is_err() {
                    break;
                }
                if matches!(event, Event::Saved(Ok(()))) {
                    let _ = crate::access::ssh_auto();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !input.is_finished() => continue,
            Err(_) => break,
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = input.join();
    worker.finish();
    Ok(())
}
fn portal_join(ssid: String, password: String) -> Result<(), String> {
    helper("station.sh")?;
    let worker = network::Worker::start();
    worker.send(network::Request::Test { ssid, password });
    let result = (|| {
        match worker
            .rx
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| "Network test timed out")?
        {
            Event::Tested(Ok(_)) => {}
            Event::Tested(Err(e)) => return Err(e),
            _ => return Err("Unexpected network reply".into()),
        }
        worker.send(network::Request::Save);
        match worker
            .rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "Network save timed out")?
        {
            Event::Saved(result) => result?,
            _ => return Err("Unexpected save reply".into()),
        }
        let _ = fs::remove_file("/tmp/couch.setup");
        let _ = crate::access::ssh_auto();
        // Existing GUI supervisor reloads the screen after portal completion.
        let _ = Command::new("/bin/busybox")
            .args(["killall", "couch-gui"])
            .status();
        Ok(())
    })();
    worker.finish();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_owner_cannot_switch_radio() {
        let gate = Mutex::new(());
        let guard = gate.lock().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        protocol::write(&Request::Hotspot, &mut client).unwrap();
        handle(server, &gate).unwrap();
        assert!(matches!(
            protocol::read::<Reply>(&mut client).unwrap(),
            Reply::Done(Err(_))
        ));
        drop(guard);
    }
    #[test]
    fn malformed_frame_never_reaches_operations() {
        use std::io::Write;
        let (mut client, server) = UnixStream::pair().unwrap();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert!(handle(server, &Mutex::new(())).is_err());
    }
}
