//! Byte transports: a real serial port, a pseudo-terminal (simulator and wfview's virtual
//! serial port) and an in-memory pipe for tests.

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

pub trait Transport: Send {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    /// Read what is available, waiting at most `timeout`. Returns 0 on timeout.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
    fn describe(&self) -> String;
}

/// A serial port (or the slave side of a pseudo-terminal) opened with POSIX calls:
/// exclusive (TIOCEXCL), raw 8N1, no flow control, DTR and RTS dropped.
pub struct SerialTransport {
    file: std::fs::File,
    path: String,
}

fn check(rc: libc::c_int, what: &str, path: &str) -> io::Result<()> {
    if rc < 0 {
        let e = io::Error::last_os_error();
        return Err(io::Error::new(e.kind(), format!("{path}: {what}: {e}")));
    }
    Ok(())
}

fn poll_fd(fd: libc::c_int, timeout: Duration) -> io::Result<bool> {
    let mut fds = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: one valid pollfd.
    let rc = unsafe { libc::poll(&mut fds, 1, ms) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(e) };
    }
    if rc > 0 && fds.revents & (libc::POLLHUP | libc::POLLNVAL | libc::POLLERR) != 0 && fds.revents & libc::POLLIN == 0 {
        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device closed"));
    }
    Ok(rc > 0)
}

impl SerialTransport {
    pub fn open(path: &str, baud: u32) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let c = std::ffi::CString::new(path).map_err(|_| io::Error::other("bad path"))?;
        // O_NONBLOCK so open does not wait for carrier detect; cleared below.
        // SAFETY: c is a valid NUL-terminated path.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK) };
        check(fd, "open", path)?;
        // SAFETY: fd was just opened and is owned by this File from here on.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let fd = file.as_raw_fd();
        // SAFETY: plain ioctl/termios calls on our own descriptor with valid pointers.
        unsafe {
            // An advisory lock as well: TIOCEXCL alone does not stop a second open of a pty.
            if libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) < 0 {
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, format!("{path}: in use by another program")));
            }
            if libc::ioctl(fd, libc::TIOCEXCL as libc::c_ulong) < 0 {
                let e = io::Error::last_os_error();
                return Err(io::Error::new(e.kind(), format!("{path}: in use by another program ({e})")));
            }
            let mut t: libc::termios = std::mem::zeroed();
            check(libc::tcgetattr(fd, &mut t), "tcgetattr", path)?;
            libc::cfmakeraw(&mut t);
            t.c_cflag |= libc::CLOCAL | libc::CREAD;
            t.c_cflag &= !(libc::CRTSCTS | libc::CSTOPB | libc::PARENB);
            t.c_cflag = (t.c_cflag & !libc::CSIZE) | libc::CS8;
            t.c_cc[libc::VMIN] = 0;
            t.c_cc[libc::VTIME] = 0;
            check(libc::cfsetspeed(&mut t, libc::speed_t::from(baud)), "baud rate", path)?;
            check(libc::tcsetattr(fd, libc::TCSANOW, &t), "tcsetattr", path)?;
            let flags = libc::fcntl(fd, libc::F_GETFL);
            check(flags, "fcntl", path)?;
            check(libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK), "fcntl", path)?;
            // Drop DTR and RTS: on some radios they key the transmitter. A pty has no modem lines.
            let lines: libc::c_int = libc::TIOCM_DTR | libc::TIOCM_RTS;
            libc::ioctl(fd, libc::TIOCMBIC, &lines);
            libc::tcflush(fd, libc::TCIOFLUSH);
        }
        Ok(Self { file, path: path.to_string() })
    }
}

impl Transport for SerialTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        use std::os::fd::AsRawFd;
        if !poll_fd(self.file.as_raw_fd(), timeout)? {
            return Ok(0);
        }
        match self.file.read(buf) {
            Ok(0) => Err(io::Error::new(io::ErrorKind::BrokenPipe, "device closed")),
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn describe(&self) -> String {
        self.path.clone()
    }
}

impl Transport for Box<dyn Transport> {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).write_all(data)
    }
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        (**self).read(buf, timeout)
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
}

/// Wraps a transport and keeps a copy of every byte received, for recordings and fixtures.
pub struct RecordingTransport<T: Transport> {
    inner: T,
    pub received: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl<T: Transport> RecordingTransport<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, received: Default::default() }
    }
}

impl<T: Transport> Transport for RecordingTransport<T> {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.inner.write_all(data)
    }
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let n = self.inner.read(buf, timeout)?;
        self.received.lock().unwrap().extend_from_slice(&buf[..n]);
        Ok(n)
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

