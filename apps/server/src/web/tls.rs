//! The self-signed certificate the HTTPS listener uses, generated once into
//! a directory and reused on every start, so a browser's one-time acceptance
//! keeps working. Browsers allow the microphone only on a secure origin,
//! which is all this is for; there is no CA and the browser will warn.

use std::path::Path;

use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod};
use openssl::x509::extension::SubjectAlternativeName;
use openssl::x509::{X509, X509NameBuilder};

use dji_common::logln;

const DAYS: u32 = 3650;

/// An acceptor for `dir/server.crt` + `dir/server.key`, generating them
/// first when missing. `hosts` are the names and addresses the certificate
/// is issued for (the Orin's hostname and IPs).
pub fn acceptor(dir: &Path, hosts: &[String]) -> Result<SslAcceptor, String> {
    let (crt, key) = (dir.join("server.crt"), dir.join("server.key"));
    if !(crt.exists() && key.exists()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let (c, k) = generate(hosts).map_err(|e| format!("generate certificate: {e}"))?;
        std::fs::write(&crt, c).map_err(|e| format!("{}: {e}", crt.display()))?;
        write_private(&key, &k).map_err(|e| format!("{}: {e}", key.display()))?;
        logln!(
            "tls: new self-signed certificate in {} for {hosts:?}",
            dir.display()
        );
    }
    let mut b =
        SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).map_err(|e| e.to_string())?;
    b.set_private_key_file(&key, SslFiletype::PEM)
        .map_err(|e| format!("{}: {e}", key.display()))?;
    b.set_certificate_chain_file(&crt)
        .map_err(|e| format!("{}: {e}", crt.display()))?;
    b.check_private_key().map_err(|e| e.to_string())?;
    Ok(b.build())
}

/// Write `data` to a new file only its owner can read (the private key).
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

/// A fresh self-signed certificate and key, PEM-encoded.
fn generate(hosts: &[String]) -> Result<(Vec<u8>, Vec<u8>), openssl::error::ErrorStack> {
    let key = PKey::from_rsa(Rsa::generate(2048)?)?;
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text("CN", "dji-orin-interface")?;
    let name = name.build();
    let mut b = X509::builder()?;
    b.set_version(2)?;
    let mut serial = BigNum::new()?;
    serial.rand(128, MsbOption::MAYBE_ZERO, false)?;
    let serial = serial.to_asn1_integer()?;
    b.set_serial_number(&serial)?;
    b.set_subject_name(&name)?;
    b.set_issuer_name(&name)?;
    b.set_pubkey(&key)?;
    let (from, until) = (Asn1Time::days_from_now(0)?, Asn1Time::days_from_now(DAYS)?);
    b.set_not_before(&from)?;
    b.set_not_after(&until)?;
    let mut san = SubjectAlternativeName::new();
    san.dns("localhost").ip("127.0.0.1");
    for h in hosts {
        if h.parse::<std::net::IpAddr>().is_ok() {
            san.ip(h);
        } else {
            san.dns(h);
        }
    }
    let san = san.build(&b.x509v3_context(None, None))?;
    b.append_extension(san)?;
    b.sign(&key, MessageDigest::sha256())?;
    let cert = b.build();
    Ok((cert.to_pem()?, key.private_key_to_pem_pkcs8()?))
}

/// This machine's hostname and addresses, for the certificate.
pub fn local_hosts() -> Vec<String> {
    let mut hosts = Vec::new();
    for cmd in [["hostname", ""], ["hostname", "-I"]] {
        let out = std::process::Command::new(cmd[0])
            .args(cmd[1..].iter().filter(|a| !a.is_empty()))
            .output();
        if let Ok(o) = out {
            hosts.extend(
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .map(String::from),
            );
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_and_reloads_a_certificate() {
        let dir = std::env::temp_dir().join(format!("dji-tls-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hosts = vec!["orin".to_string(), "10.0.0.5".to_string()];
        acceptor(&dir, &hosts).unwrap();
        let pem = std::fs::read(dir.join("server.crt")).unwrap();
        let cert = X509::from_pem(&pem).unwrap();
        let names: Vec<String> = cert
            .subject_alt_names()
            .unwrap()
            .iter()
            .map(|n| {
                n.dnsname()
                    .map(String::from)
                    .or_else(|| n.ipaddress().map(|ip| format!("{ip:?}")))
                    .unwrap_or_default()
            })
            .collect();
        assert!(names.contains(&"orin".to_string()) && names.contains(&"localhost".to_string()));
        assert!(names.iter().any(|n| n.contains("10, 0, 0, 5")), "{names:?}");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("server.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the private key is readable by others: {mode:o}"
        );
        // Second call reuses the files.
        let before = std::fs::metadata(dir.join("server.key"))
            .unwrap()
            .modified()
            .unwrap();
        acceptor(&dir, &[]).unwrap();
        assert_eq!(
            std::fs::metadata(dir.join("server.key"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
