//! HAP-style Companion authentication. Verify both SRP proof and accessory
//! signatures before returning credentials; never trust a successful status alone.
use crate::{Credentials, Error, Result};
use chacha20poly1305::{
    aead::{Aead, Payload},
    ChaCha20Poly1305, KeyInit, Nonce,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use num_bigint::BigUint;
use rand::RngCore;
use sha2::{Digest, Sha512};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
pub fn derive(secret: &[u8], salt: &str, info: &str) -> Result<[u8; 32]> {
    let mut key = [0; 32];
    Hkdf::<Sha512>::new(Some(salt.as_bytes()), secret)
        .expand(info.as_bytes(), &mut key)
        .map_err(|_| Error::Crypto)?;
    Ok(key)
}
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    ChaCha20Poly1305::new(key.into())
        .encrypt(Nonce::from_slice(nonce), Payload { msg: data, aad })
        .map_err(|_| Error::Crypto)
}
pub fn open(key: &[u8; 32], nonce: &[u8; 12], data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    ChaCha20Poly1305::new(key.into())
        .decrypt(Nonce::from_slice(nonce), Payload { msg: data, aad })
        .map_err(|_| Error::Authentication)
}
fn label_nonce(label: &[u8; 8]) -> [u8; 12] {
    let mut n = [0; 12];
    n[4..].copy_from_slice(label);
    n
}
pub fn tlv(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = vec![];
    for (tag, value) in items {
        for chunk in value.chunks(255) {
            out.extend([*tag, chunk.len() as u8]);
            out.extend(chunk);
        }
        if value.is_empty() {
            out.extend([*tag, 0]);
        }
    }
    out
}
pub fn untlv(data: &[u8]) -> Result<Vec<(u8, Vec<u8>)>> {
    let mut out: Vec<(u8, Vec<u8>)> = vec![];
    let mut pos = 0;
    let mut previous = None;
    while pos < data.len() {
        if pos + 2 > data.len() {
            return Err(Error::Protocol);
        }
        let tag = data[pos];
        let n = data[pos + 1] as usize;
        pos += 2;
        if pos + n > data.len() {
            return Err(Error::Protocol);
        }
        if let Some((_, value)) = out.iter_mut().find(|(t, _)| *t == tag) {
            if previous != Some((tag, 255)) {
                return Err(Error::Protocol);
            }
            value.extend(&data[pos..pos + n]);
        } else {
            out.push((tag, data[pos..pos + n].to_vec()));
        }
        previous = Some((tag, n));
        pos += n;
    }
    if out.iter().any(|(tag, _)| *tag == 7) {
        return Err(Error::Authentication);
    }
    Ok(out)
}
pub fn field(values: &[(u8, Vec<u8>)], tag: u8) -> Result<&[u8]> {
    values
        .iter()
        .find(|(t, _)| *t == tag)
        .map(|(_, v)| v.as_slice())
        .ok_or(Error::Protocol)
}
pub fn sequence(values: &[(u8, Vec<u8>)], n: u8) -> Result<()> {
    if field(values, 6)? == [n] {
        Ok(())
    } else {
        Err(Error::Protocol)
    }
}
fn hash(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p)
    }
    h.finalize().into()
}
fn pad(value: &BigUint) -> Result<Vec<u8>> {
    let bytes = value.to_bytes_be();
    if bytes.len() > 384 {
        return Err(Error::Authentication);
    }
    let mut out = vec![0; 384 - bytes.len()];
    out.extend(bytes);
    Ok(out)
}
pub struct Setup {
    signing: SigningKey,
    client_id: Vec<u8>,
    private: [u8; 32],
    pub public: Vec<u8>,
    pub proof: [u8; 64],
    expected: [u8; 64],
    key: [u8; 64],
}
impl Setup {
    pub fn new(pin: &str, salt: &[u8], server: &[u8]) -> Result<Self> {
        Self::with_pin(pin, salt, server, false)
    }
    pub fn new_airplay(pin: &str, salt: &[u8], server: &[u8]) -> Result<Self> {
        Self::with_pin(pin, salt, server, true)
    }
    fn with_pin(pin: &str, salt: &[u8], server: &[u8], airplay: bool) -> Result<Self> {
        if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::InvalidPin);
        }
        if salt.len() < 16 || salt.len() > 64 || server.is_empty() || server.len() > 384 {
            return Err(Error::Authentication);
        }
        let mut private = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut private);
        // Companion hashes a decimal PIN, but AirPlay hashes exactly four
        // characters (pyatv AirPlayPairingHandler.finish uses zfill(4)).
        let normalized = if airplay {
            pin.to_owned()
        } else {
            pin.parse::<u16>()
                .map_err(|_| Error::InvalidPin)?
                .to_string()
        };
        let (public, proof, expected, key) =
            srp_exchange(&private, normalized.as_bytes(), salt, server)?;
        Ok(Self {
            signing: SigningKey::generate(&mut rand::rngs::OsRng),
            client_id: uuid::Uuid::new_v4().to_string().into_bytes(),
            private,
            public,
            proof,
            expected,
            key,
        })
    }
    pub fn finish_proof(&self, server_proof: &[u8]) -> Result<Vec<u8>> {
        if !bool::from(self.expected.as_slice().ct_eq(server_proof)) {
            return Err(Error::Authentication);
        }
        let x = derive(
            &self.key,
            "Pair-Setup-Controller-Sign-Salt",
            "Pair-Setup-Controller-Sign-Info",
        )?;
        let public = self.signing.verifying_key().to_bytes();
        let data = [x.as_slice(), &self.client_id, &public].concat();
        let signature = self.signing.sign(&data).to_bytes();
        let key = derive(
            &self.key,
            "Pair-Setup-Encrypt-Salt",
            "Pair-Setup-Encrypt-Info",
        )?;
        seal(
            &key,
            &label_nonce(b"PS-Msg05"),
            &tlv(&[(1, &self.client_id), (3, &public), (10, &signature)]),
            &[],
        )
    }
    pub fn finish(self, encrypted: &[u8]) -> Result<Credentials> {
        let key = derive(
            &self.key,
            "Pair-Setup-Encrypt-Salt",
            "Pair-Setup-Encrypt-Info",
        )?;
        let data = open(&key, &label_nonce(b"PS-Msg06"), encrypted, &[])?;
        let fields = untlv(&data)?;
        let id = field(&fields, 1)?;
        let public: [u8; 32] = field(&fields, 3)?
            .try_into()
            .map_err(|_| Error::Authentication)?;
        let signature =
            Signature::from_slice(field(&fields, 10)?).map_err(|_| Error::Authentication)?;
        let x = derive(
            &self.key,
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
        )?;
        VerifyingKey::from_bytes(&public)
            .map_err(|_| Error::Authentication)?
            .verify_strict(&[x.as_slice(), id, &public].concat(), &signature)
            .map_err(|_| Error::Authentication)?;
        Ok(Credentials {
            client_id: self.client_id.clone(),
            client_secret: self.signing.to_bytes(),
            device_id: id.to_vec(),
            device_public: public,
        })
    }
}
impl Drop for Setup {
    fn drop(&mut self) {
        self.private.fill(0);
        self.key.fill(0);
    }
}
type SrpExchange = (Vec<u8>, [u8; 64], [u8; 64], [u8; 64]);

