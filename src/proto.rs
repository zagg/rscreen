//! Trames échangées entre le client et le serveur de session.
//!
//! Une trame = `[u32 little-endian: longueur][u8: balise][payload]`.

use std::io::{Read, Write};

use crate::err::{msg, Result};

pub const MAX_PAYLOAD: usize = 8 << 20;

pub mod tag {
    // serveur -> client
    pub const DATA: u8 = 0x01;
    pub const META: u8 = 0x02;
    pub const MSG: u8 = 0x03;
    pub const BELL: u8 = 0x04;
    pub const KITTY: u8 = 0x05;
    pub const KITTY_QUERY: u8 = 0x06;
    pub const BYE: u8 = 0x07;
    pub const TITLE: u8 = 0x08;
    // client -> serveur
    pub const INPUT: u8 = 0x10;
    pub const RESIZE: u8 = 0x11;
    pub const SWITCH: u8 = 0x12;
    pub const NEXT: u8 = 0x13;
    pub const PREV: u8 = 0x14;
    pub const LAST: u8 = 0x15;
    pub const NEW: u8 = 0x16;
    pub const KILL: u8 = 0x17;
    pub const DETACH: u8 = 0x18;
    pub const KILL_SESSION: u8 = 0x19;
    pub const RENAME: u8 = 0x1a;
    pub const REPAINT: u8 = 0x1b;
    pub const SCROLL: u8 = 0x1c;
    pub const QUERY: u8 = 0x1d;
    pub const KITTY_FLAGS: u8 = 0x1e;
    pub const RENAME_SESSION: u8 = 0x1f;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollOp {
    Rel,
    Set,
    Top,
    Bottom,
}

/// message envoyé par le client au serveur
#[derive(Clone, Debug)]
pub enum CMsg {
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    Switch(u32),
    Next,
    Prev,
    Last,
    New {
        cmd: Vec<String>,
        title: String,
    },
    Kill(u32),
    Detach,
    KillSession,
    Rename {
        idx: u32,
        name: String,
    },
    /// rebaptiser la session elle-même (le serveur renomme son répertoire)
    RenameSession {
        name: String,
    },
    Repaint,
    Scroll {
        op: ScrollOp,
        amount: i64,
    },
    Query,
    /// le terminal réel du client supporte le protocole clavier kitty, avec ces
    /// drapeaux ; le serveur doit traduire les touches pour les applis legacy
    KittyFlags(u32),
}

impl CMsg {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            CMsg::Input(b) => put(out, tag::INPUT, b),
            CMsg::Resize { rows, cols } => {
                let mut p = Vec::with_capacity(4);
                p.extend_from_slice(&(*rows as u32).to_le_bytes());
                p.extend_from_slice(&(*cols as u32).to_le_bytes());
                put(out, tag::RESIZE, &p);
            }
            CMsg::Switch(i) => put(out, tag::SWITCH, &i.to_le_bytes()),
            CMsg::Next => put(out, tag::NEXT, b""),
            CMsg::Prev => put(out, tag::PREV, b""),
            CMsg::Last => put(out, tag::LAST, b""),
            CMsg::New { cmd, title } => {
                let mut p = Vec::new();
                for a in cmd {
                    p.extend_from_slice(a.as_bytes());
                    p.push(0);
                }
                p.push(0); // fin de liste d'arguments
                p.extend_from_slice(title.as_bytes());
                put(out, tag::NEW, &p);
            }
            CMsg::Kill(i) => put(out, tag::KILL, &i.to_le_bytes()),
            CMsg::Detach => put(out, tag::DETACH, b""),
            CMsg::KillSession => put(out, tag::KILL_SESSION, b""),
            CMsg::Rename { idx, name } => {
                let mut p = idx.to_le_bytes().to_vec();
                p.extend_from_slice(name.as_bytes());
                put(out, tag::RENAME, &p);
            }
            CMsg::RenameSession { name } => put(out, tag::RENAME_SESSION, name.as_bytes()),
            CMsg::Repaint => put(out, tag::REPAINT, b""),
            CMsg::Scroll { op, amount } => {
                let o: u8 = match op {
                    ScrollOp::Rel => 0,
                    ScrollOp::Set => 1,
                    ScrollOp::Top => 2,
                    ScrollOp::Bottom => 3,
                };
                let mut p = vec![o];
                p.extend_from_slice(&amount.to_le_bytes());
                put(out, tag::SCROLL, &p);
            }
            CMsg::Query => put(out, tag::QUERY, b""),
            CMsg::KittyFlags(f) => put(out, tag::KITTY_FLAGS, &f.to_le_bytes()),
        }
    }

    pub fn parse(t: u8, p: &[u8]) -> Result<Option<CMsg>> {
        let u32at = |o: usize| -> Result<u32> {
            let e: [u8; 4] = p
                .get(o..o + 4)
                .map(|s| [s[0], s[1], s[2], s[3]])
                .unwrap_or_default();
            Ok(u32::from_le_bytes(e))
        };
        Ok(match t {
            tag::INPUT => Some(CMsg::Input(p.to_vec())),
            tag::RESIZE => Some(CMsg::Resize {
                rows: u32at(0)? as u16,
                cols: u32at(4)? as u16,
            }),
            tag::SWITCH => Some(CMsg::Switch(u32at(0)?)),
            tag::NEXT => Some(CMsg::Next),
            tag::PREV => Some(CMsg::Prev),
            tag::LAST => Some(CMsg::Last),
            tag::NEW => {
                let mut cmd: Vec<String> = Vec::new();
                let mut rest = p;
                loop {
                    match rest.iter().position(|b| *b == 0) {
                        Some(0) => {
                            rest = &rest[1..];
                            break;
                        }
                        Some(z) => {
                            cmd.push(String::from_utf8_lossy(&rest[..z]).into_owned());
                            rest = &rest[z + 1..];
                        }
                        None => {
                            rest = &[];
                            break;
                        }
                    }
                }
                Some(CMsg::New {
                    cmd,
                    title: String::from_utf8_lossy(rest).into_owned(),
                })
            }
            tag::KILL => Some(CMsg::Kill(u32at(0)?)),
            tag::DETACH => Some(CMsg::Detach),
            tag::KILL_SESSION => Some(CMsg::KillSession),
            tag::RENAME => Some(CMsg::Rename {
                idx: u32at(0)?,
                name: String::from_utf8_lossy(p.get(4..).unwrap_or(&[])).into_owned(),
            }),
            tag::RENAME_SESSION => Some(CMsg::RenameSession {
                name: String::from_utf8_lossy(p).into_owned(),
            }),
            tag::REPAINT => Some(CMsg::Repaint),
            tag::SCROLL => {
                let o = *p.first().unwrap_or(&0);
                let e: [u8; 8] = p
                    .get(1..9)
                    .map(|s| [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
                    .unwrap_or_default();
                Some(CMsg::Scroll {
                    op: match o {
                        1 => ScrollOp::Set,
                        2 => ScrollOp::Top,
                        3 => ScrollOp::Bottom,
                        _ => ScrollOp::Rel,
                    },
                    amount: i64::from_le_bytes(e),
                })
            }
            tag::QUERY => Some(CMsg::Query),
            tag::KITTY_FLAGS => Some(CMsg::KittyFlags(u32at(0)?)),
            _ => return Err(msg(format!("balise client inconnue: {t}"))),
        })
    }
}

/// message envoyé par le serveur au client
#[derive(Clone, Debug)]
pub enum SMsg {
    /// octets à écrire tels quels sur le terminal réel du client
    Data(Vec<u8>),
    /// état de la session (format de meta.rs)
    Meta(String),
    /// message court à afficher dans la barre de statut
    Msg(String),
    Bell,
    /// drapeaux kitty demandés par l'application de la fenêtre visualisée
    Kitty(u32),
    /// l'application vient de demander les capacités clavier du terminal
    KittyQuery,
    /// titre de la fenêtre (OSC) — pour la barre de statut du client
    Title {
        idx: u32,
        title: String,
    },
    Bye,
}

impl SMsg {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            SMsg::Data(b) => put(out, tag::DATA, b),
            SMsg::Meta(s) => put(out, tag::META, s.as_bytes()),
            SMsg::Msg(s) => put(out, tag::MSG, s.as_bytes()),
            SMsg::Bell => put(out, tag::BELL, b""),
            SMsg::Kitty(f) => put(out, tag::KITTY, &f.to_le_bytes()),
            SMsg::KittyQuery => put(out, tag::KITTY_QUERY, b""),
            SMsg::Title { idx, title } => {
                let mut p = idx.to_le_bytes().to_vec();
                p.extend_from_slice(title.as_bytes());
                put(out, tag::TITLE, &p);
            }
            SMsg::Bye => put(out, tag::BYE, b""),
        }
    }

    pub fn parse(t: u8, p: &[u8]) -> Result<Option<SMsg>> {
        Ok(match t {
            tag::DATA => Some(SMsg::Data(p.to_vec())),
            tag::META => Some(SMsg::Meta(String::from_utf8_lossy(p).into_owned())),
            tag::MSG => Some(SMsg::Msg(String::from_utf8_lossy(p).into_owned())),
            tag::BELL => Some(SMsg::Bell),
            tag::KITTY => {
                let e: [u8; 4] = p
                    .get(0..4)
                    .map(|s| [s[0], s[1], s[2], s[3]])
                    .unwrap_or_default();
                Some(SMsg::Kitty(u32::from_le_bytes(e)))
            }
            tag::KITTY_QUERY => Some(SMsg::KittyQuery),
            tag::TITLE => {
                let e: [u8; 4] = p
                    .get(0..4)
                    .map(|s| [s[0], s[1], s[2], s[3]])
                    .unwrap_or_default();
                Some(SMsg::Title {
                    idx: u32::from_le_bytes(e),
                    title: String::from_utf8_lossy(p.get(4..).unwrap_or(&[])).into_owned(),
                })
            }
            tag::BYE => Some(SMsg::Bye),
            _ => return Err(msg(format!("balise serveur inconnue: {t}"))),
        })
    }
}

