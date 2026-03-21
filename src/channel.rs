use super::suspension::Suspension;
use std::fs::File;
use std::io;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct Waker {
    tx: tokio::sync::mpsc::UnboundedSender<Suspension>,
    notified: Arc<AtomicBool>,
    sender: Arc<OwnedFd>,
}

impl Waker {
    pub fn send(&self, suspension: Suspension) {
        let _ = self.tx.send(suspension);

        if !self.notified.swap(true, Ordering::AcqRel) {
            let fd = self.sender.as_raw_fd();
            let buf = [0u8];
            unsafe {
                let _ = libc::write(fd, buf.as_ptr() as *const _, 1);
            }
        }
    }
}

pub struct Channel {
    rx: tokio::sync::mpsc::UnboundedReceiver<Suspension>,
    notified: Arc<AtomicBool>,
    receiver: File,
    waker: Waker,
}

impl Channel {
    pub fn new() -> Channel {
        let [receiver, sender] = new_raw_pipe().expect("create pipe");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let notified = Arc::new(AtomicBool::new(false));
        Self {
            rx,
            notified: notified.clone(),
            receiver: unsafe { File::from_raw_fd(receiver) },
            waker: Waker {
                tx,
                notified,
                sender: Arc::new(unsafe { OwnedFd::from_raw_fd(sender) }),
            },
        }
    }

    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    pub fn resume_all(&mut self) {
        loop {
            let mut buf = [0u8; 64];
            loop {
                match self.receiver.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            while let Ok(suspension) = self.rx.try_recv() {
                let _ = suspension.resume();
            }
            self.notified.store(false, Ordering::Release);
            if self.rx.is_empty() {
                break;
            }
        }
    }

    pub fn raw_fd(&self) -> RawFd {
        self.receiver.as_raw_fd()
    }
}

pub(crate) fn new_raw_pipe() -> io::Result<[RawFd; 2]> {
    let mut fds: [RawFd; 2] = [-1, -1];

    unsafe {
        // For platforms that don't have `pipe2(2)` we need to manually set the
        // correct flags on the file descriptor.
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }

        for fd in &fds {
            if libc::fcntl(*fd, libc::F_SETFL, libc::O_NONBLOCK) != 0
                || libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) != 0
            {
                let err = io::Error::last_os_error();
                // Don't leak file descriptors. Can't handle closing error though.
                let _ = libc::close(fds[0]);
                let _ = libc::close(fds[1]);
                return Err(err);
            }
        }
    }

    Ok(fds)
}
