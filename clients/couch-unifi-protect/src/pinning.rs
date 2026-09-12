//! Exact-certificate trust for explicitly approved local console origins.
//! Uses ureq's unversioned transport API, hence the exact ureq dependency pin.
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    sync::Arc,
};
use ureq::unversioned::{
    resolver::DefaultResolver,
    transport::{
        Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, TcpConnector, Transport,
        TransportAdapter,
    },
};
use url::Url;

#[derive(Debug)]
struct PinnedVerifier([u8; 32]);
impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual: [u8; 32] = Sha256::digest(cert.as_ref()).into();
        if actual != self.0 {
            return Err(rustls::Error::General(
                "Console certificate pin mismatch".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signed,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signed,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
#[derive(Debug)]
struct PinnedConnector {
    config: Arc<ClientConfig>,
    origin: url::Origin,
}
impl<T: Transport> Connector<T> for PinnedConnector {
    type Out = PinnedTransport;
    fn connect(
        &self,
        details: &ConnectionDetails,
        transport: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let target = Url::parse(&details.uri.to_string())
            .map_err(|_| ureq::Error::Tls("Invalid pinned origin"))?;
        if target.scheme() != "https" || target.origin() != self.origin {
            return Err(ureq::Error::Tls("Pinned certificate origin mismatch"));
        }
        let transport = transport.ok_or(ureq::Error::Tls("Missing underlying transport"))?;
        let host = target
            .host_str()
            .ok_or(ureq::Error::Tls("Missing pinned host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let name =
            ServerName::try_from(host).map_err(|_| ureq::Error::Tls("Invalid pinned host"))?;
        let mut conn = ClientConnection::new(self.config.clone(), name)?;
        let mut socket = TransportAdapter::new(transport.boxed());
        socket.set_timeout(details.timeout);
        // Finish certificate AND handshake-signature validation before returning
        // a transport on which ureq can write HTTP headers (including API key).
        while conn.is_handshaking() {
            conn.complete_io(&mut socket)?;
        }
        Ok(Some(PinnedTransport {
            stream: StreamOwned::new(conn, socket),
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
        }))
    }
}
struct PinnedTransport {
    stream: StreamOwned<ClientConnection, TransportAdapter>,
    buffers: LazyBuffers,
}
impl std::fmt::Debug for PinnedTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedTlsTransport")
    }
}
impl Transport for PinnedTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.stream.sock.set_timeout(timeout);
        self.stream.write_all(&self.buffers.output()[..amount])?;
        Ok(())
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream.sock.set_timeout(timeout);
        let count = self.stream.read(self.buffers.input_append_buf())?;
        self.buffers.input_appended(count);
        Ok(count > 0)
    }
    fn is_open(&mut self) -> bool {
        self.stream.sock.get_mut().is_open()
    }
    fn is_tls(&self) -> bool {
        true
    }
}
pub(super) fn agent(
    config: ureq::config::Config,
    origin: &Url,
    pin: &str,
) -> super::Result<ureq::Agent> {
    let tls = tls_config(pin)?;
    let connector = TcpConnector::default().chain(PinnedConnector {
        config: Arc::new(tls),
        origin: origin.origin(),
    });
    Ok(ureq::Agent::with_parts(
        config,
        connector,
        DefaultResolver::default(),
    ))
}

pub(crate) fn tls_config(pin: &str) -> super::Result<ClientConfig> {
    if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(super::Error::Configuration);
    }
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&pin[i * 2..i * 2 + 2], 16)
            .map_err(|_| super::Error::Configuration)?;
    }
    let tls =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| super::Error::Configuration)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedVerifier(bytes)))
            .with_no_client_auth();
    Ok(tls)
}