pub fn put(out: &mut Vec<u8>, t: u8, payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.push(t);
    out.extend_from_slice(payload);
}

#[derive(Debug)]
pub enum Frame {
    Need,
    Got(u8, Vec<u8>),
    Bad,
}

/// extrait une trame complète de `buf` (et la consomme)
pub fn take(buf: &mut Vec<u8>) -> Frame {
    if buf.len() < 5 {
        return Frame::Need;
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_PAYLOAD {
        return Frame::Bad;
    }
    if buf.len() < 5 + len {
        return Frame::Need;
    }
    let t = buf[4];
    let payload = buf.drain(5..5 + len).as_slice().to_vec();
    buf.drain(0..5);
    Frame::Got(t, payload)
}

/// lit une fois dans `r`, empile dans `buf` puis dépile toutes les trames
pub fn read_frames<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<Vec<(u8, Vec<u8>)>> {
    let mut tmp = [0u8; 64 * 1024];
    let n = r.read(&mut tmp)?;
    if n == 0 {
        return Ok(Vec::new());
    }
    buf.extend_from_slice(&tmp[..n]);
    let mut out = Vec::new();
    loop {
        match take(buf) {
            Frame::Need => break,
            Frame::Bad => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "trame invalide",
                ))
            }
            Frame::Got(t, p) => out.push((t, p)),
        }
    }
    Ok(out)
}

