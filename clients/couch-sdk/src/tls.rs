//! Trust a device certificate during explicit pairing; pin it on reconnect.
//!
//! LAN integrations such as LG webOS, Android TV, Samsung Tizen and Hue all
//! face the same problem: a device presents a self-signed certificate that no
//! public root can verify. Reference clients disable verification entirely;
//! Couch integrations instead record the certificate seen while the user
//! approves pairing and refuse any other one until the device is paired again.
//!
//! The shared verifier lives here so independently packaged and core clients
//! follow the same rule. [`Pin::changed`] supplies the device-specific wording.

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    client::WantsClientCert,
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, ConfigBuilder, DigitallySignedStruct, SignatureScheme,
};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Trust on first use, then exact-match.
///
/// `certificate` is empty until a pairing handshake fills it in, and holds the
/// pinned DER afterwards. It is shared with the client so the value the pairing
/// observed can be saved alongside the rest of the credential.
#[derive(Debug)]
pub struct Pin {
    pub certificate: Arc<Mutex<Vec<u8>>>,
    /// Shown verbatim to whoever is holding the remote, so it names the device
    /// they are looking at rather than a TLS concept.
    pub changed: &'static str,
}

impl Pin {
    pub fn new(certificate: Arc<Mutex<Vec<u8>>>, changed: &'static str) -> Self {
        Self {
            certificate,
            changed,
        }
    }
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut pin = self
            .certificate
            .lock()
            .map_err(|_| rustls::Error::General("Certificate lock failed".into()))?;
        if pin.is_empty() {
            *pin = cert.as_ref().to_vec();
        }
        if pin.as_slice() != cert.as_ref() {
            return Err(rustls::Error::General(self.changed.into()));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, s)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, s)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_verify_schemes()
    }
}

/// The half of a verifier that must keep working when the certificate check is
/// replaced. Exposed as free functions so a client pinning something other than
/// the whole certificate - a digest, say - does not copy these three either.
pub fn verify_tls12_signature(
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

pub fn verify_tls13_signature(
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

pub fn supported_verify_schemes() -> Vec<SignatureScheme> {
    rustls::crypto::ring::default_provider()
        .signature_verification_algorithms
        .supported_schemes()
}

/// The ring provider, safe protocol versions and `verifier`, stopping short of
/// client authentication because that is the one step these clients differ on:
/// Android TV presents a client certificate and the rest present none.
pub fn pinned_config_builder(
    verifier: Arc<dyn ServerCertVerifier>,
) -> Result<ConfigBuilder<ClientConfig, WantsClientCert>, rustls::Error> {
    Ok(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier),
    )
}

/// The common case: a pinned server certificate and no client certificate.
pub fn pinned_client_config(
    verifier: Arc<dyn ServerCertVerifier>,
) -> Result<ClientConfig, rustls::Error> {
    Ok(pinned_config_builder(verifier)?.with_no_client_auth())
}

/// Plain TCP or TLS over it, chosen by the URL scheme.
///
/// Both TV clients reach the same device over `ws://` on one port and `wss://`
/// on another, so the WebSocket above them has to be generic over which one it
/// got.
pub enum Socket {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Socket {
    /// Never zero: a zero timeout means "block forever" to the kernel, which is
    /// the opposite of what a caller counting down a deadline is asking for.
    pub fn timeout(&self, t: Duration) -> std::io::Result<()> {
        let s = match self {
            Self::Plain(s) => s,
            Self::Tls(s) => &s.sock,
        };
        s.set_read_timeout(Some(t.max(Duration::from_millis(1))))?;
        s.set_write_timeout(Some(t.max(Duration::from_millis(1))))
    }
}

impl Read for Socket {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(b),
            Self::Tls(s) => s.read(b),
        }
    }
}

impl Write for Socket {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(b),
            Self::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_certificate_is_rejected() {
        let pin = Pin::new(
            Arc::new(Mutex::new(vec![1, 2, 3])),
            "Hue bridge certificate changed; pair again",
        );
        let name = ServerName::try_from("bridge").unwrap();
        assert!(pin
            .verify_server_cert(
                &CertificateDer::from(vec![1, 2, 3]),
                &[],
                &name,
                &[],
                UnixTime::since_unix_epoch(std::time::Duration::ZERO)
            )
            .is_ok());
        assert!(pin
            .verify_server_cert(
                &CertificateDer::from(vec![1, 2, 4]),
                &[],
                &name,
                &[],
                UnixTime::since_unix_epoch(std::time::Duration::ZERO)
            )
            .is_err());
    }

    #[test]
    fn an_empty_pin_takes_the_first_certificate_and_reports_the_devices_own_message() {
        let pin = Pin::new(Arc::new(Mutex::new(Vec::new())), "Boat certificate changed");
        let name = ServerName::try_from("tv").unwrap();
        let epoch = UnixTime::since_unix_epoch(std::time::Duration::ZERO);
        assert!(pin
            .verify_server_cert(&CertificateDer::from(vec![9, 9]), &[], &name, &[], epoch)
            .is_ok());
        assert_eq!(*pin.certificate.lock().unwrap(), vec![9, 9]);
        // The pairing's certificate is now the only one accepted, and the
        // per-device sentence is what surfaces.
        let error = pin
            .verify_server_cert(&CertificateDer::from(vec![9, 8]), &[], &name, &[], epoch)
            .unwrap_err();
        assert!(error.to_string().contains("Boat certificate changed"));
    }
}
