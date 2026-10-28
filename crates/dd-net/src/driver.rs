//! Socket I/O backends. Both are non-blocking and poll-driven so the feed
//! thread can busy-spin on its pinned core.
//!
//! * [`IoBackend::Std`]: `recv`/`send` on a non-blocking socket. One syscall per poll.
//! * [`IoBackend::Uring`]: io_uring. A recv is armed once and its completion
//!   is detected by reading the shared completion ring — **no syscall while
//!   waiting**. With `sqpoll_cpu` set, a kernel thread (pinned to that CPU,
//!   e.g. the cold-path core 0) also consumes submissions, so re-arming the
//!   recv needs no syscall either: the steady state is syscall-free.

use std::io::{self, Read, Write};
use std::net::TcpStream;

use crate::buf::RecvBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoBackend {
    Std,
    /// io_uring; `sqpoll_cpu: Some(cpu)` enables a kernel submission-poll
    /// thread pinned to `cpu`.
    Uring { sqpoll_cpu: Option<u32> },
}

impl IoBackend {
    /// Checks the backend can be used on this host (io_uring may be disabled
    /// by `kernel.io_uring_disabled`, seccomp, or container policy).
    pub fn probe(self) -> io::Result<()> {
        match self {
            IoBackend::Std => Ok(()),
            #[cfg(target_os = "linux")]
            IoBackend::Uring { sqpoll_cpu } => {
                let mut b = io_uring::IoUring::<io_uring::squeue::Entry, io_uring::cqueue::Entry>::builder();
                if let Some(cpu) = sqpoll_cpu {
                    b.setup_sqpoll(2_000).setup_sqpoll_cpu(cpu);
                }
                b.build(8).map(drop)
            }
            #[cfg(not(target_os = "linux"))]
            IoBackend::Uring { .. } => Err(io::Error::new(io::ErrorKind::Unsupported, "io_uring requires Linux")),
        }
    }
}

/// Receive buffer + backend, kept together so the io_uring invariant
/// ("the kernel only ever writes to *this* buffer's spare region, and the
/// buffer is not compacted or freed while a recv is in flight") is local to
/// this module.
pub(crate) struct Io {
    // Field order matters: `be` drops first so an in-flight recv is cancelled
    // and reaped before `rx` is freed.
    be: Backend,
    pub(crate) rx: RecvBuf,
}

// Constructed once per connection; boxing the larger variant would add a
// pointer hop to every poll for no benefit.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Std(TcpStream),
    #[cfg(target_os = "linux")]
    Uring(uring::Uring),
}

impl Io {
    pub(crate) fn new(sock: TcpStream, backend: IoBackend, rx_cap: usize) -> io::Result<Self> {
        let be = match backend {
            IoBackend::Std => {
                sock.set_nonblocking(true)?;
                Backend::Std(sock)
            }
            #[cfg(target_os = "linux")]
            IoBackend::Uring { sqpoll_cpu } => Backend::Uring(uring::Uring::new(sock, sqpoll_cpu)?),
            #[cfg(not(target_os = "linux"))]
            IoBackend::Uring { .. } => return Err(io::Error::new(io::ErrorKind::Unsupported, "io_uring requires Linux")),
        };
        Ok(Self { be, rx: RecvBuf::new(rx_cap) })
    }

    /// Appends any newly received bytes to `rx`. Returns the count (0 = none yet).
    /// EOF is reported as `ErrorKind::UnexpectedEof`.
    #[inline]
    pub(crate) fn poll_recv(&mut self) -> io::Result<usize> {
        match &mut self.be {
            Backend::Std(s) => {
                self.rx.compact();
                if self.rx.spare() == 0 {
                    return Err(io::Error::new(io::ErrorKind::OutOfMemory, "receive buffer full"));
                }
                match s.read(self.rx.tail_mut()) {
                    Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(n) => {
                        self.rx.advance(n);
                        Ok(n)
                    }
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(0),
                    Err(e) => Err(e),
                }
            }
            #[cfg(target_os = "linux")]
            Backend::Uring(u) => u.poll_recv(&mut self.rx),
        }
    }

    /// Sends all of `buf`, spinning if the socket is momentarily full.
    /// Used for handshakes, subscriptions and pongs — never per market event.
    pub(crate) fn send_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match &mut self.be {
            Backend::Std(s) => {
                let mut off = 0;
                while off < buf.len() {
                    match s.write(&buf[off..]) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(n) => off += n,
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                            std::hint::spin_loop();
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(())
            }
            #[cfg(target_os = "linux")]
            Backend::Uring(u) => u.send_all(buf),
        }
    }
}

#[cfg(target_os = "linux")]
mod uring {
    use std::io;
    use std::net::TcpStream;
    use std::os::fd::AsRawFd;

    use io_uring::{IoUring, opcode, types};

    use crate::buf::RecvBuf;

