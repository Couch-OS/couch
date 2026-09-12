//! Absolute deadlines and process-wide bounded DNS for the media worker.
use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
#[derive(Default)]
pub struct Cancellation {
    stopped: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}
impl Cancellation {
    pub fn cancel(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(s) = self.socket.lock().unwrap().as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    fn register(&self, socket: &TcpStream) -> io::Result<()> {
        let mut slot = self.socket.lock().unwrap();
        *slot = Some(socket.try_clone()?);
        if self.is_cancelled() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        Ok(())
    }
}
pub(crate) struct Socket {
    pub(crate) stream: TcpStream,
    deadline: Instant,
    cancel: Arc<Cancellation>,
}
impl Socket {
    pub(crate) fn shorten(&mut self, deadline: Instant) {
        self.deadline = self.deadline.min(deadline);
    }
    pub(crate) fn new(
        stream: TcpStream,
        deadline: Instant,
        cancel: Arc<Cancellation>,
    ) -> io::Result<Self> {
        cancel.register(&stream)?;
        Ok(Self {
            stream,
            deadline,
            cancel,
        })
    }
    fn remaining(&self) -> io::Result<Duration> {
        if self.cancel.is_cancelled() {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .map(|d| d.min(Duration::from_secs(2)))
            .ok_or_else(|| io::ErrorKind::TimedOut.into())
    }
}
impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buf)
    }
}
impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.remaining()?;
        self.stream.flush()
    }
}
type Job = (String, u16, mpsc::SyncSender<Vec<SocketAddr>>);
fn resolver() -> &'static mpsc::SyncSender<Job> {
    static SERVICE: OnceLock<mpsc::SyncSender<Job>> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        std::thread::spawn(move || {
            while let Ok((host, port, answer)) = receiver.recv() {
                let addresses = (host.as_str(), port)
                    .to_socket_addrs()
                    .map(|v| v.take(4).collect())
                    .unwrap_or_default();
                let _ = answer.try_send(addresses);
            }
        });
        sender
    })
}
pub(crate) fn resolve(
    host: &str,
    port: u16,
    deadline: Instant,
    cancel: &Cancellation,
) -> io::Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    resolve_with(
        resolver(),
        host,
        port,
        deadline.min(Instant::now() + Duration::from_secs(2)),
        cancel,
    )
}
fn resolve_with(
    sender: &mpsc::SyncSender<Job>,
    host: &str,
    port: u16,
    deadline: Instant,
    cancel: &Cancellation,
) -> io::Result<Vec<SocketAddr>> {
    let (answer, receiver) = mpsc::sync_channel(1);
    sender
        .try_send((host.into(), port, answer))
        .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
    loop {
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        match receiver.recv_timeout(remaining.min(Duration::from_millis(25))) {
            Ok(addresses) => return Ok(addresses),
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(_) => return Err(io::ErrorKind::NotFound.into()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trickle_cannot_extend_absolute_read_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..50 {
                if server.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        let start = Instant::now();
        let mut socket = Socket::new(
            client,
            start + Duration::from_millis(80),
            Arc::new(Cancellation::default()),
        )
        .unwrap();
        assert!(socket.read_exact(&mut [0; 100]).is_err());
        assert!(start.elapsed() < Duration::from_millis(400));
        drop(socket);
        worker.join().unwrap();
    }
    #[test]
    fn cancellation_interrupts_a_blocked_socket_read() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let cancel = Arc::new(Cancellation::default());
        let mut socket = Socket::new(
            client,
            Instant::now() + Duration::from_secs(60),
            cancel.clone(),
        )
        .unwrap();
        let worker = std::thread::spawn(move || socket.read_exact(&mut [0; 4]));
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        cancel.cancel();
        assert!(worker.join().unwrap().is_err());
        assert!(start.elapsed() < Duration::from_millis(400));
    }
    #[test]
    fn blocked_dns_is_bounded_and_cancellable_without_new_workers() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(Cancellation::default());
        let stop = cancel.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            stop.cancel();
        });
        let start = Instant::now();
        assert!(resolve_with(
            &sender,
            "fixture.invalid",
            7441,
            start + Duration::from_secs(60),
            &cancel
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_millis(400));
        worker.join().unwrap();
        assert!(resolve_with(
            &sender,
            "fixture.invalid",
            7441,
            Instant::now() + Duration::from_millis(20),
            &Cancellation::default()
        )
        .is_err());
    }
}
