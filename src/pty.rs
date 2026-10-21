//! PTY + lancement de processus, en appelant libc directement (pas de crate pty).

use std::collections::BTreeMap;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

pub struct Pty {
    pub master: OwnedFd,
    pub pid: libc::pid_t,
    /// chemin du tty esclave (pour le nom de fenêtre)
    pub tty: String,
}

fn open_pty_pair(rows: u16, cols: u16) -> io::Result<(OwnedFd, OwnedFd, String)> {
    // SAFETY: pair de descripteurs uniquement, aucune mémoire partagée.
    unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if m < 0 {
            return Err(io::Error::last_os_error());
        }
        let master: OwnedFd = OwnedFd::from_raw_fd(m);
        if libc::grantpt(m) != 0 || libc::unlockpt(m) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = [0u8; 1024];
        if libc::ptsname_r(m, buf.as_mut_ptr() as *mut libc::c_char, buf.len()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let cstr = std::ffi::CStr::from_ptr(buf.as_ptr() as *const libc::c_char);
        let path = cstr.to_string_lossy().into_owned();

        // TIOCGPTPEER: ouvre l'esclave de façon sûre à partir du maître.
        let mut s = libc::ioctl(
            m,
            libc::TIOCGPTPEER as libc::c_ulong,
            (libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) as libc::c_ulong,
        );
        if s < 0 {
            // repli: ouvrir le chemin du pts
            let cpath = CString::new(path.as_str())
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            s = libc::open(
                cpath.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            );
        }
        if s < 0 {
            return Err(io::Error::last_os_error());
        }
        let slave: OwnedFd = OwnedFd::from_raw_fd(s);

        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if libc::ioctl(m, libc::TIOCSWINSZ as libc::c_ulong, &ws) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((master, slave, path))
    }
}

pub fn set_pty_size(fd: RawFd, rows: u16, cols: u16) -> io::Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as libc::c_ulong, &ws) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// recherche d'un exécutable dans PATH
pub fn resolve(cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if p.components().count() > 1 || cmd.contains('/') {
        return if p.exists() {
            Some(p.to_path_buf())
        } else {
            None
        };
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(cmd);
        if cand.is_file() {
            if let Ok(md) = cand.metadata() {
                use std::os::unix::fs::PermissionsExt;
                if md.permissions().mode() & 0o111 != 0 {
                    return Some(cand);
                }
            }
        }
    }
    None
}

/// tableau de pointeurs termine par NULL (attendu par execve)
fn ptrs(v: &[CString]) -> Vec<*const libc::c_char> {
    let mut p: Vec<*const libc::c_char> = v.iter().map(|c| c.as_ptr()).collect();
    p.push(std::ptr::null());
    p
}

fn cstrings(v: &[String]) -> Vec<CString> {
    v.iter()
        .filter_map(|s| CString::new(s.as_bytes()).ok())
        .collect()
}

/// environnement final (vars courants + ajouts/retraits)
pub fn build_env(extra: &[(String, String)]) -> Vec<CString> {
    let mut map: BTreeMap<String, String> = std::env::vars().collect();
    for (k, v) in extra {
        if v.is_empty() {
            map.remove(k);
        } else {
            map.insert(k.clone(), v.clone());
        }
    }
    map.into_iter()
        .filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok())
        .collect()
}

fn reset_signals() {
    for sig in [
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGILL,
        libc::SIGTRAP,
        libc::SIGABRT,
        libc::SIGPIPE,
        libc::SIGALRM,
        libc::SIGTERM,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGCHLD,
        libc::SIGWINCH,
    ] {
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
        }
    }
}

