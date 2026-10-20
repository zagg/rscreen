//! Emplacements des sockets / fichiers d'état des sessions.

use std::env;
use std::fs;
use std::path::PathBuf;

use crate::err::{msg, Result};

/// Nom de session par défaut (celui de `rscreen` sans `-S`).
pub const DEFAULT_SESSION: &str = "default";

pub fn uid() -> u32 {
    // SAFETY: geteuid() ne fait rien d'autre que lire l'uid effectif.
    unsafe { libc::geteuid() }
}

/// Répertoire racine de tous les serveurs de session.
pub fn base_dir() -> PathBuf {
    if let Ok(d) = env::var("RSCREEN_DIR") {
        return PathBuf::from(d);
    }
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join(format!("rscreen-{}", uid()))
}

/// Un nom de session ne doit pas pouvoir s'échapper du répertoire.
pub fn sanitize(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str(DEFAULT_SESSION);
    }
    while out.starts_with('.') {
        out.remove(0);
    }
    if out.is_empty() {
        out.push_str(DEFAULT_SESSION);
    }
    out
}

pub fn session_dir(name: &str) -> PathBuf {
    base_dir().join(sanitize(name))
}

/// Répertoire de la session, créé si besoin.
pub fn ensure_session_dir(name: &str) -> Result<PathBuf> {
    let dir = session_dir(name);
    fs::create_dir_all(&dir)?;
    let meta = fs::metadata(&dir)?;
    if !meta.is_dir() {
        return Err(msg(format!("{} n'est pas un répertoire", dir.display())));
    }
    Ok(dir)
}

pub fn socket_path(name: &str) -> PathBuf {
    session_dir(name).join("sock")
}

pub fn state_path(name: &str) -> PathBuf {
    session_dir(name).join("state")
}

pub fn pid_path(name: &str) -> PathBuf {
    session_dir(name).join("pid")
}

/// Toutes les sessions enregistrées (répertoires présents dans base_dir).
pub fn session_names() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(base_dir()) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                if let Some(n) = e.file_name().to_str() {
                    v.push(n.to_string());
                }
            }
        }
    }
    v.sort();
    v
}

/// supprime le répertoire d'une session morte.
pub fn cleanup_session_dir(name: &str) {
    let _ = fs::remove_dir_all(session_dir(name));
}
