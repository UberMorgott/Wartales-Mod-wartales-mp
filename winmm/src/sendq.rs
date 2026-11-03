//! Port of internal/sendq: a connection's ordered send queue of bounded size,
//! drained by a thread of its own, so a sender never waits on its peer: one
//! player's stalled connection must not hold up the frames of everyone else
//! (the relay's fan-out from the host, the master's lobby pushes). A peer that
//! falls more than the budget behind is disconnected instead of being waited
//! for.

use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::conn::ConnRef;

/// What a send gets once the peer fell too far behind.
const OVERFLOW: &str = "sendq: the peer is not reading; send queue overflowed";
const CLOSED: &str = "use of closed network connection"; // Go's net.ErrClosed

/// How long `drain` keeps writing to a peer before the connection is closed anyway.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Sends to one connection; clones share the queue.
#[derive(Clone)]
pub struct Queue(Arc<Inner>);

struct Inner {
    c: ConnRef,
    budget: usize,
    st: Mutex<State>,
}

#[derive(Default)]
struct State {
    bufs: Vec<Vec<u8>>,
    size: usize,                          // bytes queued or being written
    err: Option<(io::ErrorKind, String)>, // once set, nothing more is accepted
    running: bool,                        // a writer thread is draining bufs
    drain: bool,                          // close c once bufs are written
    flush_by: Option<Instant>,            // drain's deadline for the writes still running
}

fn error(e: &(io::ErrorKind, String)) -> io::Error {
    io::Error::new(e.0, e.1.clone())
}

impl Queue {
    /// A queue that holds at most budget bytes for c.
    pub fn new(c: ConnRef, budget: usize) -> Self {
        Queue(Arc::new(Inner {
            c,
            budget,
            st: Mutex::default(),
        }))
    }

    /// Queues bufs to be written back to back, in order with every earlier
    /// send, and returns at once. An error means nothing was queued: the
    /// connection already failed or was closed, or this send overflowed the
    /// budget, which closes it.
    pub fn send(&self, bufs: &[&[u8]]) -> io::Result<()> {
        let n: usize = bufs.iter().map(|b| b.len()).sum();
        let q = &self.0;
        let mut st = q.st.lock().unwrap();
        if let Some(e) = &st.err {
            return Err(error(e));
        }
        if st.size + n > q.budget {
            st.err = Some((io::ErrorKind::Other, OVERFLOW.into()));
            st.bufs = Vec::new();
            drop(st);
            q.c.close(); // the peer is dropped; its reader sees the close
            return Err(io::Error::other(OVERFLOW));
        }
        st.bufs.push(bufs.concat());
        st.size += n;
        let start = !st.running;
        st.running = true;
        drop(st);
        if start {
            let q = self.clone();
            let spawned = std::thread::Builder::new().spawn(move || {
                if catch_unwind(AssertUnwindSafe(|| q.run())).is_err() {
                    crate::applog::append("sendq: writer thread panicked");
                    q.fail(&io::Error::other("sendq: writer panicked"));
                }
            });
            if let Err(e) = spawned {
                self.fail(&e);
            }
        }
        Ok(())
    }

    /// Writes until the queue is empty, then exits: an idle connection holds no thread.
    fn run(&self) {
        let q = &self.0;
        loop {
            let mut st = q.st.lock().unwrap();
            let bufs = std::mem::take(&mut st.bufs);
            if bufs.is_empty() {
                st.running = false;
                let drain = st.drain;
                drop(st);
                if drain {
                    q.c.close();
                }
                return;
            }
            drop(st);
            let mut n = 0;
            for b in &bufs {
                if let Err(e) = self.write(b) {
                    self.fail(&e);
                    return;
                }
                n += b.len();
            }
            q.st.lock().unwrap().size -= n;
        }
    }

