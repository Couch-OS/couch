//! The feed's signed metadata (`feed.json`), and the rules for following a
//! redirect while fetching it.
//!
//! A signed Alpine index proves who made it, not when. Anyone between a
//! remote and a feed could keep serving an old, validly signed index, or an
//! older one than the remote already saw, and so hide a fixed package. The
//! metadata adds what the index lacks: a number that only goes up, an expiry
//! date, the hash of the one index it describes, and for each package its
//! size, hash and the Couch it needs. Everything here is a pure function of
//! its arguments; `management` does the downloading and keeps the state.
use super::{err, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const MAX_METADATA: u64 = 256 * 1024;
pub(crate) const MAX_SIGNATURE: u64 = 1024;
/// How many redirects one download may follow.
pub(crate) const MAX_REDIRECTS: usize = 3;
/// A remote can start with its clock unset. A clock more than this far before
/// the metadata was issued is not trusted to judge an expiry date.
const CLOCK_SLACK: u64 = 24 * 60 * 60;
const MAX_PACKAGES: usize = 1024;

pub(crate) const OLDER_THAN_SEEN: &str =
    "The package feed is older than one this remote has already seen";
pub(crate) const NEEDS_NEWER_COUCH: &str = "Needs a newer Couch";

/// What a remote remembers about one repository, by repository id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Seen {
    pub sequence: u64,
    pub seen_metadata: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Package {
    pub id: String,
    pub version: String,
    pub apk: String,
    pub size: u64,
    pub sha256: String,
    pub protocol_version: u32,
    // A feed always writes it; a manifest that leaves it out means 1.
    #[serde(default = "one")]
    pub min_core_protocol_version: u32,
}
fn one() -> u32 {
    1
}
#[derive(Deserialize)]
struct Index {
    path: String,
    size: u64,
    sha256: String,
}
// Unknown keys are ignored, so a later feed can say more to a later Couch.
#[derive(Deserialize)]
struct Document {
    channel: String,
    sequence: u64,
    issued: String,
    expires: String,
    index: Index,
    packages: Vec<Package>,
}

/// What the feed served beside its index.
pub(crate) enum Served<'a> {
    /// No `feed.json` there.
    Missing,
    /// A `feed.json`, and its detached signature if that was there too.
    Metadata {
        document: &'a [u8],
        signature: Option<&'a [u8]>,
    },
}

pub(crate) struct Check<'a> {
    /// The repository's trusted public key, PEM SubjectPublicKeyInfo: the key
    /// apk checks the index and the packages with.
    pub public_key: &'a str,
    pub seen: Seen,
    /// Whether this repository may no longer be used without metadata.
    pub required: bool,
    /// The channel the metadata must name. Official repositories only.
    pub channel: Option<&'a str>,
    /// The downloaded `APKINDEX.tar.gz`, byte for byte.
    pub index: &'a [u8],
    /// Seconds since the Unix epoch, by this remote's clock.
    pub now: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Verified {
    pub sequence: u64,
    pub packages: Vec<Package>,
    /// The clock was too far behind to judge the expiry date, which was
    /// therefore not checked. The caller says so in the log.
    pub clock_unreliable: bool,
}

