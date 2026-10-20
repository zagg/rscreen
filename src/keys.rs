//! Décodage des touches lues sur stdin, et ré-encodage « legacy » pour les
//! applications placées dans les fenêtres.
//!
//! Le but est de comprendre aussi bien un terminal ancien (octets de control)
//! qu'un terminal moderne ayant activé le « kitty keyboard protocol »
//! (`CSI code ; mods u`), afin de pouvoir distinguer `Tab` de `Ctrl+Tab`.

pub const SHIFT: u32 = 1;
pub const ALT: u32 = 2;
pub const CTRL: u32 = 4;
pub const SUPER: u32 = 8;
pub const HYPER: u32 = 16;
pub const META: u32 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Tab,
    Enter,
    Backspace,
    Delete,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    /// touche de fonction, `num` = numéro (1..=12)
    Func,
    /// rapport souris: on ne touche pas, on le laisse passer
    Mouse,
    /// reponse du terminal a une requete (capacites, position du curseur...)
    /// -> a ne surtout pas transmettre a l'application
    Resp,
    /// séquence non reconnue
    Other,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Key {
    pub kind: Kind,
    pub mods: u32,
    pub ch: Option<char>,
    pub num: u32,
    pub raw: Vec<u8>,
    /// 0 = forme legacy (octets de controle), 1 = kitty `CSI code;mods u`,
    /// 2 = xterm modifyOtherKeys `CSI 27;mods;code ~`
    pub enc: u8,
}

impl Key {
    pub fn new(kind: Kind, mods: u32) -> Key {
        Key {
            kind,
            mods,
            ch: None,
            num: 0,
            raw: Vec::new(),
            enc: 0,
        }
    }

    pub fn text(ch: char, mods: u32) -> Key {
        Key {
            kind: Kind::Text,
            mods,
            ch: Some(ch),
            num: 0,
            raw: Vec::new(),
            enc: 0,
        }
    }

    /// la touche est-elle un ctrl+lettre/symbole, sans autre modificateur ?
    pub fn ctrl_only(&self) -> Option<char> {
        if self.kind == Kind::Text && self.mods == CTRL {
            self.ch
        } else {
            None
        }
    }

    /// caractère simple, sans modificateur
    pub fn plain(&self) -> Option<char> {
        if self.kind == Kind::Text && self.mods == 0 {
            self.ch
        } else {
            None
        }
    }

    pub fn digit(&self) -> Option<u32> {
        self.plain().and_then(|c| c.to_digit(10))
    }

    pub fn is_tab(&self) -> bool {
        self.kind == Kind::Tab && self.mods == 0
    }

    /// Ctrl+Tab (ce que beaucoup de terminaux ne savent pas envoyer)
    pub fn is_tab_ctrl(&self) -> bool {
        self.kind == Kind::Tab && self.mods == CTRL
    }

