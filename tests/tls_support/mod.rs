//! Certificates for the TLS tests, generated per run: a fixed leaf would expire, and macOS
//! refuses a server certificate valid for more than 825 days even under a trusted root.
//!
//! Every file is test data only: P-256 keys, a CA that signs a server certificate for
//! 127.0.0.1, and a second CA that has nothing to do with the first.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const ECDSA_SHA256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const PRIME256V1: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const COMMON_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
const BASIC_CONSTRAINTS: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x13];
const KEY_USAGE: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x0f];
const EXT_KEY_USAGE: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x25];
const SUBJECT_ALT_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x11];
const SERVER_AUTH: &[u8] = &[0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

pub struct TlsFiles {
    dir: PathBuf,
}

impl TlsFiles {
    /// Writes `ca.pem`, `other-ca.pem`, `server.pem` (signed by `ca.pem`) and `server.key`.
    pub fn generate(dir: &Path) -> Self {
        let ca = KeyMaterial::new();
        let other = KeyMaterial::new();
        let server = KeyMaterial::new();
        let ca_cert = ca.certificate("abapreach test CA", 1, None, true);
        let other_cert = other.certificate("abapreach unrelated test CA", 2, None, true);
        let server_cert =
            server.certificate("127.0.0.1", 3, Some((&ca, "abapreach test CA")), false);

        fs::write(dir.join("ca.pem"), pem("CERTIFICATE", &ca_cert)).unwrap();
        fs::write(dir.join("other-ca.pem"), pem("CERTIFICATE", &other_cert)).unwrap();
        fs::write(dir.join("server.pem"), pem("CERTIFICATE", &server_cert)).unwrap();
        fs::write(dir.join("server.key"), pem("PRIVATE KEY", &server.pkcs8)).unwrap();

        Self::open(dir)
    }

    /// The files a parent test already generated, for the child process that serves them.
    pub fn open(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn ca(&self) -> PathBuf {
        self.dir.join("ca.pem")
    }

    pub fn other_ca(&self) -> PathBuf {
        self.dir.join("other-ca.pem")
    }

    pub fn server_cert(&self) -> PathBuf {
        self.dir.join("server.pem")
    }

    pub fn server_key(&self) -> PathBuf {
        self.dir.join("server.key")
    }
}

struct KeyMaterial {
    pkcs8: Vec<u8>,
    pair: EcdsaKeyPair,
}

impl KeyMaterial {
    fn new() -> Self {
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &SystemRandom::new())
                .unwrap()
                .as_ref()
                .to_vec();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8).unwrap();

        Self { pkcs8, pair }
    }

    /// An X.509 v3 certificate for this key. `issuer` is the signing CA and its name; `None`
    /// makes a self-signed CA. A leaf is valid for 127.0.0.1 and `serverAuth`.
    fn certificate(
        &self,
        name: &str,
        serial: u8,
        issuer: Option<(&KeyMaterial, &str)>,
        is_ca: bool,
    ) -> Vec<u8> {
        let subject = common_name(name);
        let (signer, issuer_name) = match issuer {
            Some((key, issuer_name)) => (key, common_name(issuer_name)),
            None => (self, subject.clone()),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let validity = der(
            0x30,
            &[
                der(0x17, utc_time(now - 86_400).as_bytes()),
                der(0x17, utc_time(now + 7 * 86_400).as_bytes()),
            ]
            .concat(),
        );
        let extensions = if is_ca {
            [
                extension(BASIC_CONSTRAINTS, true, &der(0x30, &[0x01, 0x01, 0xff])),
                // keyCertSign and cRLSign, one unused bit.
                extension(KEY_USAGE, true, &[0x03, 0x02, 0x01, 0x06]),
            ]
            .concat()
        } else {
            [
                extension(BASIC_CONSTRAINTS, true, &der(0x30, &[])),
                // digitalSignature, seven unused bits.
                extension(KEY_USAGE, true, &[0x03, 0x02, 0x07, 0x80]),
                extension(EXT_KEY_USAGE, false, &der(0x30, SERVER_AUTH)),
                extension(
                    SUBJECT_ALT_NAME,
                    false,
                    &der(0x30, &[0x87, 0x04, 127, 0, 0, 1]),
                ),
            ]
            .concat()
        };
        let algorithm = der(0x30, ECDSA_SHA256);
        let public_key = der(
            0x30,
            &[
                der(0x30, &[EC_PUBLIC_KEY, PRIME256V1].concat()),
                der(
                    0x03,
                    &[&[0u8][..], self.pair.public_key().as_ref()].concat(),
                ),
            ]
            .concat(),
        );
        let tbs = der(
            0x30,
            &[
                der(0xa0, &[0x02, 0x01, 0x02]),
                der(0x02, &[serial]),
                algorithm.clone(),
                issuer_name,
                validity,
                subject,
                public_key,
                der(0xa3, &der(0x30, &extensions)),
            ]
            .concat(),
        );
        let signature = signer.pair.sign(&SystemRandom::new(), &tbs).unwrap();

        der(
            0x30,
            &[
                tbs,
                algorithm,
                der(0x03, &[&[0u8][..], signature.as_ref()].concat()),
            ]
            .concat(),
        )
    }
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];

    match content.len() {
        length if length < 0x80 => out.push(length as u8),
        length if length < 0x100 => out.extend([0x81, length as u8]),
        length => out.extend([0x82, (length >> 8) as u8, length as u8]),
    }
    out.extend_from_slice(content);

    out
}

fn common_name(name: &str) -> Vec<u8> {
    der(
        0x30,
        &der(
            0x31,
            &der(0x30, &[COMMON_NAME, &der(0x0c, name.as_bytes())].concat()),
        ),
    )
}

fn extension(oid: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let critical: &[u8] = if critical { &[0x01, 0x01, 0xff] } else { &[] };

    der(0x30, &[oid, critical, &der(0x04, value)].concat())
}

/// `YYMMDDHHMMSSZ` for a Unix time (years 2000 to 2049).
fn utc_time(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!(
        "{:02}{month:02}{day:02}{:02}{:02}{:02}Z",
        year % 100,
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

fn pem(label: &str, der: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();

    for chunk in der.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| {
            acc | u32::from(*byte) << (16 - 8 * index)
        });

        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(group >> (18 - 6 * index) & 0x3f) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }

    let lines = encoded
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");

    format!("-----BEGIN {label}-----\n{lines}\n-----END {label}-----\n")
}