    /// One write, bounded by drain's deadline once one is set.
    fn write(&self, b: &[u8]) -> io::Result<()> {
        if let Some(t) = self.0.st.lock().unwrap().flush_by {
            let left = t.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "i/o timeout"));
            }
            self.0.c.set_write_timeout(Some(left))?;
        }
        self.0.c.write_all(b)
    }

    fn fail(&self, err: &io::Error) {
        let mut st = self.0.st.lock().unwrap();
        st.err.get_or_insert((err.kind(), err.to_string()));
        st.bufs = Vec::new();
        st.running = false;
        drop(st);
        self.0.c.close();
    }

    /// Drops whatever is queued and closes the connection now.
    pub fn close(&self) {
        let mut st = self.0.st.lock().unwrap();
        st.err
            .get_or_insert((io::ErrorKind::NotConnected, CLOSED.into()));
        st.bufs = Vec::new();
        drop(st);
        self.0.c.close();
    }

    /// Accepts nothing more, writes what is queued (for at most FLUSH_TIMEOUT)
    /// and then closes the connection. It does not wait.
    pub fn drain(&self) {
        let mut st = self.0.st.lock().unwrap();
        st.err
            .get_or_insert((io::ErrorKind::NotConnected, CLOSED.into()));
        st.drain = true;
        if !st.running {
            drop(st);
            self.0.c.close();
            return;
        }
        st.flush_by = Some(Instant::now() + FLUSH_TIMEOUT);
        drop(st);
        let _ = self.0.c.set_write_timeout(Some(FLUSH_TIMEOUT)); // bounds a write already under way
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{pipe, Conn};
    use std::sync::Condvar;
    use std::time::Instant;

    /// A peer that never reads: every write blocks until close (net.Pipe's stall).
    #[derive(Default)]
    struct Stall {
        closed: Mutex<bool>,
        cv: Condvar,
    }

    impl Conn for Stall {
        fn read(&self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn write_all(&self, _: &[u8]) -> io::Result<()> {
            let mut c = self.closed.lock().unwrap();
            while !*c {
                c = self.cv.wait(c).unwrap();
            }
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
        fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
        fn close(&self) {
            *self.closed.lock().unwrap() = true;
            self.cv.notify_all();
        }
    }

    fn read_all(c: &ConnRef) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match c.read(&mut buf).unwrap() {
                0 => return out,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[test]
    fn send_never_waits_on_the_peer() {
        let q = Queue::new(Arc::new(Stall::default()), 1 << 20);
        let start = Instant::now();
        for _ in 0..100 {
            q.send(&[b"frame"]).unwrap();
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "send waited on a peer that does not read"
        );
        q.close();
    }

    #[test]
    fn order_is_kept() {
        let (a, b) = pipe();
        let q = Queue::new(a, 1 << 20);
        let mut want = String::new();
        for i in 0..50u8 {
            let s = format!("{}|", (b'a' + i % 26) as char);
            want += &s;
            q.send(&[&s.as_bytes()[..1], &s.as_bytes()[1..]]).unwrap();
        }
        q.drain();
        assert_eq!(String::from_utf8(read_all(&b)).unwrap(), want);
    }

    #[test]
    fn overflow_drops_the_peer() {
        let stall = Arc::new(Stall::default());
        let q = Queue::new(stall.clone(), 10);
        q.send(&[&[0; 6]]).unwrap();
        assert_eq!(q.send(&[&[0; 6]]).unwrap_err().to_string(), OVERFLOW);
        assert_eq!(
            q.send(&[&[1]]).unwrap_err().to_string(),
            OVERFLOW,
            "a send after the overflow"
        );
        assert!(
            *stall.closed.lock().unwrap(),
            "the overflow did not close the peer"
        );
    }

    #[test]
    fn drain_of_an_idle_queue_closes() {
        let (a, b) = pipe();
        let q = Queue::new(a, 10);
        q.drain();
        assert_eq!(b.read(&mut [0u8; 1]).unwrap(), 0, "want EOF");
        assert!(q.send(&[&[1]]).is_err(), "a drained queue took a send");
    }
}