pub fn write_all_frames<W: Write>(w: &mut W, buf: &mut Vec<u8>) -> std::io::Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let r = w.write_all(buf);
    buf.clear();
    r?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trames_client() {
        for m in [
            CMsg::Input(b"\x03".to_vec()),
            CMsg::Resize {
                rows: 40,
                cols: 120,
            },
            CMsg::Switch(7),
            CMsg::New {
                cmd: vec!["make".into(), "-j8".into()],
                title: "build".into(),
            },
            CMsg::Scroll {
                op: ScrollOp::Rel,
                amount: -12,
            },
            CMsg::Rename {
                idx: 3,
                name: "un nom".into(),
            },
            CMsg::RenameSession {
                name: "dev-2".into(),
            },
            CMsg::KittyFlags(1),
        ] {
            let mut buf = Vec::new();
            m.write_to(&mut buf);
            let mut b = buf.clone();
            match take(&mut b) {
                Frame::Got(t, p) => {
                    let got = CMsg::parse(t, &p).unwrap().unwrap();
                    let mut buf2 = Vec::new();
                    got.write_to(&mut buf2);
                    assert_eq!(buf, buf2);
                }
                other => panic!("trame invalide: {other:?}"),
            }
        }
    }

    #[test]
    fn trame_incomplete() {
        let mut buf = vec![9, 0, 0, 0, tag::INPUT, b'a'];
        assert!(matches!(take(&mut buf), Frame::Need));
        buf.extend_from_slice(b"bcdefghi");
        assert!(matches!(take(&mut buf), Frame::Got(tag::INPUT, _)));
    }
}
