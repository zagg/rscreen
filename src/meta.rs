//! Métadonnées d'une session : le même format texte est utilisé pour le fichier
//! `state` sur le disque (lu par le sélecteur Ctrl+Tab) et pour trame `META`
//! envoyée au client.

use std::fs;

use crate::paths;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WinMeta {
    pub idx: u32,
    pub active: bool,
    /// nom demandé par l'utilisateur / commande lancée
    pub name: String,
    /// titre posé par l'application via OSC (prioritaire pour l'affichage)
    pub title: String,
    pub pid: i32,
    /// l'application de la fenetre est en mode curseur applicatif (DECCKM)
    pub app_cursor: bool,
    /// drapeaux du protocole clavier kitty demandes par l'application
    pub kitty: u32,
}

impl WinMeta {
    pub fn display(&self) -> &str {
        if self.title.is_empty() {
            &self.name
        } else {
            &self.title
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMeta {
    pub name: String,
    pub pid: i32,
    pub created: i64,
    pub rows: u16,
    pub cols: u16,
    pub attached: usize,
    pub wins: Vec<WinMeta>,
    pub shell: String,
}

impl SessionMeta {
    pub fn active_win(&self) -> Option<&WinMeta> {
        self.wins.iter().find(|w| w.active)
    }

    pub fn encode(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "name\t{}\npid\t{}\ncreated\t{}\nsize\t{}x{}\nattached\t{}\nshell\t{}\n",
            esc(&self.name),
            self.pid,
            self.created,
            self.rows,
            self.cols,
            self.attached,
            esc(&self.shell),
        ));
        for w in &self.wins {
            s.push_str(&format!(
                "win\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                w.idx,
                if w.active { 1 } else { 0 },
                w.pid,
                esc(&w.name),
                esc(&w.title),
                if w.app_cursor { 1 } else { 0 },
                w.kitty,
            ));
        }
        s
    }

    pub fn decode(s: &str) -> Option<SessionMeta> {
        let mut m = SessionMeta::default();
        for line in s.lines() {
            let mut it = line.split('\t');
            match it.next()? {
                "name" => m.name = unesc(it.next()?),
                "pid" => m.pid = it.next()?.parse().ok()?,
                "created" => m.created = it.next()?.parse().ok()?,
                "attached" => m.attached = it.next()?.parse().ok()?,
                "shell" => m.shell = unesc(it.next()?),
                "size" => {
                    let v = it.next()?;
                    let (r, c) = v.split_once('x')?;
                    m.rows = r.parse().ok()?;
                    m.cols = c.parse().ok()?;
                }
                "win" => m.wins.push(WinMeta {
                    idx: it.next()?.parse().ok()?,
                    active: it.next()? == "1",
                    pid: it.next()?.parse().ok()?,
                    name: unesc(it.next()?),
                    title: it.next().map(unesc).unwrap_or_default(),
                    app_cursor: it.next().map(|v| v == "1").unwrap_or(false),
                    kitty: it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                }),
                _ => {}
            }
        }
        if m.name.is_empty() {
            return None;
        }
        Some(m)
    }

    /// relit le fichier d'état d'une session (et le met à la journée si le
    /// processus ne vit plus).
    pub fn load(name: &str) -> Option<SessionMeta> {
        let s = fs::read_to_string(paths::state_path(name)).ok()?;
        let mut m = SessionMeta::decode(&s)?;
        if m.attached > 0 && !alive(m.pid) {
            m.attached = 0;
        }
        Some(m)
    }

    pub fn save(&self) -> std::io::Result<()> {
        fs::write(paths::state_path(&self.name), self.encode())
    }
}

/// le processus de session est-il vivant ?
pub fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(pid,0) ne fait que tester l'existence du processus.
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | '\r' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\\' => o.push_str("\\\\"),
            c => o.push(c),
        }
    }
    o
}

fn unesc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('n') => o.push('\n'),
            Some('t') => o.push('\t'),
            Some('\\') => o.push('\\'),
            Some(x) => o.push(x),
            None => {}
        }
    }
    o
}

/// liste toutes les sessions vivantes, la courante en premier si connue.
pub fn all_sessions() -> Vec<SessionMeta> {
    let mut out = Vec::new();
    for n in paths::session_names() {
        if let Some(mut m) = SessionMeta::load(&n) {
            if !alive(m.pid) && m.pid > 0 {
                // serveur mort : on range le bazar
                paths::cleanup_session_dir(&n);
                continue;
            }
            // le nom logique peut différer du nom du répertoire
            if m.name.is_empty() {
                m.name = n.clone();
            }
            out.push(m);
        }
    }
    out.sort_by_key(|m| (m.created, m.name.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let m = SessionMeta {
            name: "dev".into(),
            pid: 4242,
            created: 1700000000,
            rows: 48,
            cols: 160,
            attached: 2,
            shell: "/bin/bash".into(),
            wins: vec![
                WinMeta {
                    idx: 0,
                    active: true,
                    name: "bash".into(),
                    title: "root@serveur: /opt".into(),
                    pid: 1,
                    app_cursor: true,
                    kitty: 1,
                },
                WinMeta {
                    idx: 1,
                    active: false,
                    name: "ta\tbleau\n1".into(),
                    title: "back\\slash".into(),
                    pid: 2,
                    app_cursor: false,
                    kitty: 0,
                },
            ],
        };
        let back = SessionMeta::decode(&m.encode()).expect("décodage");
        assert_eq!(m, back);
        // display(): titre posé par l'application, sinon nom de la fenêtre
        assert_eq!(back.wins[0].display(), "root@serveur: /opt");
        assert_eq!(back.wins[1].display(), "back\\slash");
        assert_eq!(back.wins[1].name, "ta\tbleau\n1");
        assert_eq!(back.wins[0].kitty, 1);
        assert!(back.wins[0].app_cursor);
    }
}