/// `Ok(None)`: there is no usable metadata and this repository does not need
/// any yet, so its index is used as it always was. An error is why the
/// repository must not be used on this refresh.
pub(crate) fn check(served: Served, check: &Check) -> Result<Option<Verified>> {
    let Served::Metadata {
        document,
        signature,
    } = served
    else {
        return unusable(check, "The package feed's signed metadata is missing");
    };
    if document.len() as u64 > MAX_METADATA {
        return Err(err("The package feed's metadata exceeds its size limit"));
    }
    let invalid_signature = || err("The package feed's metadata signature is not valid");
    let signature = signature.ok_or_else(invalid_signature)?;
    if signature.len() as u64 > MAX_SIGNATURE {
        return Err(invalid_signature());
    }
    verify(check.public_key, document, signature).map_err(|_| invalid_signature())?;
    let invalid = || err("The package feed's metadata is not valid");
    let value: serde_json::Value = serde_json::from_slice(document).map_err(|_| invalid())?;
    match value.get("schema").and_then(serde_json::Value::as_u64) {
        Some(1) => {}
        // Signed by the feed, in a format from after this Couch was built.
        Some(schema) if schema > 1 => {
            return unusable(
                check,
                "The package feed's metadata is in a newer format. Update Couch to use this feed",
            )
        }
        _ => return Err(invalid()),
    }
    let document: Document = serde_json::from_value(value).map_err(|_| invalid())?;
    let (issued, expires) = match (timestamp(&document.issued), timestamp(&document.expires)) {
        (Some(issued), Some(expires)) if issued <= expires => (issued, expires),
        _ => return Err(invalid()),
    };
    if document.packages.len() > MAX_PACKAGES {
        return Err(invalid());
    }
    if check
        .channel
        .is_some_and(|channel| channel != document.channel)
    {
        return Err(err(
            "The package feed's metadata belongs to another channel",
        ));
    }
    if document.sequence < check.seen.sequence {
        return Err(err(OLDER_THAN_SEEN));
    }
    let clock_unreliable = check.now.saturating_add(CLOCK_SLACK) < issued;
    if !clock_unreliable && check.now > expires {
        return Err(err(format!(
            "The package feed's signed metadata expired on {}. Check the remote's date and time",
            document.expires
        )));
    }
    if document.index.path != "APKINDEX.tar.gz"
        || document.index.size != check.index.len() as u64
        || !document
            .index
            .sha256
            .eq_ignore_ascii_case(&sha256_hex(check.index))
    {
        return Err(err(
            "The repository index is not the one the package feed's signed metadata describes",
        ));
    }
    Ok(Some(Verified {
        sequence: document.sequence,
        packages: document.packages,
        clock_unreliable,
    }))
}

fn unusable(check: &Check, reason: &str) -> Result<Option<Verified>> {
    if check.required {
        Err(err(reason))
    } else {
        Ok(None)
    }
}

/// Whether a repository must come with valid metadata. A repository the owner
/// added is trusted on first use: metadata is optional until it has been seen
/// once, and required from then on. `official_required` makes the official
/// repositories strict from the start.
pub(crate) fn required(official: bool, seen: Seen, official_required: bool) -> bool {
    seen.seen_metadata || (official && official_required)
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// RSA PKCS#1 v1.5 with SHA-256 over exactly `message`, as made by
/// `openssl dgst -sha256 -sign`.
pub(crate) fn verify(public_key_pem: &str, message: &[u8], signature: &[u8]) -> Result<()> {
    let key = rsa_public_key(public_key_pem)?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, key)
        .verify(message, signature)
        .map_err(|_| err("signature does not match"))
}

/// The `RSAPublicKey` (PKCS#1) DER inside a PEM `SubjectPublicKeyInfo`, which
/// is the form the verifier takes. Anything but one RSA key, exactly, is an
/// error.
pub(crate) fn rsa_public_key(pem: &str) -> Result<Vec<u8>> {
    let invalid = || err("repository public key is not a PEM RSA public key");
    let pem = pem.replace("\r\n", "\n");
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN PUBLIC KEY-----")
        .and_then(|rest| rest.strip_suffix("-----END PUBLIC KEY-----"))
        .ok_or_else(invalid)?;
    let der = base64(body).ok_or_else(invalid)?;
    // SubjectPublicKeyInfo ::= SEQUENCE { AlgorithmIdentifier, BIT STRING }
    let (info, rest) = der_element(&der, 0x30).ok_or_else(invalid)?;
    let (algorithm, info) = der_element(info, 0x30).ok_or_else(invalid)?;
    let (key, after) = der_element(info, 0x03).ok_or_else(invalid)?;
    // rsaEncryption (1.2.840.113549.1.1.1) with its NULL parameters.
    const RSA: [u8; 13] = [
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    // A BIT STRING starts with its count of unused bits; a key has none.
    match key.split_first() {
        Some((0, key)) if rest.is_empty() && after.is_empty() && algorithm == RSA => {
            Ok(key.to_vec())
        }
        _ => Err(invalid()),
    }
}

/// One definite-length DER element with this tag: its contents, and what
/// follows it.
fn der_element(bytes: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&found, bytes) = bytes.split_first()?;
    let (&first, bytes) = bytes.split_first()?;
    if found != tag {
        return None;
    }
    let (length, bytes) = if first < 0x80 {
        (first as usize, bytes)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || bytes.len() < count {
            return None;
        }
        let (digits, bytes) = bytes.split_at(count);
        let length = digits.iter().fold(0usize, |n, b| (n << 8) | *b as usize);
        // DER uses the shortest form.
        if digits[0] == 0 || length < 0x80 {
            return None;
        }
        (length, bytes)
    };
    (bytes.len() >= length).then(|| bytes.split_at(length))
}

