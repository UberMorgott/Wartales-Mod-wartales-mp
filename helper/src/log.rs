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
        (Log::new(move |s| sink.lock().unwrap().push(s.to_string())), lines)
    }

    pub fn line(&self, s: &str) {
        (self.0)(s)
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

/// Fills buf from the OS CSPRNG.
pub fn rand_bytes(buf: &mut [u8]) {
    use ring::rand::SecureRandom;
    ring::rand::SystemRandom::new().fill(buf).expect("OS random source failed");
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
}