/// lance `cmd` dans un nouveau pty; le processus enfant devient leader de
/// session avec le pty comme terminal contrôlant.
pub fn spawn_in_pty(
    cmd: &[String],
    env_extra: &[(String, String)],
    rows: u16,
    cols: u16,
) -> io::Result<Pty> {
    if cmd.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "commande vide"));
    }
    let program = resolve(&cmd[0]).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("introuvable: {}", cmd[0]))
    })?;
    let (master, slave, tty) = open_pty_pair(rows, cols)?;
    let argv = cstrings(
        &[program.to_string_lossy().into_owned()]
            .into_iter()
            .chain(cmd[1..].iter().cloned())
            .collect::<Vec<_>>(),
    );
    let envp = build_env(env_extra);

    // SAFETY: fork dans un processus mono-thread, l'enfant n'appelle que des
    // fonctions async-signal-safe puis execve().
    let pid = unsafe {
        let mraw = master.as_raw_fd();
        let sraw = slave.as_raw_fd();
        let pid = libc::fork();
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            libc::close(mraw);
            libc::setsid();
            libc::ioctl(sraw, libc::TIOCSCTTY as libc::c_ulong, 1 as libc::c_ulong);
            libc::dup2(sraw, 0);
            libc::dup2(sraw, 1);
            libc::dup2(sraw, 2);
            if sraw > 2 {
                libc::close(sraw);
            }
            reset_signals();
            libc::execve(argv[0].as_ptr(), ptrs(&argv).as_ptr(), ptrs(&envp).as_ptr());
            // exec a echoue: on le signale sur le terminal avant de mourir
            let msg = format!(
                "rscreen: exec {} echoue (errno {})\n",
                argv[0].to_string_lossy(),
                *libc::__errno_location()
            );
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::_exit(127);
        }
        pid
    };
    drop(slave); // important: sinon le maître ne verra jamais EOF
    Ok(Pty { master, pid, tty })
}

/// lance un programme en tâche de fond (nouvelle session, stdio vers /dev/null).
/// Aucun descripteur hérité (socket, terminal...) n'est conservé par l'enfant.
pub fn spawn_detached(exe: &Path, args: &[String]) -> io::Result<libc::pid_t> {
    let path = if exe.to_string_lossy().contains('/') {
        PathBuf::from(exe)
    } else {
        resolve(&exe.to_string_lossy()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, exe.to_string_lossy().into_owned())
        })?
    };
    let argv_s: Vec<String> = std::iter::once(path.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .collect();
    let argv_c = cstrings(&argv_s);
    let envp = build_env(&[]);
    let null = CString::new("/dev/null").unwrap();

    // SAFETY: idem spawn_in_pty: fork dans un processus mono-thread, puis
    // uniquement des appels async-signal-safe avant execve().
    Ok(unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            let _ = libc::setsid();
            let fd = libc::open(null.as_ptr(), libc::O_RDWR);
            if fd >= 0 {
                libc::dup2(fd, 0);
                libc::dup2(fd, 1);
                libc::dup2(fd, 2);
                if fd > 2 {
                    libc::close(fd);
                }
            }
            // on ferme tout ce qui a pu être hérité (sockets des autres sessions)
            let maxfd = libc::sysconf(libc::_SC_OPEN_MAX).clamp(256, 8192) as i32;
            for f in 3..maxfd {
                libc::close(f);
            }
            reset_signals();
            libc::execve(
                argv_c[0].as_ptr(),
                ptrs(&argv_c).as_ptr(),
                ptrs(&envp).as_ptr(),
            );
            libc::_exit(127);
        }
        pid
    })
}

pub fn shell_name() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// shell de connexion par défaut: bash/zsh lancé en mode login ("-bash")
pub fn default_shell_cmd() -> Vec<String> {
    let sh = shell_name();
    let base = Path::new(&sh)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sh".to_string());
    vec![sh.clone(), format!("-{base}")]
}

/// découpe une ligne de commande (avec ' simple et ")
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in line.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                '\\' => cur.push('\\'),
                c if c.is_whitespace() => {
                    if has {
                        out.push(std::mem::take(&mut cur));
                    }
                    has = false;
                }
                c => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoupage_commande() {
        assert_eq!(
            split_command("make -j8"),
            vec!["make".to_string(), "-j8".to_string()]
        );
        assert_eq!(
            split_command("sh -c 'echo bonjour le   monde'"),
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo bonjour le   monde".to_string()
            ]
        );
        assert_eq!(
            split_command("\"deux mots\""),
            vec!["deux mots".to_string()]
        );
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn resolution_de_shell() {
        assert!(resolve("/bin/sh").is_some());
        assert!(resolve("rscreen-inexistant").is_none());
        assert!(!default_shell_cmd().is_empty());
    }
}