/// Standard base64 with padding; line breaks and spaces between are ignored.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count, mut padding) = (0u32, 0u8, 0u8);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        };
        if padding > 0 {
            return None;
        }
        bits = (bits << 6) | value as u32;
        count += 1;
        if count == 4 {
            output.extend_from_slice(&bits.to_be_bytes()[1..]);
            (bits, count) = (0, 0);
        }
    }
    match (count, padding) {
        (0, 0) => {}
        (3, 1) if bits & 0x3 == 0 => output.extend_from_slice(&(bits << 6).to_be_bytes()[1..3]),
        (2, 2) if bits & 0xf == 0 => output.push((bits >> 4) as u8),
        _ => return None,
    }
    Some(output)
}

/// Seconds since the Unix epoch for an RFC 3339 UTC time in whole seconds,
/// `2026-09-19T21:00:00Z`, the only form a feed writes.
pub(crate) fn timestamp(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'Z'),
        ]
        .iter()
        .any(|(at, byte)| bytes[*at] != *byte)
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u64> {
        let digits = &text[range];
        digits
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if year < 1970 || day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Where a redirect leads, if it may be followed: at most `MAX_REDIRECTS` of
/// them, and only to an `https://` address. What is downloaded is checked
/// against a signature, a sequence number and a hash, so another address
/// cannot make a bad feed acceptable; plain HTTP would still tell everyone on
/// the network which packages a remote asks for.
pub(crate) fn redirect(from: &url::Url, location: &str, followed: usize) -> Result<url::Url> {
    if followed >= MAX_REDIRECTS {
        return Err(err("the download was redirected too many times"));
    }
    let to = from
        .join(location)
        .map_err(|_| err("the download was redirected to an invalid address"))?;
    if to.scheme() != "https"
        || to.host_str().is_none()
        || !to.username().is_empty()
        || to.password().is_some()
    {
        return Err(err(
            "the download was redirected to an address that is not HTTPS",
        ));
    }
    Ok(to)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        process::{Command, Stdio},
    };

    const OFFICIAL_KEY: &str = include_str!("official.rsa.pub");

    /// A throwaway RSA key made by the openssl command line, which is also
    /// what a feed signs with. Nothing here is ever a real key.
    pub(crate) struct TestKey {
        directory: PathBuf,
        pub public_pem: String,
    }
    impl TestKey {
        pub(crate) fn new(bits: u32) -> Self {
            let directory = std::env::temp_dir().join(format!("couch-feed-key-{}", crate::nonce()));
            fs::create_dir_all(&directory).unwrap();
            openssl(&[
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                &format!("rsa_keygen_bits:{bits}"),
                "-out",
                directory.join("key.pem").to_str().unwrap(),
            ]);
            let public_pem = String::from_utf8(openssl(&[
                "pkey",
                "-in",
                directory.join("key.pem").to_str().unwrap(),
                "-pubout",
            ]))
            .unwrap();
            Self {
                directory,
                public_pem,
            }
        }
        /// `openssl dgst -sha256 -sign KEY -out feed.json.sig feed.json`
        pub(crate) fn sign(&self, message: &[u8]) -> Vec<u8> {
            let file = self.directory.join(format!("message-{}", crate::nonce()));
            fs::write(&file, message).unwrap();
            openssl(&[
                "dgst",
                "-sha256",
                "-sign",
                self.directory.join("key.pem").to_str().unwrap(),
                file.to_str().unwrap(),
            ])
        }
        /// `openssl dgst -sha256 -verify PUB -signature SIG FILE`
        fn openssl_verifies(&self, message: &[u8], signature: &[u8]) -> bool {
            let name = crate::nonce();
            let (file, sig, public) = (
                self.directory.join(format!("m-{name}")),
                self.directory.join(format!("s-{name}")),
                self.directory.join("public.pem"),
            );
            fs::write(&file, message).unwrap();
            fs::write(&sig, signature).unwrap();
            fs::write(&public, &self.public_pem).unwrap();
            Command::new("openssl")
                .args(["dgst", "-sha256", "-verify"])
                .arg(&public)
                .arg("-signature")
                .arg(&sig)
                .arg(&file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        }
    }
    impl Drop for TestKey {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    fn openssl(args: &[&str]) -> Vec<u8> {
        let output = Command::new("openssl")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("these tests need the openssl command line");
        assert!(
            output.status.success(),
            "openssl {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    /// What openssl itself says the PKCS#1 form of a public key is.
    fn openssl_rsa_public_key(pem: &str) -> Vec<u8> {
        let file = std::env::temp_dir().join(format!("couch-feed-pub-{}", crate::nonce()));
        fs::write(&file, pem).unwrap();
        let der = openssl(&[
            "rsa",
            "-pubin",
            "-in",
            file.to_str().unwrap(),
            "-RSAPublicKey_out",
            "-outform",
            "DER",
        ]);
        fs::remove_file(file).unwrap();
        der
    }

    pub(crate) const ISSUED: u64 = 1_789_851_600; // 2026-09-19T21:00:00Z
    pub(crate) fn document(sequence: u64, index: &[u8]) -> serde_json::Value {
        serde_json::json!({
            "schema": 1, "channel": "preview", "sequence": sequence,
            "issued": "2026-09-19T21:00:00Z", "expires": "2026-10-19T21:00:00Z",
            "index": {"path": "APKINDEX.tar.gz", "size": index.len(), "sha256": sha256_hex(index)},
            "packages": [{"id": "denon", "version": "0.2.1", "apk": "couch-integration-denon-0.2.1-r0.apk",
                "size": 5, "sha256": sha256_hex(b"bytes"), "protocol_version": 2, "min_core_protocol_version": 2}],
            "later": {"ignored": true}
        })
    }

    #[test]
    fn a_public_key_converts_to_the_form_openssl_says_it_has() {
        for bits in [2048, 4096] {
            let key = TestKey::new(bits);
            assert_eq!(
                rsa_public_key(&key.public_pem).unwrap(),
                openssl_rsa_public_key(&key.public_pem),
                "{bits}"
            );
            // Pasted keys arrive with other line endings and stray space.
            let pasted = format!("  {}\n\n", key.public_pem.replace('\n', "\r\n"));
            assert_eq!(
                rsa_public_key(&pasted).unwrap(),
                openssl_rsa_public_key(&key.public_pem)
            );
        }
        // The real official key: 4096 bits, modulus first, exponent 65537 last.
        let official = rsa_public_key(OFFICIAL_KEY).unwrap();
        assert_eq!(official, openssl_rsa_public_key(OFFICIAL_KEY));
        assert_eq!(official.len(), 526);
        assert_eq!(official[..4], [0x30, 0x82, 0x02, 0x0a]);
        assert_eq!(
            official[official.len() - 5..],
            [0x02, 0x03, 0x01, 0x00, 0x01]
        );
    }

    #[test]
    fn only_one_whole_rsa_key_is_a_public_key() {
        let key = TestKey::new(2048);
        let der = base64(
            key.public_pem
                .trim()
                .strip_prefix("-----BEGIN PUBLIC KEY-----")
                .unwrap()
                .strip_suffix("-----END PUBLIC KEY-----")
                .unwrap(),
        )
        .unwrap();
        let pem = |der: &[u8]| {
            let file = std::env::temp_dir().join(format!("couch-feed-der-{}", crate::nonce()));
            fs::write(&file, der).unwrap();
            let text = openssl(&["base64", "-in", file.to_str().unwrap()]);
            fs::remove_file(file).unwrap();
            format!(
                "-----BEGIN PUBLIC KEY-----\n{}-----END PUBLIC KEY-----\n",
                String::from_utf8(text).unwrap()
            )
        };
        assert!(rsa_public_key(&pem(&der)).is_ok());
        let mut trailing = der.clone();
        trailing.push(0);
        assert!(rsa_public_key(&pem(&trailing)).is_err());
        assert!(rsa_public_key(&pem(&der[..der.len() - 1])).is_err());
        // The same key relabelled as another algorithm (the OID's last byte).
        let mut other = der.clone();
        let at = other.windows(9).position(|w| w == &RSA_OID[2..]).unwrap();
        other[at + 8] = 0x0a;
        assert!(rsa_public_key(&pem(&other)).is_err());
        // An elliptic-curve key is a public key, but not one a feed signs with.
        let curve = String::from_utf8(openssl(&[
            "genpkey",
            "-algorithm",
            "EC",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
        ]))
        .unwrap();
        let file = std::env::temp_dir().join(format!("couch-feed-ec-{}", crate::nonce()));
        fs::write(&file, curve).unwrap();
        let public = openssl(&["pkey", "-in", file.to_str().unwrap(), "-pubout"]);
        fs::remove_file(file).unwrap();
        assert!(rsa_public_key(&String::from_utf8(public).unwrap()).is_err());
        for text in [
            "",
            "-----BEGIN PUBLIC KEY-----\n-----END PUBLIC KEY-----",
            "-----BEGIN PUBLIC KEY-----\n!!!!\n-----END PUBLIC KEY-----",
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----",
        ] {
            assert!(rsa_public_key(text).is_err(), "{text}");
        }
    }
    const RSA_OID: [u8; 11] = [
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01,
    ];

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64("").unwrap(), b"");
        assert_eq!(base64("Zg==").unwrap(), b"f");
        assert_eq!(base64("Zm8=").unwrap(), b"fo");
        assert_eq!(base64("Zm9v\nYmFy\n").unwrap(), b"foobar");
        assert_eq!(base64("+/+/").unwrap(), [0xfb, 0xff, 0xbf]);
        for text in ["Zg", "Zg=", "Zm8", "Z", "Zg==Zg==", "Zh==", "Zm9=", "Zm-_"] {
            assert!(base64(text).is_none(), "{text}");
        }
    }

    #[test]
    fn signatures_agree_with_openssl_in_both_directions() {
        // 4096 bits is what the official feed signs with: a 512-byte signature.
        for bits in [2048, 4096] {
            signatures_agree(bits);
        }
    }
    fn signatures_agree(bits: u32) {
        let key = TestKey::new(bits);
        let message = br#"{"schema":1}"#;
        let signature = key.sign(message);
        assert_eq!(signature.len(), bits as usize / 8);
        // What openssl signed, this verifies; and openssl agrees with each
        // refusal below, so the two are interchangeable for a feed.
        assert!(key.openssl_verifies(message, &signature));
        verify(&key.public_pem, message, &signature).unwrap();
        let mut changed = message.to_vec();
        changed[3] ^= 1;
        assert!(!key.openssl_verifies(&changed, &signature));
        assert!(verify(&key.public_pem, &changed, &signature).is_err());
        let mut forged = signature.clone();
        forged[10] ^= 1;
        assert!(!key.openssl_verifies(message, &forged));
        assert!(verify(&key.public_pem, message, &forged).is_err());
        assert!(verify(&key.public_pem, message, b"").is_err());
        let other = TestKey::new(2048);
        assert!(!other.openssl_verifies(message, &signature));
        assert!(verify(&other.public_pem, message, &signature).is_err());
        assert!(verify(OFFICIAL_KEY, message, &signature).is_err());
        // A signature a byte short is not one.
        assert!(verify(&key.public_pem, message, &signature[..signature.len() - 1]).is_err());
    }

    #[test]
    fn timestamps_are_whole_utc_seconds() {
        assert_eq!(timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(timestamp("2026-09-19T21:00:00Z"), Some(ISSUED));
        assert_eq!(timestamp("2024-02-29T23:59:59Z"), Some(1_709_251_199));
        assert_eq!(timestamp("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(timestamp("2100-03-01T00:00:00Z"), Some(4_107_542_400));
        for text in [
            "2026-09-19T21:00:00+00:00",
            "2026-09-19T21:00:00.5Z",
            "2026-09-19 21:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-00-10T00:00:00Z",
            "2026-09-31T00:00:00Z",
            "2026-09-19T24:00:00Z",
            "2026-09-19T21:60:00Z",
            "1969-12-31T23:59:59Z",
            "2026-09-19T21:00:0+Z",
            "",
        ] {
            assert_eq!(timestamp(text), None, "{text}");
        }
    }

    struct Case {
        key: TestKey,
        index: Vec<u8>,
    }
    impl Case {
        fn check(
            &self,
            document: &serde_json::Value,
            edit: impl Fn(&mut Check),
        ) -> Result<Option<Verified>> {
            let bytes = serde_json::to_vec(document).unwrap();
            let signature = self.key.sign(&bytes);
            let mut input = Check {
                public_key: &self.key.public_pem,
                seen: Seen::default(),
                required: false,
                channel: Some("preview"),
                index: &self.index,
                now: ISSUED + 60,
            };
            edit(&mut input);
            check(
                Served::Metadata {
                    document: &bytes,
                    signature: Some(&signature),
                },
                &input,
            )
        }
    }

    #[test]
    fn metadata_is_checked_in_order_and_every_failure_says_why() {
        let case = Case {
            key: TestKey::new(2048),
            index: b"the index".to_vec(),
        };
        let valid = document(10, &case.index);
        let verified = case.check(&valid, |_| {}).unwrap().unwrap();
        assert_eq!(verified.sequence, 10);
        assert_eq!(verified.packages.len(), 1);
        assert_eq!(verified.packages[0].min_core_protocol_version, 2);
        assert!(!verified.clock_unreliable);

        // Sequence: the same feed again is fine, an older one is not.
        let seen = |sequence| {
            move |check: &mut Check| {
                check.seen = Seen {
                    sequence,
                    seen_metadata: true,
                }
            }
        };
        assert!(case.check(&valid, seen(10)).unwrap().is_some());
        assert!(case.check(&valid, seen(9)).unwrap().is_some());
        assert_eq!(case.check(&valid, seen(11)).unwrap_err().0, OLDER_THAN_SEEN);

        // Expiry, by a clock that can be believed.
        let at = |now| move |check: &mut Check| check.now = now;
        let expires = timestamp("2026-10-19T21:00:00Z").unwrap();
        assert!(case.check(&valid, at(expires)).unwrap().is_some());
        let expired = case.check(&valid, at(expires + 1)).unwrap_err();
        assert!(
            expired.0.contains("expired on 2026-10-19T21:00:00Z"),
            "{expired}"
        );
        // A day's slack before `issued` is an ordinary clock.
        assert!(
            !case
                .check(&valid, at(ISSUED - 86_400))
                .unwrap()
                .unwrap()
                .clock_unreliable
        );
        // An unset clock (1970, or years behind) cannot judge expiry, and
        // everything else is still checked.
        for now in [0, ISSUED - 86_401] {
            assert!(
                case.check(&valid, at(now))
                    .unwrap()
                    .unwrap()
                    .clock_unreliable
            );
            let old = |check: &mut Check| {
                check.now = now;
                check.seen.sequence = 11;
            };
            assert_eq!(case.check(&valid, old).unwrap_err().0, OLDER_THAN_SEEN);
        }

        // The index the metadata describes, and no other.
        for other in [&b"another index"[..], b"the indeX", b""] {
            let error = case.check(&valid, |check| check.index = other).unwrap_err();
            assert!(
                error.0.contains("repository index is not the one"),
                "{error}"
            );
        }
        let mut upper = valid.clone();
        upper["index"]["sha256"] = sha256_hex(&case.index).to_uppercase().into();
        assert!(case.check(&upper, |_| {}).unwrap().is_some());

        // Channel, for the official repositories; a custom feed names its own.
        let stable = |check: &mut Check| check.channel = Some("stable");
        assert!(case
            .check(&valid, stable)
            .unwrap_err()
            .0
            .contains("another channel"));
        assert!(case
            .check(&valid, |check| check.channel = None)
            .unwrap()
            .is_some());

        // Not valid, whatever the repository: these are signed, so the feed is broken.
        for (key, value) in [
            ("schema", serde_json::json!(0)),
            ("schema", serde_json::json!("1")),
            ("sequence", serde_json::json!(-1)),
            ("sequence", serde_json::json!(1.5)),
            ("issued", serde_json::json!("yesterday")),
            ("expires", serde_json::json!("2026-09-18T21:00:00Z")),
            ("packages", serde_json::json!({})),
            ("index", serde_json::json!(null)),
        ] {
            let mut broken = valid.clone();
            broken[key] = value;
            let error = case.check(&broken, |_| {}).unwrap_err();
            assert_eq!(error.0, "The package feed's metadata is not valid", "{key}");
        }
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("schema");
        assert!(case.check(&missing, |_| {}).is_err());
    }

    #[test]
    fn a_bad_or_absent_signature_is_refused_before_anything_is_read() {
        let key = TestKey::new(2048);
        let index = b"the index".to_vec();
        let bytes = serde_json::to_vec(&document(10, &index)).unwrap();
        let signature = key.sign(&bytes);
        let input = Check {
            public_key: &key.public_pem,
            seen: Seen::default(),
            required: false,
            channel: None,
            index: &index,
            now: ISSUED,
        };
        let served = |document, signature| Served::Metadata {
            document,
            signature,
        };
        assert!(check(served(&bytes, Some(&signature)), &input)
            .unwrap()
            .is_some());
        // The bytes that were signed are the bytes checked, however they are
        // laid out: a feed writes indented JSON with a final newline.
        let mut pretty = serde_json::to_vec_pretty(&document(10, &index)).unwrap();
        pretty.push(b'\n');
        let pretty_signature = key.sign(&pretty);
        assert!(check(served(&pretty, Some(&pretty_signature)), &input)
            .unwrap()
            .is_some());
        assert!(check(served(&pretty, Some(&signature)), &input).is_err());
        let not_valid = "The package feed's metadata signature is not valid";
        // Even a repository that has never published metadata: what is there
        // has to be the feed's own.
        assert_eq!(
            check(served(&bytes, None), &input).unwrap_err().0,
            not_valid
        );
        let mut edited = bytes.clone();
        let at = edited.windows(2).position(|w| w == b"10").unwrap();
        edited[at] = b'9';
        assert_eq!(
            check(served(&edited, Some(&signature)), &input)
                .unwrap_err()
                .0,
            not_valid
        );
        assert_eq!(
            check(served(&bytes, Some(&[0; 256])), &input)
                .unwrap_err()
                .0,
            not_valid
        );
        assert_eq!(
            check(served(&bytes, Some(&[0; 1025])), &input)
                .unwrap_err()
                .0,
            not_valid
        );
        let other = Check {
            public_key: OFFICIAL_KEY,
            ..input
        };
        assert_eq!(
            check(served(&bytes, Some(&signature)), &other)
                .unwrap_err()
                .0,
            not_valid
        );
        let large = vec![b' '; MAX_METADATA as usize + 1];
        assert!(check(served(&large, Some(&signature)), &input)
            .unwrap_err()
            .0
            .contains("size limit"));
    }

    #[test]
    fn missing_or_newer_format_metadata_is_refused_only_once_it_is_required() {
        let case = Case {
            key: TestKey::new(2048),
            index: b"the index".to_vec(),
        };
        let input = |required| Check {
            public_key: &case.key.public_pem,
            seen: Seen::default(),
            required,
            channel: None,
            index: &case.index,
            now: ISSUED,
        };
        assert_eq!(check(Served::Missing, &input(false)).unwrap(), None);
        assert!(check(Served::Missing, &input(true))
            .unwrap_err()
            .0
            .contains("metadata is missing"));
        // A later format this Couch cannot read, properly signed: as good as
        // missing. Nothing in it is believed, not even its sequence.
        let mut later = document(1, b"some other index");
        later["schema"] = 2.into();
        later["sequence"] = "not a number any more".into();
        assert_eq!(case.check(&later, |_| {}).unwrap(), None);
        let error = case
            .check(&later, |check| check.required = true)
            .unwrap_err();
        assert!(error.0.contains("newer format"), "{error}");
    }

    #[test]
    fn metadata_is_required_once_seen_and_for_official_feeds_once_switched_on() {
        let (never, seen) = (
            Seen::default(),
            Seen {
                sequence: 0,
                seen_metadata: true,
            },
        );
        // Today: official and custom repositories alike, trust on first use.
        assert!(!required(true, never, false));
        assert!(!required(false, never, false));
        assert!(required(true, seen, false));
        assert!(required(false, seen, false));
        // Once OFFICIAL_METADATA_REQUIRED is true: official from the start.
        assert!(required(true, never, true));
        assert!(!required(false, never, true));
    }

    #[test]
    fn redirects_are_followed_three_times_and_only_to_https() {
        let from = url::Url::parse("https://packages.example/preview/armv7/feed.json").unwrap();
        let to = |location, followed| redirect(&from, location, followed).map(String::from);
        assert_eq!(
            to("https://cdn.example/feed.json", 0).unwrap(),
            "https://cdn.example/feed.json"
        );
        // Relative to where it came from, as a browser would.
        assert_eq!(
            to("/elsewhere/feed.json", 2).unwrap(),
            "https://packages.example/elsewhere/feed.json"
        );
        assert_eq!(
            to("other.json", 0).unwrap(),
            "https://packages.example/preview/armv7/other.json"
        );
        assert!(to("https://cdn.example/feed.json", 3).is_err());
        assert!(to("https://cdn.example/feed.json", 4).is_err());
        for location in [
            "http://cdn.example/feed.json",
            "//user:secret@cdn.example/feed.json",
            "ftp://cdn.example/feed.json",
            "file:///etc/passwd",
            "data:text/plain,hello",
            "https://",
            "",
        ] {
            // "" resolves to the same https address, which is allowed.
            assert_eq!(to(location, 0).is_err(), !location.is_empty(), "{location}");
        }
        // A plain-HTTP origin (a test server) may be sent to HTTPS, never to HTTP.
        let plain = url::Url::parse("http://127.0.0.1:8080/armv7/feed.json").unwrap();
        assert!(redirect(&plain, "https://cdn.example/feed.json", 0).is_ok());
        assert!(redirect(&plain, "/armv7/other.json", 0).is_err());
        assert!(redirect(&plain, "http://127.0.0.1:8080/other", 0).is_err());
    }
}