    pub fn is_enter_ctrl(&self) -> bool {
        self.kind == Kind::Enter && self.mods == CTRL
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Dec {
    /// il faut plus d'octets
    Need,
    /// touche décodée, N octets consommés
    Done(Key, usize),
    /// séquence invalide, N octets à jeter
    Invalid(usize),
}

/// décode une touche depuis le début de `buf`
pub fn decode(buf: &[u8]) -> Dec {
    if buf.is_empty() {
        return Dec::Need;
    }
    let b0 = buf[0];
    if b0 != 0x1b {
        return decode_plain(buf);
    }
    if buf.len() == 1 {
        return Dec::Need;
    }
    match buf[1] {
        b'[' => decode_csi(buf),
        b'O' => {
            if buf.len() < 3 {
                return Dec::Need;
            }
            let c = buf[2];
            let kind = match c {
                b'A' => Some(Kind::Up),
                b'B' => Some(Kind::Down),
                b'C' => Some(Kind::Right),
                b'D' => Some(Kind::Left),
                b'P' => Some(Kind::Func),
                b'Q' => Some(Kind::Func),
                b'R' => Some(Kind::Func),
                b'S' => Some(Kind::Func),
                _ => None,
            };
            match kind {
                Some(k) => {
                    let mut key = Key::new(k, 0);
                    if k == Kind::Func {
                        key.num = (c - b'P') as u32 + 1;
                    }
                    key.raw = buf[..3].to_vec();
                    Dec::Done(key, 3)
                }
                None => Dec::Invalid(3),
            }
        }
        0x1b => {
            // ESC ESC: deux touches Escape de suite
            Dec::Done(Key::new(Kind::Escape, 0), 1)
        }
        0x7f => Dec::Done(Key::new(Kind::Backspace, ALT), 2),
        b if (0x20..0x7f).contains(&b) => {
            let ch = (b as char).to_ascii_lowercase();
            Dec::Done(Key::text(ch, ALT), 2)
        }
        b if b >= 0x80 => {
            // suite UTF-8 après un ESC
            match decode_plain(&buf[1..]) {
                Dec::Done(mut k, n) => {
                    k.mods |= ALT;
                    k.raw = buf[..n + 1].to_vec();
                    Dec::Done(k, n + 1)
                }
                other => other,
            }
        }
        _ => Dec::Invalid(2),
    }
}

fn decode_plain(buf: &[u8]) -> Dec {
    let b = buf[0];
    if b >= 0x80 {
        // caractère UTF-8
        let len = utf8_len(b);
        if buf.len() < len {
            return Dec::Need;
        }
        match std::str::from_utf8(&buf[..len]) {
            Ok(s) => {
                let ch = s.chars().next().unwrap_or('\u{0}');
                let mut k = Key::text(ch, 0);
                k.raw = buf[..len].to_vec();
                Dec::Done(k, len)
            }
            Err(_) => Dec::Invalid(1),
        }
    } else {
        Dec::Done(byte_key(b), 1)
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x80..=0xbf => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// touche issue d'un octet simple (terminal traditionnel)
pub fn byte_key(b: u8) -> Key {
    let mut k = byte_key_inner(b);
    k.raw = vec![b];
    k
}

fn byte_key_inner(b: u8) -> Key {
    match b {
        0x00 => Key::text(' ', CTRL),
        0x08 => Key::text('h', CTRL),
        0x09 => Key::new(Kind::Tab, 0),
        0x0a => Key::text('j', CTRL),
        0x0b => Key::text('k', CTRL),
        0x0c => Key::text('l', CTRL),
        0x0d => Key::new(Kind::Enter, 0),
        0x1b => Key::new(Kind::Escape, 0),
        0x1c => Key::text('\\', CTRL),
        0x1d => Key::text(']', CTRL),
        0x1e => Key::text('^', CTRL),
        0x1f => Key::text('_', CTRL),
        0x7f => Key::new(Kind::Backspace, 0),
        c if (0x01..=0x1a).contains(&c) => Key::text((c + 96) as char, CTRL),
        c if (0x20..0x7f).contains(&c) => Key::text(c as char, 0),
        c => {
            let mut k = Key::new(Kind::Other, 0);
            k.raw = vec![c];
            k
        }
    }
}

/// décompose `\x1b[ ... X`
struct Csi {
    nums: Vec<u16>,
    inter: Vec<u8>,
    privs: Vec<u8>,
    final_c: char,
    used: usize,
}

fn csi_split(buf: &[u8]) -> Option<Csi> {
    let mut i = 2;
    let mut priv_marks = Vec::new();
    let mut nums: Vec<u16> = Vec::new();
    let mut cur: Option<u32> = None;
    let mut intermediates = Vec::new();
    while i < buf.len() {
        let c = buf[i];
        match c {
            b'0'..=b'9' => {
                cur = Some(cur.unwrap_or(0) * 10 + u32::from(c - b'0'));
            }
            b';' => {
                nums.push(cur.take().map(|v| v.min(65535) as u16).unwrap_or(0));
                priv_marks.push(0);
            }
            b'<' | b'=' | b'>' | b'?' => {
                priv_marks.push(c);
            }
            0x20..=0x2f => {
                intermediates.push(c);
            }
            0x40..=0x7e => {
                if let Some(v) = cur.take() {
                    nums.push(v.min(65535) as u16);
                }
                return Some(Csi {
                    nums,
                    inter: intermediates,
                    privs: priv_marks,
                    final_c: c as char,
                    used: i + 1,
                });
            }
            _ => {
                return Some(Csi {
                    nums,
                    inter: intermediates,
                    privs: priv_marks,
                    final_c: 0 as char,
                    used: i,
                })
            }
        }
        i += 1;
    }
    None
}

fn decode_csi(buf: &[u8]) -> Dec {
    let Csi {
        nums,
        inter: _inter,
        privs,
        final_c,
        used,
    } = match csi_split(buf) {
        Some(x) => x,
        None => return Dec::Need,
    };
    let raw = buf[..used].to_vec();
    if used < 3 {
        return Dec::Invalid(used);
    }
    let p = |i: usize| -> u32 { nums.get(i).copied().unwrap_or(0) as u32 };
    // kitty: mods = paramètre - 1 ; xterm modifyOtherKeys idem
    let mods_of = |i: usize| -> u32 { p(i).saturating_sub(1) };

    let key = match final_c {
        'u' => {
            // reponse a la requete de capacites: CSI ? <flags> u  (ou CSI ? u)
            if privs.contains(&b'?') {
                let mut k = Key::new(Kind::Resp, 0);
                k.raw = raw.clone();
                k
            } else {
                let cp = p(0);
                let m = mods_of(1);
                let mut k = key_from_cp(cp, m, &raw);
                k.enc = 1;
                k
            }
        }
        '~' => {
            // modifyOtherKeys: CSI 27 ; mods ; codepoint ~
            if p(0) == 27 && nums.len() >= 3 {
                let m = mods_of(1);
                let cp = p(2);
                let mut k = key_from_cp(cp, m, &raw);
                k.enc = 2;
                k
            } else {
                let m = if nums.len() >= 2 { mods_of(1) } else { 0 };
                match p(0) {
                    1 | 7 => Key::new(Kind::Home, m),
                    2 => Key::new(Kind::Insert, m),
                    3 => Key::new(Kind::Delete, m),
                    4 | 8 => Key::new(Kind::End, m),
                    5 => Key::new(Kind::PageUp, m),
                    6 => Key::new(Kind::PageDown, m),
                    11..=15 => mk_func(p(0) - 10, m, &raw),
                    17..=21 => mk_func(p(0) - 11, m, &raw),
                    23..=26 => mk_func(p(0) - 12, m, &raw),
                    200 | 201 => {
                        let mut k = Key::new(Kind::Other, 0);
                        k.raw = raw.clone();
                        k
                    }
                    _ => {
                        let mut k = Key::new(Kind::Other, m);
                        k.raw = raw.clone();
                        k
                    }
                }
            }
        }
        'A' | 'B' | 'C' | 'D' | 'H' | 'F' | 'P' | '@' => {
            let m = if nums.len() >= 2 { mods_of(1) } else { 0 };
            match final_c {
                'A' => Key::new(Kind::Up, m),
                'B' => Key::new(Kind::Down, m),
                'C' => Key::new(Kind::Right, m),
                'D' => Key::new(Kind::Left, m),
                'H' => Key::new(Kind::Home, m),
                'F' => Key::new(Kind::End, m),
                'P' => Key::new(Kind::Delete, m),
                _ => Key::new(Kind::Insert, m),
            }
        }
        'Z' => Key::new(Kind::Tab, SHIFT),
        'I' => Key::new(Kind::Tab, SHIFT),
        'M' | 'm' => {
            let mut k = Key::new(Kind::Mouse, 0);
            k.raw = raw.clone();
            k
        }
        _ => {
            let mut k = Key::new(Kind::Other, 0);
            k.raw = raw.clone();
            k
        }
    };
    let mut key = key;
    if key.raw.is_empty() {
        key.raw = raw;
    }
    if final_c == 0 as char {
        Dec::Invalid(used)
    } else {
        Dec::Done(key, used)
    }
}

fn mk_func(n: u32, m: u32, raw: &[u8]) -> Key {
    let mut k = Key::new(Kind::Func, m);
    k.num = n;
    k.raw = raw.to_vec();
    k
}

/// transforme un point de code + modificateurs en Key
fn key_from_cp(cp: u32, mods: u32, raw: &[u8]) -> Key {
    match cp {
        9 => Key::new(Kind::Tab, mods),
        13 | 57399 => Key::new(Kind::Enter, mods),
        27 | 57376 => Key::new(Kind::Escape, mods & !CTRL),
        8 | 127 | 57391 => Key::new(Kind::Backspace, mods),
        32 => Key::text(' ', mods),
        // touches nommées (uniquement si le drapeau "named keys" est actif)
        57350 => Key::new(Kind::Up, mods),
        57351 => Key::new(Kind::Down, mods),
        57352 => Key::new(Kind::Right, mods),
        57353 => Key::new(Kind::Left, mods),
        57356 => Key::new(Kind::Home, mods),
        57357 => Key::new(Kind::End, mods),
        57358 => Key::new(Kind::PageUp, mods),
        57359 => Key::new(Kind::PageDown, mods),
        57354 => Key::new(Kind::Insert, mods),
        57355 => Key::new(Kind::Delete, mods),
        c if (57312..=57323).contains(&c) => mk_func(c - 57311, mods, raw),
        c if (33..=126).contains(&c) => Key::text(c as u8 as char, mods),
        c if c > 127 => match char::from_u32(c) {
            Some(ch) => Key::text(ch, mods),
            None => {
                let mut k = Key::new(Kind::Other, mods);
                k.raw = raw.to_vec();
                k
            }
        },
        c if c < 32 => {
            // codepoint de control: on le normalise en ctrl+lettre
            if c == 0 {
                Key::text(' ', mods | CTRL)
            } else {
                Key::text((c + 96) as u8 as char, mods | CTRL)
            }
        }
        _ => {
            let mut k = Key::new(Kind::Other, mods);
            k.raw = raw.to_vec();
            k
        }
    }
}

/// ré-encode une touche dans le format habituel d'un terminal traditionnel.
///
/// `app_cursor` = l'application attend des flèches en `ESC O A` (DECCKM),
/// `kitty` = l'application comprend le protocole kitty, on renvoie alors les
/// octets reçus tels quels.
pub fn legacy_bytes(k: &Key, app_cursor: bool, kitty: bool) -> Vec<u8> {
    if kitty && !k.raw.is_empty() {
        return k.raw.clone();
    }
    let m = k.mods;
    let extra = |out: &mut Vec<u8>, base: &str| {
        // CSI 1 ; mods X
        if m != 0 {
            out.extend_from_slice(format!("\x1b[1;{}{}", m + 1, base).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{base}").as_bytes());
        }
    };
    let mut out: Vec<u8> = Vec::new();
    match k.kind {
        Kind::Text => {
            let ch = k.ch.unwrap_or(' ');
            if m & CTRL != 0 {
                if let Some(c) = ctrl_code(ch) {
                    if m & ALT != 0 {
                        out.push(0x1b);
                    }
                    out.push(c);
                    return out;
                }
                // pas d'équivalent legacy: on envoie au pire le caractère nu
                if m & ALT != 0 {
                    out.push(0x1b);
                }
            } else if m & ALT != 0 {
                out.push(0x1b);
            }
            let mut b = [0u8; 4];
            let s = if m & SHIFT != 0 && ch.is_ascii_alphabetic() {
                ch.to_ascii_uppercase()
            } else {
                ch
            };
            out.extend_from_slice(s.encode_utf8(&mut b).as_bytes());
            out
        }
        Kind::Tab => {
            if m & SHIFT != 0 {
                out.extend_from_slice(b"\x1b[Z");
            } else if m == 0 {
                out.push(b'\t');
            }
            out
        }
        Kind::Enter => {
            if m == 0 || m & SHIFT != 0 {
                out.push(b'\r');
            }
            out
        }
        Kind::Backspace => {
            out.push(if m & CTRL != 0 { 0x08 } else { 0x7f });
            out
        }
        Kind::Delete => {
            extra(&mut out, "3~");
            out
        }
        Kind::Insert => {
            extra(&mut out, "2~");
            out
        }
        Kind::Escape => {
            out.push(0x1b);
            out
        }
        Kind::Up => {
            if app_cursor && m == 0 {
                out.extend_from_slice(b"\x1bOA");
            } else {
                extra(&mut out, "A");
            }
            out
        }
        Kind::Down => {
            if app_cursor && m == 0 {
                out.extend_from_slice(b"\x1bOB");
            } else {
                extra(&mut out, "B");
            }
            out
        }
        Kind::Right => {
            if app_cursor && m == 0 {
                out.extend_from_slice(b"\x1bOC");
            } else {
                extra(&mut out, "C");
            }
            out
        }
        Kind::Left => {
            if app_cursor && m == 0 {
                out.extend_from_slice(b"\x1bOD");
            } else {
                extra(&mut out, "D");
            }
            out
        }
        Kind::Home => {
            extra(&mut out, "H");
            out
        }
        Kind::End => {
            extra(&mut out, "F");
            out
        }
        Kind::PageUp => {
            extra(&mut out, "5~");
            out
        }
        Kind::PageDown => {
            extra(&mut out, "6~");
            out
        }
        Kind::Func => {
            let n = k.num as usize;
            if n <= 4 {
                out.extend_from_slice(&[0x1b, b'O', b'P' + (n as u8 - 1)]);
            } else {
                // xterm: F5=15~ F6=17~ ... F12=24~ F13=25~ ...
                const XK: [u32; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
                let code = if n <= 12 { XK[n - 5] } else { n as u32 + 12 };
                out.extend_from_slice(format!("\x1b[{code}~").as_bytes());
            }
            if m != 0 {
                out.clear();
                out.extend_from_slice(format!("\x1b[1;{}P", m + 1).as_bytes());
            }
            out
        }
        Kind::Mouse | Kind::Other | Kind::Resp => k.raw.clone(),
    }
}

/// caractère de control legacy correspondant à ctrl+<touche>
fn ctrl_code(ch: char) -> Option<u8> {
    let c = ch.to_ascii_lowercase();
    let v = match c {
        ' ' => 0x00,
        'a'..='z' => (c as u8) - 96,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' | '-' | '/' => 0x1f,
        '0' => 0x00,
        '2' => 0x00,
        '3' => 0x1b,
        '4' => 0x1c,
        '5' => 0x1d,
        '6' => 0x1e,
        '7' => 0x1f,
        '8' => 0x7f,
        _ => return None,
    };
    Some(v)
}

/// nom lisible d'un raccourci, pour l'aide
pub fn label(k: &Key) -> String {
    let mut s = String::new();
    if k.mods & CTRL != 0 {
        s.push_str("Ctrl+");
    }
    if k.mods & ALT != 0 {
        s.push_str("Alt+");
    }
    if k.mods & SHIFT != 0 {
        s.push_str("Maj+");
    }
    if k.mods & SUPER != 0 {
        s.push_str("Super+");
    }
    match k.kind {
        Kind::Text => s.push(k.ch.unwrap_or('?')),
        other => s.push_str(match other {
            Kind::Tab => "Tab",
            Kind::Enter => "Entrée",
            Kind::Backspace => "RetArr",
            Kind::Delete => "Suppr",
            Kind::Escape => "Échap",
            Kind::Up => "↑",
            Kind::Down => "↓",
            Kind::Left => "←",
            Kind::Right => "→",
            Kind::Home => "Début",
            Kind::End => "Fin",
            Kind::PageUp => "PgPr",
            Kind::PageDown => "PgSu",
            Kind::Insert => "Inser",
            Kind::Func => "F",
            _ => "?",
        }),
    }
    if k.kind == Kind::Func {
        s.push_str(&k.num.to_string());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(buf: &[u8]) -> Key {
        match decode(buf) {
            Dec::Done(k, n) => {
                assert_eq!(n, buf.len(), "la séquence entière doit être consommée");
                k
            }
            other => panic!("décodage raté pour {buf:?}: {other:?}"),
        }
    }

    #[test]
    fn legacy_simple() {
        let k = one(b"\t");
        assert_eq!((k.kind, k.mods), (Kind::Tab, 0));
        assert!(k.is_tab());

        let k = one(b"\x01");
        assert_eq!((k.kind, k.mods, k.ch), (Kind::Text, CTRL, Some('a')));
        assert_eq!(k.ctrl_only(), Some('a'));

        let k = one(b"\x1b[Z");
        assert_eq!((k.kind, k.mods), (Kind::Tab, SHIFT));

        let k = one(b"\x1bOA");
        assert_eq!((k.kind, k.mods), (Kind::Up, 0));

        let k = one(b"\x1b[5~");
        assert_eq!((k.kind, k.mods), (Kind::PageUp, 0));

        let k = one(b"\x1b[1;5C");
        assert_eq!((k.kind, k.mods), (Kind::Right, CTRL));
    }

    #[test]
    fn ctrl_tab_distingue_de_tab() {
        // kitty
        let k = one(b"\x1b[9;5u");
        assert_eq!((k.kind, k.mods, k.enc), (Kind::Tab, CTRL, 1));
        assert!(k.is_tab_ctrl());
        // xterm modifyOtherKeys
        let k = one(b"\x1b[27;5;9~");
        assert_eq!((k.kind, k.mods, k.enc), (Kind::Tab, CTRL, 2));
        // et le Ctrl+Tab ne doit jamais être transmis tel quel
        assert!(legacy_bytes(&k, false, false).is_empty());
    }

    #[test]
    fn reencodage_legacy() {
        let k = one(b"\x1b[99;5u"); // Ctrl+C façon kitty
        assert_eq!(k.ctrl_only(), Some('c'));
        assert_eq!(legacy_bytes(&k, false, false), vec![3u8]);

        let up = one(b"\x1b[A");
        assert_eq!(legacy_bytes(&up, false, false), b"\x1b[A".to_vec());
        assert_eq!(legacy_bytes(&up, true, false), b"\x1bOA".to_vec());

        let f = one(b"\x1b[15~"); // F5
        assert_eq!((f.kind, f.num), (Kind::Func, 5));
        assert_eq!(legacy_bytes(&f, false, false), b"\x1b[15~".to_vec());
    }

    #[test]
    fn reponses_terminal_jamais_transmises_comme_touche() {
        let k = one(b"\x1b[?1u"); // réponse à la requête kitty
        assert_eq!(k.kind, Kind::Resp);
        // attention: interprété comme une touche, ce serait un faux Ctrl+A
        assert_ne!(k.ctrl_only(), Some('a'));
    }

    #[test]
    fn utf8_et_incomplet() {
        let k = one("é".as_bytes());
        assert_eq!((k.kind, k.ch), (Kind::Text, Some('é')));
        assert_eq!(decode(b"\x1b["), Dec::Need);
        assert_eq!(decode(b"\xc3"), Dec::Need);
        assert_eq!(decode(b""), Dec::Need);
    }
}
