//! Hue certificates name the bridge ID, not its IP. Trust on first pairing,
//! then pin the exact certificate (`couch_sdk::tls::Pin`, shared with the TV
//! clients). What is local to Hue is the ureq transport underneath it.
use couch_sdk::tls::Pin;
use rustls::{pki_types::ServerName, ClientConfig, ClientConnection, StreamOwned};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
};
use ureq::unversioned::{
    resolver::DefaultResolver,
    transport::{
        Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, TcpConnector, Transport,
        TransportAdapter,
    },
};
#[derive(Debug)]
struct PinnedConnector(Arc<ClientConfig>, bool);
impl<In: Transport> Connector<In> for PinnedConnector {
    type Out = PinnedTransport;
    fn connect(
        &self,
        d: &ConnectionDetails,
        input: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let input = input.ok_or(ureq::Error::Tls("Missing TCP connection"))?;
        let name = ServerName::try_from(
            d.uri
                .host()
                .ok_or(ureq::Error::Tls("Missing host"))?
                .to_string(),
        )
        .map_err(|_| ureq::Error::Tls("Invalid host"))?;
        let mut conn = ClientConnection::new(self.0.clone(), name)?;
        let mut sock = TransportAdapter::new(input.boxed());
        sock.set_timeout(d.timeout);
        conn.complete_io(&mut sock)?;
        Ok(Some(PinnedTransport {
            stream: StreamOwned::new(conn, sock),
            buffers: LazyBuffers::new(8192, 8192),
            stream_timeout: self.1,
        }))
    }
}
struct PinnedTransport {
    stream: StreamOwned<ClientConnection, TransportAdapter>,
    buffers: LazyBuffers,
    stream_timeout: bool,
}
impl Transport for PinnedTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, n: usize, t: NextTimeout) -> Result<(), ureq::Error> {
        self.stream.sock.set_timeout(t);
        self.stream.write_all(&self.buffers.output()[..n])?;
        Ok(())
    }
    fn await_input(&mut self, mut t: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream.sock.set_timeout(t);
        // Bound each idle read, not the lifetime of an active SSE response.
        if self.stream_timeout {
            t.after = t.after.min(std::time::Duration::from_secs(45).into());
            self.stream.sock.set_timeout(t);
        }
        let n = self.stream.read(self.buffers.input_append_buf())?;
        self.buffers.input_appended(n);
        Ok(n > 0)
    }
    fn is_open(&mut self) -> bool {
        self.stream.sock.get_mut().is_open()
    }
    fn is_tls(&self) -> bool {
        true
    }
}
pub fn agent(certificate: Arc<Mutex<Vec<u8>>>) -> ureq::Agent {
    make_agent(certificate, false)
}
pub fn stream_agent(certificate: Arc<Mutex<Vec<u8>>>) -> ureq::Agent {
    make_agent(certificate, true)
}
fn make_agent(certificate: Arc<Mutex<Vec<u8>>>, stream: bool) -> ureq::Agent {
    let tls = couch_sdk::tls::pinned_client_config(Arc::new(Pin::new(
        certificate,
        "Hue bridge certificate changed; pair again",
    )))
    .expect("TLS versions");
    let cfg = ureq::Agent::config_builder()
        .timeout_global(if stream {
            None
        } else {
            Some(std::time::Duration::from_secs(5))
        })
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(5)))
        .max_redirects(0)
        .proxy(None)
        .build();
    ureq::Agent::with_parts(
        cfg,
        TcpConnector::default().chain(PinnedConnector(Arc::new(tls), stream)),
        DefaultResolver::default(),
    )
}
impl std::fmt::Debug for PinnedTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedTransport").finish_non_exhaustive()
    }
}