    const RECV: u64 = 1;
    const SEND: u64 = 2;
    const CANCEL: u64 = 3;
    const EAGAIN: i32 = 11;
    const EINTR: i32 = 4;

    pub(super) struct Uring {
        ring: IoUring,
        sock: TcpStream,
        recv_in_flight: bool,
        /// A recv completion reaped while waiting for a send.
        stashed_recv: Option<i32>,
    }

    impl Uring {
        pub(super) fn new(sock: TcpStream, sqpoll_cpu: Option<u32>) -> io::Result<Self> {
            let mut b = IoUring::builder();
            if let Some(cpu) = sqpoll_cpu {
                b.setup_sqpoll(2_000).setup_sqpoll_cpu(cpu);
            }
            // Deliberately *not* COOP_TASKRUN/DEFER_TASKRUN: completions must be
            // posted while our thread busy-spins in userspace without entering the kernel.
            let ring = b.build(64)?;
            Ok(Self { ring, sock, recv_in_flight: false, stashed_recv: None })
        }

        fn fd(&self) -> types::Fd {
            types::Fd(self.sock.as_raw_fd())
        }

        fn push(&mut self, e: &io_uring::squeue::Entry) -> io::Result<()> {
            // SAFETY: every buffer referenced by our SQEs outlives the operation:
            // recv targets `RecvBuf`'s spare region, which is neither compacted nor
            // freed while `recv_in_flight` (see `Io` field order and `Drop` below);
            // send buffers are borrowed for the whole of `send_all`, which waits
            // for the completion before returning.
            unsafe { self.ring.submission().push(e) }.map_err(|_| io::Error::other("io_uring submission queue full"))
        }

        fn finish_recv(res: i32, rx: &mut RecvBuf) -> io::Result<usize> {
            match res {
                0 => Err(io::ErrorKind::UnexpectedEof.into()),
                n if n > 0 => {
                    rx.advance(n as usize);
                    Ok(n as usize)
                }
                e if -e == EAGAIN || -e == EINTR => Ok(0),
                e => Err(io::Error::from_raw_os_error(-e)),
            }
        }

        #[inline]
        pub(super) fn poll_recv(&mut self, rx: &mut RecvBuf) -> io::Result<usize> {
            if let Some(res) = self.stashed_recv.take() {
                return Self::finish_recv(res, rx);
            }
            if !self.recv_in_flight {
                rx.compact(); // safe: nothing in flight
                if rx.spare() == 0 {
                    return Err(io::Error::new(io::ErrorKind::OutOfMemory, "receive buffer full"));
                }
                let e = opcode::Recv::new(self.fd(), rx.tail_ptr(), rx.spare() as u32).build().user_data(RECV);
                self.push(&e)?;
                self.ring.submit()?; // no-op syscall-wise under SQPOLL unless the poller sleeps
                self.recv_in_flight = true;
            }
            // Reading the completion ring is a shared-memory load: no syscall.
            let mut out = Ok(0);
            for cqe in self.ring.completion() {
                if cqe.user_data() == RECV {
                    self.recv_in_flight = false;
                    out = Self::finish_recv(cqe.result(), rx);
                }
            }
            out
        }

        pub(super) fn send_all(&mut self, buf: &[u8]) -> io::Result<()> {
            let mut off = 0;
            while off < buf.len() {
                let rest = &buf[off..];
                let e = opcode::Send::new(self.fd(), rest.as_ptr(), rest.len() as u32).build().user_data(SEND);
                self.push(&e)?;
                let res = loop {
                    self.ring.submit_and_wait(1)?;
                    let mut sent = None;
                    for cqe in self.ring.completion() {
                        match cqe.user_data() {
                            SEND => sent = Some(cqe.result()),
                            RECV => {
                                self.recv_in_flight = false;
                                self.stashed_recv = Some(cqe.result());
                            }
                            _ => {}
                        }
                    }
                    if let Some(r) = sent {
                        break r;
                    }
                };
                match res {
                    n if n > 0 => off += n as usize,
                    0 => return Err(io::ErrorKind::WriteZero.into()),
                    e if -e == EAGAIN || -e == EINTR => {}
                    e => return Err(io::Error::from_raw_os_error(-e)),
                }
            }
            Ok(())
        }
    }

    impl Drop for Uring {
        fn drop(&mut self) {
            if !self.recv_in_flight {
                return;
            }
            // The kernel still holds a pointer into RecvBuf: cancel and reap it
            // before RecvBuf is freed.
            let _ = self.sock.shutdown(std::net::Shutdown::Both);
            let e = opcode::AsyncCancel::new(RECV).build().user_data(CANCEL);
            if self.push(&e).is_err() {
                return;
            }
            while self.recv_in_flight {
                if self.ring.submit_and_wait(1).is_err() {
                    break;
                }
                for cqe in self.ring.completion() {
                    if cqe.user_data() == RECV {
                        self.recv_in_flight = false;
                    }
                }
            }
        }
    }
}
