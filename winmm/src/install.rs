//! The private CA and the leaf certificate for master.shirogames.com, both
//! under %ProgramData%\wartales-mp, and the TLS server side of the master
//! (port of internal/install).
//!
//! Nothing here touches the machine or the game folder: the winmm.dll proxy
//! resolves master*.shirogames.com to 127.0.0.1 and appends ca.crt to the
//! chain the game configures (proxy.c load_ca reads it once).

use crate::conn::{Conn, ConnRef};
use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType, SerialNumber,
};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Hosts the master certificate must be valid for (the game pins these names).
pub const MASTER_HOSTS: [&str; 2] = ["master.shirogames.com", "master2.shirogames.com"];

const CA_CERT_FILE: &str = "ca.crt";
const CA_KEY_FILE: &str = "ca.key";
const LEAF_CERT_FILE: &str = "master.crt";
const LEAF_KEY_FILE: &str = "master.key";

const DAY: Duration = Duration::from_secs(86400);

/// Where the generated certificates live.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").filter(|v| !v.is_empty()).map(PathBuf::from);
    base.unwrap_or_else(std::env::temp_dir).join("wartales-mp")
}

/// Generates the CA and the leaf certificate if they are missing or expire
/// within 24 h; existing valid files (RSA from the Go helper included) are reused.
pub fn ensure_certs(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    if cert_valid(&dir.join(CA_CERT_FILE)) && cert_valid(&dir.join(LEAF_CERT_FILE)) {
        return Ok(());
    }
    let [ca_crt, ca_key, leaf_crt, leaf_key] = generate()?;
    // master.crt goes first and comes back last: a crash in between leaves no
    // valid pair of certs over mismatched keys, so the next start regenerates.
    let _ = std::fs::remove_file(dir.join(LEAF_CERT_FILE));
    write_atomic(&dir.join(CA_KEY_FILE), &ca_key)?;
    write_atomic(&dir.join(LEAF_KEY_FILE), &leaf_key)?;
    write_atomic(&dir.join(CA_CERT_FILE), &ca_crt)?;
    write_atomic(&dir.join(LEAF_CERT_FILE), &leaf_crt)
}

/// New CA + leaf as PEM: [ca.crt, ca.key, master.crt, master.key].
/// ECDSA P-256 because ring cannot generate RSA; swap the key here if needed.
fn generate() -> Result<[String; 4]> {
    let alg = &rcgen::PKCS_ECDSA_P256_SHA256;
    // rcgen takes time::OffsetDateTime; build it from the epoch to avoid a direct time dependency.
    let now = rcgen::date_time_ymd(1970, 1, 1) + SystemTime::now().duration_since(UNIX_EPOCH)?;
    let params = |cn: &str, sans: Vec<SanType>| {
        let mut p = CertificateParams::default();
        p.distinguished_name = DistinguishedName::new();
        p.distinguished_name.push(DnType::CommonName, cn);
        p.distinguished_name.push(DnType::OrganizationName, "wartales-mp");
        p.not_before = now - Duration::from_secs(3600);
        p.not_after = now + 3652 * DAY; // ~10 years
        p.serial_number = Some(serial());
        p.subject_alt_names = sans;
        p
    };

    let ca_key = KeyPair::generate_for(alg)?;
    let mut ca = params("wartales-mp local CA", vec![]);
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    let ca_cert = ca.self_signed(&ca_key)?;
    let ca_pem = ca_key.serialize_pem();
    let issuer = Issuer::new(ca, ca_key);

    let leaf_key = KeyPair::generate_for(alg)?;
    let mut sans: Vec<SanType> =
        MASTER_HOSTS.iter().map(|h| Ok(SanType::DnsName((*h).try_into()?))).collect::<Result<_>>()?;
    sans.push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    sans.push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    let mut leaf = params(MASTER_HOSTS[0], sans);
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf.use_authority_key_identifier_extension = true;
    let leaf_cert = leaf.signed_by(&leaf_key, &issuer)?;

    Ok([ca_cert.pem(), ca_pem, leaf_cert.pem(), leaf_key.serialize_pem()])
}

/// A random positive 127-bit serial, as Go's rand.Int(2^127).
fn serial() -> SerialNumber {
    let mut b = [0u8; 16];
    crate::rand_bytes(&mut b);
    b[0] &= 0x7f;
    SerialNumber::from_slice(&b)
}

/// tmp + rename, so a reader (the shim's load_ca) never sees half a file.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))
}

