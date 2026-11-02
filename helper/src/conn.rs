//! `Conn` is Go's `net.Conn`: one byte stream that one thread reads while
//! others write, and that any thread can close to unblock the reader. TCP,
//! the master's TLS, the SDR bridge's per-peer streams and the in-memory
//! `pipe` of the tests all implement it, so the protocols on top (wsx, link)
//! are written once.

use std::collections::VecDeque;
use std::io;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub trait Conn: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize>;
    /// Writes all of buf or fails.
    fn write_all(&self, buf: &[u8]) -> io::Result<()>;
    /// None = block forever. A timed-out read fails with TimedOut or WouldBlock.
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()>;
    /// Go's Close: both directions, unblocks a pending read. Idempotent.
    fn close(&self);
    /// The remote address, when there is one (for logs and inbound checks).
    fn peer_addr(&self) -> Option<SocketAddr> {
        None
    }
}

pub type ConnRef = Arc<dyn Conn>;

impl Conn for TcpStream {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut &*self, buf)
    }
    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        io::Write::write_all(&mut &*self, buf)
    }
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, d)
    }
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, d)
    }
    fn close(&self) {
        let _ = self.shutdown(Shutdown::Both); // already closed is fine
    }
    fn peer_addr(&self) -> Option<SocketAddr> {
        TcpStream::peer_addr(self).ok()
    }
}

/// io::Read + io::Write over a shared Conn (for BufReader, read_exact, ...).
pub struct Io(pub ConnRef);

impl io::Read for Io {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl io::Write for Io {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(buf).map(|_| buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Is err a read timeout (Go's os.ErrDeadlineExceeded)?
pub fn is_timeout(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

/// An in-memory duplex pair, Go's net.Pipe for tests (buffered, unbounded).
pub fn pipe() -> (ConnRef, ConnRef) {
    let a = Arc::new(Half::default());
    let b = Arc::new(Half::default());
    (
        Arc::new(PipeEnd { rx: a.clone(), tx: b.clone(), timeout: Mutex::new(None) }),
        Arc::new(PipeEnd { rx: b, tx: a, timeout: Mutex::new(None) }),
    )
}

#[derive(Default)]
struct Half {
    q: Mutex<(VecDeque<u8>, bool)>, // bytes, closed
    cv: Condvar,
}

impl Half {
    fn close(&self) {
        self.q.lock().unwrap().1 = true;
        self.cv.notify_all();
    }
}

struct PipeEnd {
    rx: Arc<Half>,
    tx: Arc<Half>,
    timeout: Mutex<Option<Duration>>,
}

impl Conn for PipeEnd {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let deadline = self.timeout.lock().unwrap().map(|d| Instant::now() + d);
        let mut q = self.rx.q.lock().unwrap();
        loop {
            if !q.0.is_empty() {
                let n = buf.len().min(q.0.len());
                for (i, b) in q.0.drain(..n).enumerate() {
                    buf[i] = b;
                }
                return Ok(n);
            }
            if q.1 {
                return Ok(0);
            }
            match deadline {
                None => q = self.rx.cv.wait(q).unwrap(),
                Some(t) => {
                    let now = Instant::now();
                    if now >= t {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    q = self.rx.cv.wait_timeout(q, t - now).unwrap().0;
                }
            }
        }
    }
    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        let mut q = self.tx.q.lock().unwrap();
        if q.1 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        q.0.extend(buf);
        self.tx.cv.notify_all();
        Ok(())
    }
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        *self.timeout.lock().unwrap() = d;
        Ok(())
    }
    fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(()) // writes never block
    }
    fn close(&self) {
        self.rx.close();
        self.tx.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_roundtrip_timeout_close() {
        let (a, b) = pipe();
        a.write_all(b"hi").unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(b.read(&mut buf).unwrap(), 2);
        b.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        assert!(is_timeout(&b.read(&mut buf).unwrap_err()));
        let t = std::thread::spawn({
            let b = b.clone();
            move || {
                b.set_read_timeout(None).unwrap();
                let mut buf = [0u8; 1];
                b.read(&mut buf).unwrap()
            }
        });
        std::thread::sleep(Duration::from_millis(20));
        a.close();
        assert_eq!(t.join().unwrap(), 0);
        assert!(b.write_all(b"x").is_err());
    }
}