fn srp_exchange(private: &[u8], pin: &[u8], salt: &[u8], server: &[u8]) -> Result<SrpExchange> {
    let group = &srp::groups::G_3072;
    let client = srp::client::SrpClient::<Sha512>::new(group);
    let a = BigUint::from_bytes_be(private);
    let a_pub = client.compute_a_pub(&a);
    let b = BigUint::from_bytes_be(server);
    if &b % &group.n == BigUint::default() {
        return Err(Error::Authentication);
    }
    let u = BigUint::from_bytes_be(&hash(&[&pad(&a_pub)?, &pad(&b)?]));
    if u == BigUint::default() {
        return Err(Error::Authentication);
    }
    let k = BigUint::from_bytes_be(&hash(&[&group.n.to_bytes_be(), &pad(&group.g)?]));
    let x = BigUint::from_bytes_be(&hash(&[salt, &hash(&[b"Pair-Setup:", pin])]));
    let shared = client.compute_premaster_secret(&b, &k, &x, &a, &u);
    let key = hash(&[&shared.to_bytes_be()]);
    let hn = hash(&[&group.n.to_bytes_be()]);
    let hg = hash(&[&group.g.to_bytes_be()]);
    let xor: Vec<u8> = hn.into_iter().zip(hg).map(|(a, b)| a ^ b).collect();
    let public = a_pub.to_bytes_be();
    let proof = hash(&[
        &xor,
        &hash(&[b"Pair-Setup"]),
        salt,
        &public,
        &b.to_bytes_be(),
        &key,
    ]);
    let expected = hash(&[&public, &proof, &key]);
    Ok((public, proof, expected, key))
}
pub struct Verify {
    secret: StaticSecret,
    pub public: [u8; 32],
}
impl Verify {
    pub fn new() -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }
    pub fn reply(
        self,
        creds: &Credentials,
        server: &[u8],
        encrypted: &[u8],
    ) -> Result<(Vec<u8>, [u8; 32], [u8; 32])> {
        let (response, shared) = self.reply_shared(creds, server, encrypted)?;
        Ok((
            response,
            derive(&shared, "", "ClientEncrypt-main")?,
            derive(&shared, "", "ServerEncrypt-main")?,
        ))
    }
    pub fn reply_shared(
        self,
        creds: &Credentials,
        server: &[u8],
        encrypted: &[u8],
    ) -> Result<(Vec<u8>, [u8; 32])> {
        let server: [u8; 32] = server.try_into().map_err(|_| Error::Authentication)?;
        let shared = self.secret.diffie_hellman(&PublicKey::from(server));
        if !shared.was_contributory() {
            return Err(Error::Authentication);
        }
        let key = derive(
            shared.as_bytes(),
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
        )?;
        let data = open(&key, &label_nonce(b"PV-Msg02"), encrypted, &[])?;
        let fields = untlv(&data)?;
        if field(&fields, 1)? != creds.device_id {
            return Err(Error::Authentication);
        }
        let signature =
            Signature::from_slice(field(&fields, 10)?).map_err(|_| Error::Authentication)?;
        VerifyingKey::from_bytes(&creds.device_public)
            .map_err(|_| Error::Authentication)?
            .verify_strict(
                &[server.as_slice(), &creds.device_id, &self.public].concat(),
                &signature,
            )
            .map_err(|_| Error::Authentication)?;
        let signature = SigningKey::from_bytes(&creds.client_secret)
            .sign(&[self.public.as_slice(), &creds.client_id, &server].concat())
            .to_bytes();
        let response = seal(
            &key,
            &label_nonce(b"PV-Msg03"),
            &tlv(&[(1, &creds.client_id), (10, &signature)]),
            &[],
        )?;
        Ok((response, *shared.as_bytes()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tlv_fragments_roundtrip_and_reject_duplicate_short_fields() {
        let data = vec![42; 600];
        assert_eq!(
            field(&untlv(&tlv(&[(3, &data)])).unwrap(), 3).unwrap(),
            data
        );
        for v in [vec![3, 1, 1, 3, 1, 2], vec![6, 2, 1], vec![7, 1, 2]] {
            assert!(untlv(&v).is_err());
        }
    }
    #[test]
    fn encryption_authenticates_header_and_payload() {
        let key = [7; 32];
        let nonce = [0; 12];
        let c = seal(&key, &nonce, b"payload", b"header").unwrap();
        assert_eq!(open(&key, &nonce, &c, b"header").unwrap(), b"payload");
        assert!(open(&key, &nonce, &c, b"different").is_err());
    }
    #[test]
    fn reject_invalid_pins_and_zero_server_public() {
        assert!(Setup::new("123", &[1; 16], &[1; 384]).is_err());
        assert!(Setup::new("1234", &[1; 16], &[0; 384]).is_err());
    }
    #[test]
    fn hap_srp_matches_independent_srptools_1_0_1_vector() {
        fn hex(s: &str) -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        }
        let salt = hex("00112233445566778899aabbccddeeff");
        let server=hex("d7cf4efb52417757b0cd43dd4b6dc336c4d70e241d57343490df6db510abf0c8dffafe6362262a38f3a712acd348cf57b8ec7bfd7879ba12e5f1422b604241aae6225a53d23cf63efbcfae73935f1df16bbd2db4b04a60c59e3ec6c3b64b99445327a3b469bc0788483e467ca09384574ce3258fc2c1367b37d1c4f5b5827ba7d9941f9bba637505a39aadfe1bbaae2ce2cc3b4c3998d410880393b52a2220553b63e144f14e8c1807f4a4cde700cd3398994acffb32c9e028308aaf29c36e92617b0ef40cd1e1cc83e0c002bb8cee81ba1df725b643692a770041a97a28a8f0e2c4b041a8c5cde7353b977473f1c10883bf04e98cad089c9218f314feaa4b64368ae4e1ed0157798dffc37fe0bc7a96997a924e3b817e07dfe833afb6da36b0e1422c36212ecba108cbba411bb1b46914cefdfbfea3ab4ec676fe069fe53bfbc5ffd6049d99fdc70c21ba1344c97113a4e42af7771707c5ee096bae03ff0fb4e5b48e4d658dd2ef8038c8782e75ac81aa5e902da3c9b149681ee61d02cefcaf");
        let (public, proof, expected, key) =
            srp_exchange(&[0x11; 32], b"1234", &salt, &server).unwrap();
        assert_eq!(public,hex("c95341372ca4d0b469f87c0ae37bda635713223b577ef5e8d64a959a54e5395722fea50e71bbd44306055760924a64885805ae13545fcfc7e65f4e7b75c765a112d267f79bffb8353d96ae81cfa64ca7eb687a5dba4cf223b21e1362489ca3e056254b25f610c00643490ea19944f6d3d0872bdcc5fc338bebc8936f30b695924e117f364a4cfc3898eeaa1a4bb98957df8eb046eba3cf42558138ca616a1dc7991426292dd258693bbd4c11816e3064e4a7c536f7255ae61735db25e3cf0349eb8b3e9848d0aa572b0f809d8983f1b280581aba96104422fcf088b698f8d899115a5653cc473e87f5e45552a5a126c8b7fc273bd2cd12bfb15ff3501a572b17608f16c33f6e971081f3af1be37a0a96748f60d204e935f6a5d2a71303aea434b45bcb9ab6b4444208d3fdf7a00c07a3ba2a09b56b578170c78ba1573ca2692406ba8d461e43bb3d160f0024a388d7234236537e29c3604f666c69d7244eae29a0e8b9fa5ee490890e5892973fa535707fbe8f06e90dcbb99c7b39b5eb2b45fb"));
        assert_eq!(proof.as_slice(),hex("46b6dc08b52d7663d391bafc6a376bc365271985e112c5c9a6ce7e00d2c992a71fdba85c07b1b42c6e42d294d0b87665d531bf6c4daae73539fc1fd20ec98b7d"));
        assert_eq!(expected.as_slice(),hex("e3adabdc41ec272680ae0ed71066b7a711beec055ab4c56758cc0b3ad0ba20670d83c5f073671ec7c67fcb9050ea46fc7532cdd877bad0f2898e9ca82172d31a"));
        assert_eq!(key.as_slice(),hex("9e3a14995d2b65bcb7d9b9d459c33e21a86cd2ce9bb4ba0793d528c7545d1a72cb9bb7b484abd8c2ce2212285fe7eb754b7635e7c9b970cae980ce71901cff1b"));
    }
    #[test]
    fn airplay_leading_zero_pin_matches_independent_srptools_vector() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("metadata/fixtures/airplay-srp-0123.json")).unwrap();
        let hex = |name: &str| {
            let value = fixture[name].as_str().unwrap();
            (0..value.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        let (private, salt, server) = (hex("private"), hex("salt"), hex("server"));
        let (public, proof, expected, key) =
            srp_exchange(&private, b"0123", &salt, &server).unwrap();
        assert_eq!(public, hex("public"));
        assert_eq!(proof.as_slice(), hex("proof"));
        assert_eq!(expected.as_slice(), hex("server_proof"));
        assert_eq!(key.as_slice(), hex("key"));
        let airplay = Setup::new_airplay("0123", &salt, &server).unwrap();
        let (_, expected, _, _) = srp_exchange(&airplay.private, b"0123", &salt, &server).unwrap();
        assert_eq!(airplay.proof, expected);
        let companion = Setup::new("0123", &salt, &server).unwrap();
        let (_, expected, _, _) = srp_exchange(&companion.private, b"123", &salt, &server).unwrap();
        assert_eq!(companion.proof, expected);
    }
    #[test]
    fn setup_requires_server_proof_and_accessory_signature() {
        let make = || Setup::new("1234", &[7; 16], &[1; 384]).unwrap();
        let setup = make();
        assert!(setup.finish_proof(&[0; 64]).is_err());
        assert!(setup.finish_proof(&setup.expected).is_ok());
        let accessory = SigningKey::from_bytes(&[77; 32]);
        let public = accessory.verifying_key().to_bytes();
        let id = b"test-accessory";
        let x = derive(
            &setup.key,
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
        )
        .unwrap();
        let signature = accessory
            .sign(&[x.as_slice(), id, &public].concat())
            .to_bytes();
        let key = derive(
            &setup.key,
            "Pair-Setup-Encrypt-Salt",
            "Pair-Setup-Encrypt-Info",
        )
        .unwrap();
        let encrypted = seal(
            &key,
            &label_nonce(b"PS-Msg06"),
            &tlv(&[(1, id), (3, &public), (10, &signature)]),
            &[],
        )
        .unwrap();
        let credentials = setup.finish(&encrypted).unwrap();
        assert_eq!(credentials.device_id, id);
        let setup = make();
        let key = derive(
            &setup.key,
            "Pair-Setup-Encrypt-Salt",
            "Pair-Setup-Encrypt-Info",
        )
        .unwrap();
        let forged = seal(
            &key,
            &label_nonce(b"PS-Msg06"),
            &tlv(&[(1, id), (3, &public), (10, &[0; 64])]),
            &[],
        )
        .unwrap();
        assert!(setup.finish(&forged).is_err());
    }
}