/// Is the PEM cert at path parseable and valid for at least 24 more hours?
fn cert_valid(path: &Path) -> bool {
    let Ok(der) = CertificateDer::from_pem_file(path) else { return false };
    not_after(&der).is_some_and(|t| SystemTime::now() + DAY < t)
}

/// One DER TLV: (tag, content, rest).
fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, b) = b.split_first()?;
    let (&l, mut b) = b.split_first()?;
    let len = if l < 0x80 {
        l as usize
    } else {
        let n = (l & 0x7f) as usize;
        if n == 0 || n > 4 || b.len() < n {
            return None;
        }
        let len = b[..n].iter().fold(0usize, |a, &x| a << 8 | x as usize);
        b = &b[n..];
        len
    };
    (b.len() >= len).then(|| (tag, &b[..len], &b[len..]))
}

/// The notAfter of an X.509 certificate (Certificate > TBS > Validity).
fn not_after(der: &[u8]) -> Option<SystemTime> {
    let seq = |b| tlv(b).filter(|t| t.0 == 0x30);
    let (_, cert, _) = seq(der)?;
    let (_, mut tbs, _) = seq(cert)?;
    if tbs.first() == Some(&0xa0) {
        tbs = tlv(tbs)?.2; // explicit version
    }
    for _ in 0..3 {
        tbs = tlv(tbs)?.2; // serial, signature algorithm, issuer
    }
    let (_, validity, _) = seq(tbs)?;
    let (tag, t, _) = tlv(tlv(validity)?.2)?;
    let s = std::str::from_utf8(t).ok()?.strip_suffix('Z')?;
    let (year, rest) = match (tag, s.len()) {
        (0x17, 12) => {
            let yy: i64 = s.get(..2)?.parse().ok()?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, s.get(2..)?)
        }
        (0x18, 14) => (s.get(..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    let f = |i: usize| -> Option<i64> { rest.get(i..i + 2)?.parse().ok() };
    let (mon, day, h, m, sec) = (f(0)?, f(2)?, f(4)?, f(6)?, f(8)?);
    // days_from_civil (H. Hinnant)
    let y = if mon <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mon > 2 { mon - 3 } else { mon + 9 }) + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let secs = days * 86400 + h * 3600 + m * 60 + sec;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
}

/// The master listener's TLS config: master.crt/master.key, TLS 1.2 + 1.3.
pub fn tls_config(dir: &Path) -> Result<Arc<ServerConfig>> {
    let (crt, key) = (dir.join(LEAF_CERT_FILE), dir.join(LEAF_KEY_FILE));
    let certs = CertificateDer::pem_file_iter(&crt)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .with_context(|| format!("load {}", crt.display()))?;
    let key = PrivateKeyDer::from_pem_file(&key).with_context(|| format!("load {}", key.display()))?;
    let cfg = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(Arc::new(cfg))
}

/// Wraps an accepted socket as a server-side TLS Conn; the handshake runs
/// inside reads (the master always has a reader per connection).
pub fn accept_tls(stream: TcpStream, cfg: Arc<ServerConfig>) -> io::Result<ConnRef> {
    let mut tls = ServerConnection::new(cfg).map_err(io::Error::other)?;
    tls.set_buffer_limit(None); // write_all never stops short, even before the handshake is done
    Ok(Arc::new(TlsConn {
        rsock: stream.try_clone()?,
        sock: stream,
        st: Mutex::new(State { tls, raw: Vec::new(), eof: false, failed: None }),
        send: Mutex::new(()),
        closed: AtomicBool::new(false),
    }))
}

struct State {
    tls: ServerConnection,
    raw: Vec<u8>, // received TLS bytes not yet fed to rustls
    eof: bool,
    failed: Option<rustls::Error>, // terminal protocol error, returned by every later read
}

/// The reader blocks in the socket read WITHOUT any lock, and socket writes
/// happen outside the state lock (under `send`, which keeps record order), so
/// a push never waits for inbound data and a stuck send never stalls the reader.
struct TlsConn {
    sock: TcpStream,
    rsock: TcpStream,
    st: Mutex<State>,
    send: Mutex<()>,
    closed: AtomicBool,
}

impl TlsConn {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends whatever rustls has queued; takes `send` before releasing the state lock.
    fn flush<'a>(&'a self, mut s: MutexGuard<'a, State>) -> io::Result<MutexGuard<'a, State>> {
        let mut out = Vec::new();
        while s.tls.wants_write() {
            s.tls.write_tls(&mut out)?;
        }
        if out.is_empty() {
            return Ok(s);
        }
        let g = self.send.lock().unwrap_or_else(|e| e.into_inner());
        drop(s);
        (&self.sock).write_all(&out)?;
        drop(g);
        Ok(self.lock())
    }
}

impl Conn for TlsConn {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut tmp = vec![0u8; 32 * 1024];
        let mut s = self.lock();
        loop {
            if let Some(e) = &s.failed {
                return Err(io::Error::new(io::ErrorKind::InvalidData, e.clone()));
            }
            match s.tls.reader().read(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                r => return r, // data, clean close (0) or an error
            }
            if s.raw.is_empty() && !s.eof {
                drop(s);
                let n = (&self.rsock).read(&mut tmp)?;
                s = self.lock();
                if n == 0 {
                    s.eof = true;
                } else {
                    s.raw.extend_from_slice(&tmp[..n]);
                }
                continue;
            }
            let fed_eof = s.raw.is_empty();
            let st = &mut *s;
            // Fed only while the plaintext buffer is empty, so rustls never refuses for a full buffer.
            let n = st.tls.read_tls(&mut &st.raw[..])?;
            st.raw.drain(..n);
            if let Err(e) = st.tls.process_new_packets() {
                st.failed = Some(e);
            }
            s = self.flush(s)?; // handshake records, or the alert for a failure
            if fed_eof && s.failed.is_none() {
                return match s.tls.reader().read(buf) {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
                    r => r,
                };
            }
        }
    }
    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let mut s = self.lock();
        s.tls.writer().write_all(buf)?; // held by rustls until the handshake is done
        self.flush(s).map(drop)
    }
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.rsock.set_read_timeout(d)
    }
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.sock.set_write_timeout(d)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        // Best-effort close_notify that never blocks, then kill the socket.
        if let (Ok(mut s), Ok(_g)) = (self.st.try_lock(), self.send.try_lock()) {
            s.tls.send_close_notify();
            if self.sock.set_nonblocking(true).is_ok() {
                let _ = s.tls.write_tls(&mut &self.sock);
            }
        }
        crate::conn::close_tcp(&self.rsock);
        crate::conn::close_tcp(&self.sock);
    }
    fn peer_addr(&self) -> Option<SocketAddr> {
        self.sock.peer_addr().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn temp_dir(name: &str) -> PathBuf {
        let mut r = [0u8; 8];
        crate::rand_bytes(&mut r);
        let d = std::env::temp_dir().join(format!("wmp-install-{name}-{}", u64::from_le_bytes(r)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Accepts one connection on loopback, returns (server Conn, client stream).
    fn pair(dir: &Path) -> (ConnRef, StreamOwned<ClientConnection, TcpStream>) {
        let cfg = tls_config(dir).unwrap();
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(ln.local_addr().unwrap()).unwrap();
        let server = accept_tls(ln.accept().unwrap().0, cfg).unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from_pem_file(dir.join(CA_CERT_FILE)).unwrap()).unwrap();
        let ccfg = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let cc = ClientConnection::new(Arc::new(ccfg), "master.shirogames.com".try_into().unwrap()).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        (server, StreamOwned::new(cc, tcp))
    }

    /// Echoes one read back, in a thread; returns what it read.
    fn echo_once(server: ConnRef) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut b = [0u8; 64];
            let n = server.read(&mut b).unwrap();
            server.write_all(&b[..n]).unwrap();
            b[..n].to_vec()
        })
    }

    #[test]
    fn generated_certs_handshake_and_echo() {
        let dir = temp_dir("echo");
        ensure_certs(&dir).unwrap();
        let (server, mut client) = pair(&dir);
        let t = echo_once(server);
        client.write_all(b"hello").unwrap();
        let mut b = [0u8; 5];
        client.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hello");
        assert_eq!(t.join().unwrap(), b"hello");
        // Reused as-is on the next start.
        let before = std::fs::read(dir.join(CA_CERT_FILE)).unwrap();
        ensure_certs(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join(CA_CERT_FILE)).unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn push_while_reader_blocked() {
        let dir = temp_dir("push");
        ensure_certs(&dir).unwrap();
        let (server, mut client) = pair(&dir);
        let reader = echo_once(server.clone());
        let (done, finished) = mpsc::channel();
        let push = std::thread::spawn(move || {
            server.write_all(b"push").unwrap(); // reader holds no lock while blocked
            done.send(()).unwrap();
        });
        let mut b = [0u8; 4];
        client.read_exact(&mut b).unwrap(); // drives the handshake, then the push arrives
        assert_eq!(&b, b"push");
        finished.recv_timeout(Duration::from_secs(10)).expect("push deadlocked behind the blocked read");
        push.join().unwrap();
        client.write_all(b"x").unwrap();
        let mut b = [0u8; 1];
        client.read_exact(&mut b).unwrap();
        assert_eq!(reader.join().unwrap(), b"x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn close_unblocks_read() {
        let dir = temp_dir("close");
        ensure_certs(&dir).unwrap();
        let (server, mut client) = pair(&dir);
        let t = echo_once(server.clone()); // complete the handshake first
        client.write_all(b"hs").unwrap();
        client.read_exact(&mut [0u8; 2]).unwrap();
        t.join().unwrap();
        let (tx, rx) = mpsc::channel();
        let s2 = server.clone();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 8];
            tx.send(s2.read(&mut b).map(|n| n == 0).unwrap_or(true)).unwrap();
        });
        std::thread::sleep(Duration::from_millis(100));
        server.close();
        assert!(rx.recv_timeout(Duration::from_secs(5)).expect("close did not unblock read"));
        t.join().unwrap();
        assert!(server.write_all(b"late").is_err(), "write after close");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A push far over rustls' default 64 KiB buffer, written before the
    /// handshake while the reader is blocked.
    #[test]
    fn large_push() {
        let dir = temp_dir("large");
        ensure_certs(&dir).unwrap();
        let (server, mut client) = pair(&dir);
        let reader = echo_once(server.clone());
        let big = vec![7u8; 300_000];
        server.write_all(&big).unwrap();
        let mut got = vec![0u8; big.len()];
        client.read_exact(&mut got).unwrap();
        assert!(got == big);
        client.write_all(b"x").unwrap();
        reader.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Existing users have RSA-2048 PKCS#1 files from the Go helper
    /// (testdata/rsa, made once with openssl): reused untouched and served.
    #[test]
    fn reuses_rsa_pkcs1_pair() {
        let dir = temp_dir("rsa");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rsa");
        for f in [CA_CERT_FILE, CA_KEY_FILE, LEAF_CERT_FILE, LEAF_KEY_FILE] {
            std::fs::copy(src.join(f), dir.join(f)).unwrap();
        }
        ensure_certs(&dir).unwrap();
        for f in [CA_CERT_FILE, CA_KEY_FILE, LEAF_CERT_FILE, LEAF_KEY_FILE] {
            assert_eq!(std::fs::read(dir.join(f)).unwrap(), std::fs::read(src.join(f)).unwrap(), "{f} rewritten");
        }
        let (server, mut client) = pair(&dir);
        let t = echo_once(server);
        client.write_all(b"rsa").unwrap();
        let mut b = [0u8; 3];
        client.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"rsa");
        t.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expiring_cert_is_regenerated() {
        let dir = temp_dir("expiry");
        let now = rcgen::date_time_ymd(1970, 1, 1) + SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let write = |f: &str, after: Duration| {
            let key = KeyPair::generate().unwrap();
            let mut p = CertificateParams::default();
            p.not_before = now - DAY;
            p.not_after = now + after;
            std::fs::write(dir.join(f), p.self_signed(&key).unwrap().pem()).unwrap();
        };
        write(CA_CERT_FILE, 2 * DAY);
        assert!(cert_valid(&dir.join(CA_CERT_FILE)));
        write(LEAF_CERT_FILE, Duration::from_secs(3600));
        assert!(!cert_valid(&dir.join(LEAF_CERT_FILE)));
        assert!(!cert_valid(&dir.join("missing.crt")));
        ensure_certs(&dir).unwrap();
        assert!(cert_valid(&dir.join(LEAF_CERT_FILE)));
        let leaf = CertificateDer::from_pem_file(dir.join(LEAF_CERT_FILE)).unwrap();
        let days = not_after(&leaf).unwrap().duration_since(SystemTime::now()).unwrap().as_secs() / 86400;
        assert!((3650..=3652).contains(&days), "{days} days");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn not_after_of_rsa_fixture() {
        let der = CertificateDer::from_pem_file(Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rsa/master.crt")).unwrap();
        // openssl: notAfter=Oct 30 20:41:19 2036 GMT
        assert_eq!(not_after(&der).unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs(), 2109012079);
    }
}
