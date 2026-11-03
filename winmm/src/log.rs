//! The logger every module takes, and crypto-grade random bytes.

use std::sync::{Arc, Mutex};

/// A line sink: `applog::open` writes to wartales-mp.log, tests capture.
/// Cheap to clone; every module takes one like Go took a `*log.Logger`.
#[derive(Clone)]
pub struct Log(Arc<dyn Fn(&str) + Send + Sync>);

impl Log {
    pub fn new(f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Log(Arc::new(f))
    }

    /// Drops every line.
    pub fn discard() -> Self {
        Log::new(|_| {})
    }

    /// Keeps every line in memory, for tests.
    pub fn capture() -> (Self, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        (
            Log::new(move |s| sink.lock().unwrap().push(s.to_string())),
            lines,
        )
    }

    pub fn line(&self, s: &str) {
        (self.0)(s);
    }
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Log")
    }
}

/// `logf!(log, "fmt", args..)` is Go's `logger.Printf`.
#[macro_export]
macro_rules! logf {
    ($log:expr, $($arg:tt)*) => { $log.line(&format!($($arg)*)) };
}

use windows_sys::Win32::Security::Cryptography::{
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptGenRandom, BCryptHashData,
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_MD5_ALG_HANDLE, BCRYPT_SHA1_ALG_HANDLE,
    BCRYPT_SHA256_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

/// CNG takes u32 lengths.
const CHUNK: usize = 1 << 30;

/// Fills buf from the OS CSPRNG (Windows CNG).
pub fn rand_bytes(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(CHUNK) {
        // SAFETY: chunk is a live, writable buffer of exactly the length passed.
        let st = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                u32::try_from(chunk.len()).expect("bounded by CHUNK"),
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        assert!(st == 0, "OS random source failed: NTSTATUS {st:#x}");
    }
}

/// A CNG hash algorithm (pseudo-handles, Windows 10+).
#[derive(Clone, Copy)]
pub enum Alg {
    Md5,
    Sha1,
    Sha256,
}

/// An incremental CNG hash.
pub struct Hasher(BCRYPT_HASH_HANDLE, usize);

impl Hasher {
    pub fn new(alg: Alg) -> Self {
        let (h, n): (BCRYPT_ALG_HANDLE, usize) = match alg {
            Alg::Md5 => (BCRYPT_MD5_ALG_HANDLE, 16),
            Alg::Sha1 => (BCRYPT_SHA1_ALG_HANDLE, 20),
            Alg::Sha256 => (BCRYPT_SHA256_ALG_HANDLE, 32),
        };
        let mut hash = std::ptr::null_mut();
        // SAFETY: a pseudo-handle, no hash object buffer (CNG allocates it), no secret.
        let st = unsafe {
            BCryptCreateHash(
                h,
                &raw mut hash,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                0,
                0,
            )
        };
        assert!(st == 0, "BCryptCreateHash failed: NTSTATUS {st:#x}");
        Hasher(hash, n)
    }

    pub fn update(&mut self, data: &[u8]) {
        for chunk in data.chunks(CHUNK) {
            // SAFETY: self.0 is a live hash handle; chunk is readable for its length.
            let st = unsafe {
                BCryptHashData(
                    self.0,
                    chunk.as_ptr(),
                    u32::try_from(chunk.len()).expect("bounded by CHUNK"),
                    0,
                )
            };
            assert!(st == 0, "BCryptHashData failed: NTSTATUS {st:#x}");
        }
    }

    pub fn finish(self) -> Vec<u8> {
        let mut out = vec![0u8; self.1];
        // SAFETY: out holds exactly the digest size of the algorithm.
        let st = unsafe {
            BCryptFinishHash(
                self.0,
                out.as_mut_ptr(),
                u32::try_from(self.1).expect("a digest size"),
                0,
            )
        };
        assert!(st == 0, "BCryptFinishHash failed: NTSTATUS {st:#x}");
        out
    }
}

impl Drop for Hasher {
    fn drop(&mut self) {
        // SAFETY: the handle came from BCryptCreateHash and is destroyed once.
        unsafe { BCryptDestroyHash(self.0) };
    }
}

/// The digest of data.
pub fn digest(alg: Alg, data: &[u8]) -> Vec<u8> {
    let mut h = Hasher::new(alg);
    h.update(data);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_keeps_lines() {
        let (log, lines) = Log::capture();
        logf!(log, "a {}", 1);
        assert_eq!(*lines.lock().unwrap(), vec!["a 1".to_string()]);
        let mut b = [0u8; 16];
        rand_bytes(&mut b);
        assert_ne!(b, [0u8; 16]);
    }

    #[test]
    fn digests_match_known_vectors() {
        let hex = crate::wsx::hex;
        assert_eq!(
            hex(&digest(Alg::Md5, b"abc")),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hex(&digest(Alg::Sha1, b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        let mut h = Hasher::new(Alg::Sha256);
        h.update(b"a");
        h.update(b"bc");
        assert_eq!(
            hex(&h.finish()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