/// One end of an in-memory byte pipe.
pub struct PipeEnd {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    name: String,
}

/// Two connected pipe ends, like a null-modem cable.
pub fn pipe_pair() -> (PipeEnd, PipeEnd) {
    let (a_tx, b_rx) = mpsc::channel();
    let (b_tx, a_rx) = mpsc::channel();
    (
        PipeEnd { tx: a_tx, rx: a_rx, pending: Vec::new(), name: "pipe A".into() },
        PipeEnd { tx: b_tx, rx: b_rx, pending: Vec::new(), name: "pipe B".into() },
    )
}

impl Transport for PipeEnd {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.tx.send(data.to_vec()).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if self.pending.is_empty() {
            match self.rx.recv_timeout(timeout) {
                Ok(v) => self.pending = v,
                Err(RecvTimeoutError::Timeout) => return Ok(0),
                Err(RecvTimeoutError::Disconnected) => return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed")),
            }
            while let Ok(more) = self.rx.try_recv() {
                self.pending.extend(more);
            }
        }
        let n = self.pending.len().min(buf.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}

/// The master side of a pseudo-terminal. The simulator serves on this; apps open the slave path
/// as if it were a serial port.
pub struct PtyMaster {
    file: std::fs::File,
    pub slave_path: String,
    // Keep the slave open so the master does not see EOF/EIO before the app connects.
    _slave: std::fs::File,
}

impl PtyMaster {
    pub fn open() -> io::Result<Self> {
        use std::os::fd::FromRawFd;
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: openpty writes two descriptors and a NUL-terminated name into our buffers.
        let rc = unsafe { libc::openpty(&mut master, &mut slave, name.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openpty returned success, so both descriptors are open and owned by us.
        let (file, slave_file) = unsafe { (std::fs::File::from_raw_fd(master), std::fs::File::from_raw_fd(slave)) };
        // SAFETY: name is NUL-terminated by openpty.
        let slave_path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
        // Raw mode on the slave so CI-V bytes (FD, FE, ...) pass through unchanged.
        // SAFETY: termios is plain data; the calls only touch the slave descriptor.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(slave, &mut t) == 0 {
                libc::cfmakeraw(&mut t);
                libc::tcsetattr(slave, libc::TCSANOW, &t);
            }
        }
        Ok(Self { file, slave_path, _slave: slave_file })
    }

    fn poll(&self, timeout: Duration) -> io::Result<bool> {
        use std::os::fd::AsRawFd;
        poll_fd(self.file.as_raw_fd(), timeout)
    }
}

impl Transport for PtyMaster {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if !self.poll(timeout)? {
            return Ok(0);
        }
        self.file.read(buf)
    }

    fn describe(&self) -> String {
        format!("pty master for {}", self.slave_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_moves_bytes_both_ways() {
        let (mut a, mut b) = pipe_pair();
        a.write_all(b"abc").unwrap();
        a.write_all(b"def").unwrap();
        let mut buf = [0u8; 16];
        let n = b.read(&mut buf, Duration::from_millis(100)).unwrap();
        assert_eq!(&buf[..n], b"abcdef");
        assert_eq!(b.read(&mut buf, Duration::from_millis(10)).unwrap(), 0);
        b.write_all(&[0xFE, 0xFD]).unwrap();
        let n = a.read(&mut buf, Duration::from_millis(100)).unwrap();
        assert_eq!(&buf[..n], &[0xFE, 0xFD]);
        drop(b);
        assert!(a.read(&mut buf, Duration::from_millis(10)).is_err());
    }

    #[test]
    fn pty_carries_binary_bytes_to_a_serial_port_open() {
        let mut m = PtyMaster::open().unwrap();
        let mut s = SerialTransport::open(&m.slave_path, 115_200).unwrap();
        let all: Vec<u8> = (0..=255u8).collect();
        s.write_all(&all).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 512];
        while got.len() < all.len() {
            let n = m.read(&mut buf, Duration::from_millis(500)).unwrap();
            assert!(n > 0, "timed out after {} bytes", got.len());
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, all);
        m.write_all(&[0xFE, 0xFE, 0x0D, 0x0A, 0x11, 0x13, 0xFD]).unwrap();
        let mut got = Vec::new();
        while got.len() < 7 {
            let n = s.read(&mut buf, Duration::from_millis(500)).unwrap();
            assert!(n > 0);
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, vec![0xFE, 0xFE, 0x0D, 0x0A, 0x11, 0x13, 0xFD]);
    }
}
