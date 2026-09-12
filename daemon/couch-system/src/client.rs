use crate::{
    network::{Event, Request},
    protocol::{self, Reply},
};
use std::{os::unix::net::UnixStream, sync::mpsc, time::Duration};

pub fn call(request: protocol::Request) -> Result<Reply, String> {
    let mut socket = connect(protocol::SOCKET)?;
    protocol::write(&request, &mut socket).map_err(|_| "System service disconnected")?;
    protocol::read(&mut socket).map_err(|_| "System service did not reply".into())
}
pub fn action(request: protocol::Request) -> Result<(), String> {
    match call(request)? {
        Reply::Done(result) => result,
        _ => Err("Unexpected system service reply".into()),
    }
}
fn connect(path: &str) -> Result<UnixStream, String> {
    let stream = UnixStream::connect(path).map_err(|_| "System service is unavailable")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(100)))
        .map_err(|_| "Could not configure system connection")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "Could not configure system connection")?;
    Ok(stream)
}
fn failure(request: &Request, error: String) -> Event {
    match request {
        Request::Hotspot => Event::Hotspot(Err(error)),
        Request::Scan => Event::Scanned(Err(error)),
        Request::Test { .. } => Event::Tested(Err(error)),
        Request::Save => Event::Saved(Err(error)),
        Request::Cancel => Event::Cancelled(Err(error)),
    }
}
fn finished(event: &Event) -> bool {
    matches!(
        event,
        Event::Scanned(_)
            | Event::Tested(Err(_))
            | Event::Saved(Ok(()))
            | Event::Cancelled(_)
            | Event::Expired(_)
    )
}

/// The UI owns input and presentation; the server owns trial lifetime/rollback.
pub struct Worker {
    tx: mpsc::Sender<Request>,
    pub rx: mpsc::Receiver<Event>,
}
impl Worker {
    pub fn send(&self, request: Request) {
        let _ = self.tx.send(request);
    }
    pub fn start() -> Self {
        Self::start_at(protocol::SOCKET.to_owned())
    }
    fn start_at(path: String) -> Self {
        let (tx, requests) = mpsc::channel::<Request>();
        let (events, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut cancelling = false;
            let mut connection: Option<(UnixStream, mpsc::Receiver<Result<Event, String>>)> = None;
            loop {
                match requests.recv_timeout(Duration::from_millis(25)) {
                    Ok(request) => {
                        if matches!(request, Request::Hotspot) {
                            if connection.is_some() {
                                let _ = events.send(Event::Hotspot(Err(
                                    "Finish or cancel the network test first".into(),
                                )));
                            } else {
                                let _ =
                                    events.send(Event::Hotspot(action(protocol::Request::Hotspot)));
                            }
                            continue;
                        }
                        cancelling |= matches!(request, Request::Cancel);
                        if connection.is_none() {
                            let result = (|| {
                                let mut socket = connect(&path)?;
                                protocol::write(&protocol::Request::Network, &mut socket)
                                    .map_err(|_| "System service disconnected")?;
                                match protocol::read::<Reply>(&mut socket)
                                    .map_err(|_| "System service disconnected")?
                                {
                                    Reply::Ready => {}
                                    Reply::Done(Err(error)) => return Err(error),
                                    _ => return Err("Unexpected system service reply".into()),
                                }
                                let mut reader = socket
                                    .try_clone()
                                    .map_err(|_| "Could not connect to system service")?;
                                let (tx, rx) = mpsc::sync_channel(8);
                                std::thread::spawn(move || loop {
                                    let event = protocol::read::<Event>(&mut reader).map_err(|_| "System service disconnected; connection recovery will run on reconnect".into());
                                    let stop = event.is_err();
                                    if tx.send(event).is_err() || stop {
                                        break;
                                    }
                                });
                                Ok((socket, rx))
                            })();
                            match result {
                                Ok(value) => connection = Some(value),
                                Err(error) => {
                                    cancelling = false;
                                    let _ = events.send(failure(&request, error));
                                    continue;
                                }
                            }
                        }
                        if let Some((socket, _)) = connection.as_mut() {
                            if protocol::write(&request, socket).is_err() {
                                let _ = events
                                    .send(failure(&request, "System service disconnected".into()));
                                let _ = socket.shutdown(std::net::Shutdown::Both);
                                connection = None;
                                cancelling = false;
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if let Some((_, replies)) = &connection {
                    if let Ok(reply) = replies.try_recv() {
                        let event = reply.unwrap_or_else(|error| Event::Expired(Err(error)));
                        let done = if cancelling {
                            matches!(event, Event::Cancelled(_) | Event::Expired(_))
                        } else {
                            finished(&event)
                        };
                        let _ = events.send(event);
                        if done {
                            cancelling = false;
                            if let Some((socket, _)) = connection.take() {
                                let _ = socket.shutdown(std::net::Shutdown::Both);
                            }
                        }
                    }
                }
            }
            if let Some((socket, _)) = connection {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        });
        Self { tx, rx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    fn fixture() -> (String, UnixListener) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = format!(
            "/tmp/couch-system-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let socket = UnixListener::bind(&path).unwrap();
        (path, socket)
    }
    fn accept(listener: &UnixListener) -> UnixStream {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert!(matches!(
            protocol::read::<protocol::Request>(&mut stream).unwrap(),
            protocol::Request::Network
        ));
        protocol::write(&Reply::Ready, &mut stream).unwrap();
        stream
    }
    #[test]
    fn cancelled_scan_waits_for_rollback_reply() {
        let (path, listener) = fixture();
        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            assert!(matches!(
                protocol::read::<Request>(&mut stream).unwrap(),
                Request::Scan
            ));
            assert!(matches!(
                protocol::read::<Request>(&mut stream).unwrap(),
                Request::Cancel
            ));
            protocol::write(&Event::Scanned(Err("Cancelled".into())), &mut stream).unwrap();
            protocol::write(&Event::Cancelled(Ok(())), &mut stream).unwrap();
            assert!(protocol::read::<Request>(&mut stream).is_err());
        });
        let worker = Worker::start_at(path.clone());
        worker.send(Request::Scan);
        worker.send(Request::Cancel);
        assert!(matches!(
            worker.rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Scanned(Err(_))
        ));
        assert!(matches!(
            worker.rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Cancelled(Ok(()))
        ));
        drop(worker);
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn dropped_ui_closes_trial_session_without_saving() {
        let (path, listener) = fixture();
        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            assert!(matches!(
                protocol::read::<Request>(&mut stream).unwrap(),
                Request::Test { .. }
            ));
            protocol::write(
                &Event::Tested(Ok(("Home".into(), "192.0.2.1".into()))),
                &mut stream,
            )
            .unwrap();
            assert!(protocol::read::<Request>(&mut stream).is_err());
        });
        let worker = Worker::start_at(path.clone());
        worker.send(Request::Test {
            ssid: "Home".into(),
            password: "password".into(),
        });
        assert!(matches!(
            worker.rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Tested(Ok(_))
        ));
        drop(worker);
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
