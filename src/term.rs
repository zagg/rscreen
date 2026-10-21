//! Terminal: mode raw, taille, signalfd, poll — le strict nécessaire, sans crate.

use std::io;
use std::mem;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

pub struct RawTTY {
    fd: RawFd,
    saved: libc::termios,
    active: bool,
}

impl RawTTY {
    /// active le mode raw sur `fd` (normalement stdin)
    pub fn enable(fd: RawFd) -> io::Result<RawTTY> {
        let mut t: libc::termios = unsafe { mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let saved = t;
        unsafe { libc::cfmakeraw(&mut t) };
        // ctrl +- etc. restent lisibles; pas de traduction LF->CRLF côté tty:
        // c'est nous qui écrivons \r\n quand il faut.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(RawTTY {
            fd,
            saved,
            active: true,
        })
    }

    pub fn disable(&mut self) {
        if self.active {
            unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.saved) };
            self.active = false;
        }
    }
}

impl Drop for RawTTY {
    fn drop(&mut self) {
        self.disable();
    }
}

pub fn term_size(fd: RawFd) -> io::Result<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as libc::c_ulong, &mut ws) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if ws.ws_row == 0 || ws.ws_col == 0 {
        return Err(io::Error::other("taille de terminal invalide"));
    }
    Ok((ws.ws_row, ws.ws_col))
}

/// construit un mask de signaux
pub fn sigmask(sigs: &[i32]) -> io::Result<libc::sigset_t> {
    let mut set: libc::sigset_t = unsafe { mem::zeroed() };
    unsafe {
        if libc::sigemptyset(&mut set) != 0 {
            return Err(io::Error::last_os_error());
        }
        for s in sigs {
            if libc::sigaddset(&mut set, *s) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // on bloque ces signaux pour pouvoir les lire via signalfd()
        if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(set)
}

pub fn make_signalfd(mask: &libc::sigset_t) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::signalfd(-1, mask, libc::SFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok::<OwnedFd, io::Error>(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// lit un signal depuis un signalfd; renvoie le numéro (0 = rien)
pub fn read_signal(fd: RawFd) -> i32 {
    let mut si: libc::signalfd_siginfo = unsafe { mem::zeroed() };
    let n = unsafe {
        libc::read(
            fd,
            &mut si as *mut libc::signalfd_siginfo as *mut libc::c_void,
            mem::size_of::<libc::signalfd_siginfo>(),
        )
    };
    if n as usize == mem::size_of::<libc::signalfd_siginfo>() {
        si.ssi_signo as i32
    } else {
        0
    }
}

pub fn ignore_signal(sig: i32) {
    unsafe {
        libc::signal(sig, libc::SIG_IGN);
    }
}

/// petit encapsulateur poll(2)
#[derive(Default)]
pub struct Poller {
    pub fds: Vec<libc::pollfd>,
    pub toks: Vec<(u32, usize)>,
}

impl Poller {
    pub fn clear(&mut self) {
        self.fds.clear();
        self.toks.clear();
    }

    pub fn add(&mut self, fd: RawFd, tok: u32, idx: usize) {
        self.fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL,
            revents: 0,
        });
        self.toks.push((tok, idx));
    }

    /// retourne (jeton, index, revents) pour chaque fd prêt
    pub fn wait(&mut self, timeout_ms: i32) -> io::Result<Vec<(u32, usize, i16)>> {
        let n = loop {
            let r = unsafe {
                libc::poll(
                    self.fds.as_mut_ptr(),
                    self.fds.len() as libc::nfds_t,
                    timeout_ms,
                )
            };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(e);
            }
            break r;
        };
        let mut ready = Vec::new();
        if n > 0 {
            for (i, p) in self.fds.iter().enumerate() {
                if p.revents != 0 {
                    let (tok, idx) = self.toks[i];
                    ready.push((tok, idx, p.revents));
                }
            }
        }
        Ok(ready)
    }
}

pub const TOK_LISTEN: u32 = 1;
pub const TOK_CLIENT: u32 = 2;
pub const TOK_WINDOW: u32 = 3;
pub const TOK_STDIN: u32 = 4;
pub const TOK_SIGNAL: u32 = 5;
