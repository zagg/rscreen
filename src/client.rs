//! Client rscreen : terminal local, barre de statut fenêtrée, sélecteur de
//! sessions (duo `Ctrl+b` puis `a`), bascule en arrière-plan.
//!
//! `Tab` et `Maj+Tab` ne sont jamais capturées : elles partent telles quelles
//! vers l'application tenue par la fenêtre (complétion du shell, TAB d'un
//! éditeur, navigations Windows...). Le changement de fenêtre se fait par
//! `Ctrl+a n` / `Ctrl+a p`, les chiffres `0..9`, ou la fenêtre modale.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

use crate::err::{msg, Result};
use crate::keys::{self, Dec, Key, Kind, CTRL, SHIFT};
use crate::meta::{self, SessionMeta};
use crate::paths;
use crate::proto::{self, CMsg, SMsg, ScrollOp};
use crate::pty;
use crate::term;

pub struct Config {
    pub session: String,
    pub cmd: Vec<String>,
    pub title: String,
    pub create: bool,
    pub scrollback: usize,
    pub first_win: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Live,
    Prefix,
    /// `Ctrl+b` pressé, on attend la deuxième touche du duo
    Chord,
    History,
    Picker,
    Help,
    Prompt,
    Confirm,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PromptKind {
    NewWindow,
    Rename,
    NewSession,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ItemKind {
    Session,
    Window,
    Background,
    NewSession,
    ReturnShell,
}

#[derive(Clone, Debug)]
struct Item {
    kind: ItemKind,
    session: String,
    win: Option<u32>,
    label: String,
    info: String,
    cur: bool,
}

#[derive(Default)]
struct Picker {
    items: Vec<Item>,
    sel: usize,
    top: usize,
    expanded: Vec<String>,
    first_open: bool,
}

/// couleurs 256: barre de statut
const BAR: &str = "\x1b[38;5;252m\x1b[48;5;236m";
const BAR_HI: &str = "\x1b[1m\x1b[38;5;16m\x1b[48;5;252m";
const BAR_TITLE: &str = "\x1b[1m\x1b[38;5;231m\x1b[48;5;31m";
const BAR_MSG: &str = "\x1b[38;5;16m\x1b[48;5;226m";
const ROW_CUR: &str = "\x1b[38;5;231m\x1b[48;5;17m";
const ROW: &str = "\x1b[38;5;250m\x1b[48;5;24m";
const ROW_SEL: &str = "\x1b[1m\x1b[38;5;16m\x1b[48;5;226m";
const DIM: &str = "\x1b[38;5;245m\x1b[48;5;16m";

pub struct Client {
    cfg: Config,
    session: String,
    sock: Option<UnixStream>,
    writer: Option<UnixStream>,
    rbuf: Vec<u8>,
    keybuf: Vec<u8>,
    pend_in: Vec<u8>,
    out: Vec<u8>,
    meta: SessionMeta,
    rows: u16,
    cols: u16,
    pty_rows: u16,
    pty_cols: u16,
    mode: Mode,
    /// duo « Ctrl+b puis a » en cours : octets du préfixe à restituer s'il rate,
    /// mode d'où l'on vient, et échéance du duo
    chord_pending: Vec<u8>,
    chord_from: Mode,
    chord_at: Option<Instant>,
    msg: Option<(String, Instant)>,
    picker: Picker,
    prompt_buf: String,
    prompt_pos: usize,
    prompt_label: String,
    prompt_kind: PromptKind,
    prompt_target: u32,
    confirm_session: bool,
    help_top: usize,
    kitty_term: u32,
    kitty_app: u32,
    app_cursor: bool,
    pending: Option<String>,
    status_cache: Vec<u8>,
    dead: bool,
}

const HELP: &[&str] = &[
    "r s c r e e n   —   aide / raccourcis",
    "",
    "Ctrl+b puis a           sélecteur de sessions (fenêtre modale)",
    "Ctrl+Entrée             détacher : la session reste en arrière-plan",
    "Ctrl+a                  préfixe (identique à gnu-screen)",
    "",
    "Fenêtre modale du sélecteur — la liste contient :",
    "  • les sessions actives (et, dépliées, leurs fenêtres)",
    "  • « + nouvelle session… »         en créer une",
    "  • « ← retour au shell »           revenir à la session courante",
    "  • « ⤳ rester en arrière-plan »    détacher ce terminal",
    "",
    "Dans cette fenêtre modale :",
    "  ↑ ↓  (ou j k)               monter / descendre d'une ligne",
    "  PgPr PgSu Début Fin         d'une page, en haut, en bas",
    "  Entrée                      VALIDER la ligne surlignée",
    "  ← →                         replier / déplier les fenêtres d'une session",
    "  d  /  x                     mettre cette session en arrière-plan / la tuer",
    "  n                           en créer une nouvelle",
    "  Échap  ou  Ctrl+b puis a    la refermer sans rien changer",
    "",
    "Une fois le préfixe Ctrl+a pressé :",
    "  a                        envoie un vrai Ctrl+a",
    "  Tab / Maj+Tab            envoie une tabulation (complétion du shell)",
    "  n , Entrée               fenêtre suivante        p , ^   fenêtre précédente",
    "  0..9                     aller à la fenêtre N    Ctrl+a  fenêtre précédente",
    "  w  ou  \"                sélecteur de sessions (comme Ctrl+b puis a)",
    "  c                        nouvelle fenêtre (shell) C  nouvelle fenêtre avec commande",
    "  k                        tuer la fenêtre courante  x  tuer toute la session",
    "  r                        renommer la fenêtre courante",
    "  d                        arrière-plan (détacher ce terminal)   D  déconnecter les autres",
    "  [                        mode historique (↑ ↓ PgUp PgDn Début, Échap sortir)",
    "  l                        redessiner               t  état de la session",
    "  ?  ou  h                 cette aide",
    "",
    "Note : Tab et Maj+Tab ne sont plus capturées du tout par rscreen — elles",
    "vont directement à l'application tenue par la fenêtre. Pour changer de",
    "fenêtre : Ctrl+a n / Ctrl+a p, les touches 0..9, ou le sélecteur.",
    "Le duo Ctrl+b puis a fonctionne sur tous les terminaux, y compris ceux",
    "qui ne savent pas envoyer Ctrl+Tab. Au-delà de 300 ms sans deuxième",
    "touche, Ctrl+b est rendu à l'application (Ctrl+b = « flèche gauche »",
    "sous readline / emacs, et il reste donc utilisable ailleurs).",
];

/// durée du duo « Ctrl+b puis a » : passé ce délai, Ctrl+b est rendu à l'app
const CHORD_MS: u64 = 300;

impl Client {
    fn new(cfg: Config) -> Result<Client> {
        let (rows, cols) = term::term_size(0)?;
        let session = cfg.session.clone();
        let meta = SessionMeta {
            name: session.clone(),
            ..Default::default()
        };
        Ok(Client {
            pty_rows: rows.saturating_sub(1).max(2),
            pty_cols: cols,
            session,
            meta,
            cfg,
            sock: None,
            writer: None,
            rbuf: Vec::new(),
            keybuf: Vec::new(),
            pend_in: Vec::new(),
            out: Vec::new(),
            rows,
            cols,
            mode: Mode::Live,
            chord_pending: Vec::new(),
            chord_from: Mode::Live,
            chord_at: None,
            msg: None,
            picker: Picker::default(),
            prompt_buf: String::new(),
            prompt_pos: 0,
            prompt_label: String::new(),
            prompt_kind: PromptKind::NewWindow,
            prompt_target: 0,
            confirm_session: false,
            help_top: 0,
            kitty_term: 0,
            kitty_app: 0,
            app_cursor: false,
            pending: None,
            status_cache: Vec::new(),
            dead: false,
        })
    }

    // ---------------------------------------------------------------- terminal

    fn setup_term(&mut self) {
        self.out
            .extend_from_slice(b"\x1b[?1049h\x1b[0m\x1b[?7h\x1b[?25l");
        self.set_region();
        // requête kitty puis activation : utile pour bien reconna\u00eetre les
        // modificateurs (Ctrl+Entrée), mais le sélecteur, lui, n'en dépend pas
        self.out.extend_from_slice(b"\x1b[?u\x1b[>1u");
        self.flush_out();
        self.kitty_wait();
        if self.kitty_term != 0 {
            self.notify("protocole clavier étendu actif · sélecteur : Ctrl+b puis a");
        } else {
            self.notify("sélecteur : Ctrl+b puis a · fenêtres : Ctrl+a n / p / 0..9");
        }
    }

    fn teardown(&mut self) {
        self.out.extend_from_slice(
            b"\x1b[0m\x1b[39;49m\x1b[m\x1b[r\x1b[<u\x1b[?1049l\x1b[?25h\x1b[?1000l\x1b[?1006l",
        );
        self.flush_out();
    }

    fn set_region(&mut self) {
        let s = format!("\x1b[1;{}r", self.pty_rows);
        self.out.extend_from_slice(s.as_bytes());
    }

    /// lit les réponses éventuelles du terminal avant d'entrer dans la boucle
    fn kitty_wait(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(150);
        let mut tmp = [0u8; 512];
        while Instant::now() < deadline {
            let mut p = term::Poller::default();
            p.add(0, term::TOK_STDIN, 0);
            let ready = match p.wait(10) {
                Ok(r) => r,
                Err(_) => break,
            };
            if ready.is_empty() {
                continue;
            }
            let n = match std::io::stdin().read(&mut tmp) {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            self.keybuf.extend_from_slice(&tmp[..n]);
            loop {
                match keys::decode(&self.keybuf) {
                    Dec::Need => break,
                    Dec::Invalid(k) => {
                        self.keybuf.drain(..k);
                    }
                    Dec::Done(k, used) => {
                        self.keybuf.drain(..used);
                        if k.kind == Kind::Resp {
                            if let Some(f) = parse_kitty_reply(&k.raw) {
                                self.kitty_term = f;
                            }
                        }
                    }
                }
            }
            if self.kitty_term != 0 {
                break;
            }
        }
    }

    fn flush_out(&mut self) {
        if self.out.is_empty() {
            return;
        }
        let buf = std::mem::take(&mut self.out);
        let mut off = 0usize;
        while off < buf.len() {
            let r = unsafe {
                libc::write(
                    1,
                    buf[off..].as_ptr() as *const libc::c_void,
                    buf.len() - off,
                )
            };
            if r <= 0 {
                break;
            }
            off += r as usize;
        }
    }

    // ------------------------------------------------------------- connexions

    fn connect(&mut self, name: &str, create: bool, first_win: Option<u32>) -> Result<()> {
        self.session = name.to_string();
        self.rbuf.clear();
        self.keybuf.clear();
        self.meta = SessionMeta {
            name: name.to_string(),
            ..Default::default()
        };
        self.kitty_app = 0;
        self.app_cursor = false;
        let path = paths::socket_path(name);
        let mut attempt = 0usize;
        let mut spawned = false;
        let sock = loop {
            match UnixStream::connect(&path) {
                Ok(s) => break s,
                Err(e) => {
                    if !spawned {
                        if paths::session_dir(name).exists() && !session_alive(name) {
                            paths::cleanup_session_dir(name);
                        }
                        if !create {
                            return Err(msg(format!(
                                "aucune session « {name} » en cours ({e})\n\
                                 \t(`rscreen -S {name}` pour en démarrer une)"
                            )));
                        }
                        self.spawn_server(name)?;
                        spawned = true;
                    }
                    attempt += 1;
                    if attempt > 200 {
                        return Err(msg(format!("impossible de joindre « {name} »: {e}")));
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
            }
        };
        let writer = sock.try_clone()?;
        sock.set_write_timeout(Some(Duration::from_secs(5)))?;
        self.sock = Some(sock);
        self.writer = Some(writer);
        self.send(&CMsg::Resize {
            rows: self.pty_rows,
            cols: self.pty_cols,
        });
        self.send(&CMsg::KittyFlags(self.kitty_term));
        self.send(&CMsg::Query);
        if let Some(w) = first_win {
            self.send(&CMsg::Switch(w));
        }
        // premier rendu complet
        self.out.extend_from_slice(b"\x1b[H\x1b[2J");
        self.flush_out();
        Ok(())
    }

    fn spawn_server(&mut self, name: &str) -> Result<()> {
        let exe = std::env::current_exe()?;
        let mut args: Vec<String> = vec![
            "--__server".to_string(),
            name.to_string(),
            self.rows.to_string(),
            self.cols.to_string(),
            self.cfg.scrollback.to_string(),
        ];
        if !self.cfg.cmd.is_empty() {
            args.push("--cmd".to_string());
            args.push(self.cfg.cmd.join("\u{1}"));
        }
        if !self.cfg.title.is_empty() {
            args.push("--title".to_string());
            args.push(self.cfg.title.clone());
        }
        if std::env::var("RSCREEN_DEBUG").is_ok() {
            args.push("--log".to_string());
            args.push(
                paths::base_dir()
                    .join("debug.log")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        pty::spawn_detached(&exe, &args)?;
        Ok(())
    }

    fn send(&mut self, m: &CMsg) {
        if let Some(w) = self.writer.as_mut() {
            let mut buf = Vec::new();
            m.write_to(&mut buf);
            let _ = w.write_all(&buf);
        }
    }

    fn send_input(&mut self, bytes: &[u8]) {
        self.pend_in.extend_from_slice(bytes);
    }

    fn flush_input(&mut self) {
        if self.pend_in.is_empty() {
            return;
        }
        let b = std::mem::take(&mut self.pend_in);
        self.send(&CMsg::Input(b));
    }

    // ------------------------------------------------------------ boucle de jeu

    fn run(&mut self) -> Result<()> {
        let mut poller = term::Poller::default();
        let sig = term::sigmask(&[libc::SIGWINCH, libc::SIGTERM, libc::SIGINT, libc::SIGHUP])?;
        let sigfd: OwnedFd = term::make_signalfd(&sig)?;
        loop {
            poller.clear();
            poller.add(0, term::TOK_STDIN, 0);
            poller.add(sigfd.as_raw_fd(), term::TOK_SIGNAL, 0);
            if let Some(s) = self.sock.as_ref() {
                poller.add(s.as_raw_fd(), term::TOK_CLIENT, 0);
            }
            let mut timeout = if self.keybuf.is_empty() { 400 } else { 60 };
            if let Some(dl) = self.chord_at {
                // on doit se réveiller à l'heure pour rendre Ctrl+b à l'app
                let left = dl.saturating_duration_since(Instant::now()).as_millis() as i32;
                if left < timeout {
                    timeout = left.max(1);
                }
            }
            let ready = poller.wait(timeout)?;
            let mut stdin_evt = false;

            for (tok, _, _) in ready {
                match tok {
                    term::TOK_STDIN => {
                        stdin_evt = true;
                        let mut tmp = [0u8; 8192];
                        let n = unsafe {
                            libc::read(0, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                        };
                        if n <= 0 {
                            self.notify("entrée fermée : détachement");
                            self.flush_keys(true);
                            self.flush_input();
                            self.tick();
                            self.draw();
                            self.flush_out();
                            break;
                        }
                        self.keybuf.extend_from_slice(&tmp[..n as usize]);
                    }
                    term::TOK_CLIENT => self.read_server()?,
                    term::TOK_SIGNAL => match term::read_signal(sigfd.as_raw_fd()) {
                        libc::SIGWINCH => self.on_winch(),
                        libc::SIGTERM | libc::SIGINT | libc::SIGHUP => {
                            self.detach();
                            self.dead = true;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }

            if !self.keybuf.is_empty() {
                self.flush_keys(!stdin_evt);
            }
            self.expire_chord();
            self.flush_input();

            if let Some(name) = self.pending.take() {
                if let Err(e) = self.connect(&name, true, None) {
                    self.notify(format!("{e}"));
                }
            }

            self.tick();
            self.draw();
            self.flush_out();
            if self.dead {
                return Ok(());
            }
        }
    }

    fn tick(&mut self) {
        if let Some((_, when)) = self.msg.clone() {
            if when.elapsed() > Duration::from_secs(4) {
                self.msg = None;
            }
        }
    }

    fn read_server(&mut self) -> Result<()> {
        let frames = match self.sock.as_mut() {
            Some(s) => proto::read_frames(s, &mut self.rbuf),
            None => Ok(Vec::new()),
        };
        let fl = match frames {
            Ok(fl) => fl,
            Err(_) => {
                self.dead = true;
                return Ok(());
            }
        };
        if fl.is_empty() {
            self.notify(format!("session « {} » terminée", self.session));
            self.dead = true;
            return Ok(());
        }
        for (t, p) in fl {
            let m = match SMsg::parse(t, &p) {
                Ok(Some(m)) => m,
                _ => continue,
            };
            match m {
                SMsg::Data(b) => {
                    if !self.overlay_open() {
                        self.out.extend_from_slice(&b);
                    }
                }
                SMsg::Meta(s) => {
                    if let Some(mt) = SessionMeta::decode(&s) {
                        self.meta = mt;
                        if let Some(w) = self.meta.active_win() {
                            self.app_cursor = w.app_cursor;
                            self.kitty_app = w.kitty;
                        }
                    }
                }
                SMsg::Msg(s) => self.msg = Some((s, Instant::now())),
                SMsg::Bell => self.out.push(7),
                SMsg::Kitty(f) => self.kitty_app = f,
                SMsg::Bye => {
                    self.dead = true;
                }
                SMsg::Title { .. } | SMsg::KittyQuery => {}
            }
        }
        Ok(())
    }

    fn overlay_open(&self) -> bool {
        let m = self.shown_mode();
        m == Mode::Picker || m == Mode::Help
    }

    /// le mode réellement visible pendant un duo : le sélecteur reste à
    /// l'écran pendant qu'on attend la deuxième touche
    fn shown_mode(&self) -> Mode {
        if self.mode == Mode::Chord {
            self.chord_from
        } else {
            self.mode
        }
    }

    fn on_winch(&mut self) {
        let (rows, cols) = match term::term_size(0) {
            Ok(v) => v,
            Err(_) => return,
        };
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.pty_rows = rows.saturating_sub(1).max(2);
        self.pty_cols = cols;
        self.set_region();
        self.out.extend_from_slice(b"\x1b[2J\x1b[H");
        self.send(&CMsg::Resize {
            rows: self.pty_rows,
            cols: self.pty_cols,
        });
        self.send(&CMsg::Repaint);
    }

    fn detach(&mut self) {
        self.send(&CMsg::Detach);
        std::thread::sleep(Duration::from_millis(40));
    }

    // -------------------------------------------------------------- clavier

    fn flush_keys(&mut self, force: bool) {
        loop {
            match keys::decode(&self.keybuf) {
                Dec::Need => {
                    if !force || self.keybuf.is_empty() {
                        return;
                    }
                    let b = self.keybuf.remove(0);
                    let k = keys::byte_key(b);
                    self.on_key(&k);
                }
                Dec::Invalid(n) => {
                    self.keybuf.drain(..n);
                }
                Dec::Done(k, n) => {
                    self.keybuf.drain(..n);
                    self.on_key(&k);
                }
            }
        }
    }

    fn on_key(&mut self, k: &Key) {
        match self.mode {
            Mode::Live => self.live_key(k),
            Mode::Prefix => self.prefix_key(k),
            Mode::Chord => self.chord_key(k),
            Mode::History => self.history_key(k),
            Mode::Picker => self.picker_key(k),
            Mode::Help => self.help_key(k),
            Mode::Prompt => self.prompt_key(k),
            Mode::Confirm => self.confirm_key(k),
        }
    }

    fn live_key(&mut self, k: &Key) {
        if k.kind == Kind::Resp {
            if let Some(f) = parse_kitty_reply(&k.raw) {
                self.kitty_term = f;
                self.send(&CMsg::KittyFlags(f));
            }
            return;
        }
        if k.is_enter_ctrl() {
            self.detach();
            self.notify(format!("session « {} » en arrière-plan", self.session));
            self.dead = true;
            return;
        }
        if k.ctrl_only() == Some('a') {
            self.mode = Mode::Prefix;
            return;
        }
        if k.ctrl_only() == Some('b') {
            self.start_chord(k, Mode::Live);
            return;
        }
        // Tab et Maj+Tab sont laisseees : elles partent vers l'application
        // (completion du shell, TAB d'un editeur, touches de navigation...)
        self.forward(k);
    }

    fn forward(&mut self, k: &Key) {
        // forme legacy recue du terminal: on la transmet telle quelle.
        // forme etendue (kitty/modifyOtherKeys): on la retraduit en legacy,
        // sauf si l'application de la fenetre gere elle-meme le protocole.
        let bytes = if k.enc == 0 || self.kitty_app != 0 {
            k.raw.clone()
        } else {
            keys::legacy_bytes(k, self.app_cursor, false)
        };
        if !bytes.is_empty() {
            self.send_input(&bytes);
        }
    }

    fn prefix_key(&mut self, k: &Key) {
        self.mode = Mode::Live;
        if k.kind == Kind::Tab {
            // Ctrl+a Tab = vraie tabulation (complete le shell)
            self.forward(k);
            return;
        }
        if k.ctrl_only() == Some('a') || (k.kind == Kind::Escape && k.mods == 0) {
            self.send(&CMsg::Last);
            return;
        }
        if let Some(d) = k.digit() {
            if self.meta.wins.iter().any(|w| w.idx == d) {
                self.send(&CMsg::Switch(d));
            } else {
                self.notify(format!("aucune fenêtre {d}"));
            }
            return;
        }
        let c = key_char(k);
        match c {
            Some('n') | Some(' ') => self.send(&CMsg::Next),
            Some('p') | Some('^') => self.send(&CMsg::Prev),
            Some('w') | Some('"') | Some('q') => self.open_picker(),
            Some('c') => self.send(&CMsg::New {
                cmd: Vec::new(),
                title: String::new(),
            }),
            Some('C') => self.ask(PromptKind::NewWindow, "nouvelle fenêtre — commande : ", 0),
            Some('k') => {
                self.prompt_target = self.meta.active_win().map(|w| w.idx).unwrap_or(0);
                self.confirm_session = false;
                self.mode = Mode::Confirm;
                self.notify(format!("tuer la fenêtre {} ? o/N", self.prompt_target));
            }
            Some('x') | Some('X') => {
                self.confirm_session = true;
                self.mode = Mode::Confirm;
                self.notify(format!("tuer la session « {} » ? o/N", self.session));
            }
            Some('d') => {
                self.detach();
                self.notify(format!("session « {} » en arrière-plan", self.session));
                self.draw();
                self.flush_out();
                self.dead = true;
            }
            Some('D') => {
                self.send(&CMsg::Detach);
                self.notify("autres clients déconnectés (session redémarrée en arrière-plan)");
            }
            Some('r') | Some('R') => {
                let idx = self.meta.active_win().map(|w| w.idx).unwrap_or(0);
                self.ask(
                    PromptKind::Rename,
                    &format!("renommer la fenêtre {idx} : "),
                    idx,
                );
            }
            Some('[') | Some('{') => {
                self.mode = Mode::History;
                self.notify("historique : ↑ ↓ PgUp PgDn Début · Échap pour revenir");
            }
            Some('l') => {
                self.out.extend_from_slice(b"\x1b[2J\x1b[H");
                // l'ecran efface a aussi efface la barre : on oublie son cache
                self.status_cache.clear();
                self.send(&CMsg::Repaint);
            }
            Some('t') => self.notify(format!(
                "session {} · {} fenêtre(s) · {} · {}×{}",
                self.session,
                self.meta.wins.len(),
                if self.meta.attached > 0 {
                    "jointe"
                } else {
                    "libre"
                },
                self.cols,
                self.rows
            )),
            Some('?') | Some('h') => {
                self.help_top = 0;
                self.mode = Mode::Help;
            }
            Some('a') => self.send_input(&[1u8]),
            _ => {
                if k.kind == Kind::Escape {
                    self.notify("préfixe annulé");
                } else {
                    self.notify(format!(
                        "raccourci inconnu après Ctrl+a : {}",
                        keys::label(k)
                    ));
                }
            }
        }
    }

    fn history_key(&mut self, k: &Key) {
        let page = (self.pty_rows.max(3) as i64) - 1;
        match k.kind {
            // sens: une distance au-dessus de la ligne de vie
            Kind::Up => self.send(&CMsg::Scroll {
                op: ScrollOp::Rel,
                amount: 1,
            }),
            Kind::Down => self.send(&CMsg::Scroll {
                op: ScrollOp::Rel,
                amount: -1,
            }),
            Kind::PageUp => self.send(&CMsg::Scroll {
                op: ScrollOp::Rel,
                amount: page,
            }),
            Kind::PageDown => self.send(&CMsg::Scroll {
                op: ScrollOp::Rel,
                amount: -page,
            }),
            Kind::Home => self.send(&CMsg::Scroll {
                op: ScrollOp::Top,
                amount: 0,
            }),
            Kind::End => self.exit_history(),
            _ => {
                if k.plain() == Some('g') {
                    self.send(&CMsg::Scroll {
                        op: ScrollOp::Top,
                        amount: 0,
                    });
                } else {
                    // G, Entrée, Échap et toute autre touche : retour à la ligne de vie
                    self.exit_history();
                }
            }
        }
    }

    fn confirm_key(&mut self, k: &Key) {
        let c = key_char(k).unwrap_or('\u{0}');
        if c == 'y' || c == 'Y' || c == 'o' || c == 'O' {
            if self.confirm_session {
                self.send(&CMsg::KillSession);
                self.notify(format!("session « {} » tuée", self.session));
            } else {
                let t = self.prompt_target;
                self.send(&CMsg::Kill(t));
            }
        } else {
            self.notify("annulé");
        }
        self.mode = Mode::Live;
    }

    fn help_key(&mut self, k: &Key) {
        let last = HELP.len().saturating_sub(1);
        match k.kind {
            Kind::Down | Kind::PageDown => self.help_top = (self.help_top + 1).min(last),
            Kind::Up | Kind::PageUp => self.help_top = self.help_top.saturating_sub(1),
            Kind::Home => self.help_top = 0,
            Kind::End => self.help_top = last,
            _ => {}
        }
        if k.plain() == Some('j') {
            self.help_top = (self.help_top + 1).min(last);
        }
        if k.plain() == Some('k') {
            self.help_top = self.help_top.saturating_sub(1);
        }
        if k.kind == Kind::Escape
            || k.plain() == Some('q')
            || k.plain() == Some('?')
            || k.plain() == Some('h')
        {
            self.close_overlay();
        }
    }

    fn prompt_key(&mut self, k: &Key) {
        match k.kind {
            Kind::Enter => {
                let line = std::mem::take(&mut self.prompt_buf);
                self.mode = Mode::Live;
                match self.prompt_kind {
                    PromptKind::NewWindow => {
                        let cmd = pty::split_command(line.trim());
                        self.send(&CMsg::New {
                            cmd,
                            title: String::new(),
                        });
                    }
                    PromptKind::NewSession => {
                        let name = paths::sanitize(line.trim());
                        if !name.is_empty() {
                            self.pending = Some(name);
                        }
                    }
                    PromptKind::Rename => {
                        let t = self.prompt_target;
                        self.send(&CMsg::Rename {
                            idx: t,
                            name: line.trim().to_string(),
                        });
                    }
                }
            }
            Kind::Escape => {
                self.mode = Mode::Live;
                self.prompt_buf.clear();
            }
            Kind::Backspace => {
                if self.prompt_pos > 0 {
                    let p = prev_char(&self.prompt_buf, self.prompt_pos);
                    self.prompt_buf.remove(p);
                    self.prompt_pos = p;
                }
            }
            Kind::Delete => {
                let p = next_char(&self.prompt_buf, self.prompt_pos);
                if p < self.prompt_buf.len() {
                    self.prompt_buf.remove(p);
                }
            }
            Kind::Left => self.prompt_pos = prev_char(&self.prompt_buf, self.prompt_pos),
            Kind::Right => self.prompt_pos = next_char(&self.prompt_buf, self.prompt_pos),
            Kind::Home => self.prompt_pos = 0,
            Kind::End => self.prompt_pos = self.prompt_buf.len(),
            _ => {
                if k.ctrl_only() == Some('u') {
                    self.prompt_buf.clear();
                    self.prompt_pos = 0;
                } else if k.ctrl_only() == Some('w') {
                    while self.prompt_pos > 0 {
                        let p = prev_char(&self.prompt_buf, self.prompt_pos);
                        if self.prompt_buf[p..].starts_with(' ') {
                            break;
                        }
                        self.prompt_pos = p;
                    }
                    self.prompt_buf.truncate(self.prompt_pos);
                } else if let Some(c) = key_char(k) {
                    if c != '\u{0}' && c != '\t' {
                        self.prompt_buf.insert(self.prompt_pos, c);
                        self.prompt_pos += c.len_utf8();
                    }
                }
            }
        }
    }

    fn ask(&mut self, kind: PromptKind, label: &str, target: u32) {
        self.prompt_kind = kind;
        self.prompt_target = target;
        self.prompt_buf = String::new();
        self.prompt_pos = 0;
        self.prompt_label = label.to_string();
        self.mode = Mode::Prompt;
    }

    fn open_picker(&mut self) {
        if !self.picker.expanded.iter().any(|e| e == &self.session) {
            self.picker.expanded.push(self.session.clone());
        }
        self.picker.first_open = true;
        self.picker.sel = 0;
        self.refresh_picker();
        self.mode = Mode::Picker;
    }

    // ------------------------------------------------- duo « Ctrl+b puis a »

    /// on entre dans le duo : `Ctrl+b` est note, la prochaine touche decidera
    fn start_chord(&mut self, k: &Key, from: Mode) {
        self.chord_pending = if k.enc == 0 || self.kitty_app != 0 {
            k.raw.clone()
        } else {
            keys::legacy_bytes(k, self.app_cursor, false)
        };
        self.chord_from = from;
        self.chord_at = Some(Instant::now() + Duration::from_millis(CHORD_MS));
        self.mode = Mode::Chord;
    }

    /// deuxieme touche du duo « Ctrl+b puis a »
    fn chord_key(&mut self, k: &Key) {
        let from = self.chord_from;
        let pend = std::mem::take(&mut self.chord_pending);
        self.chord_at = None;
        let wants_picker = k.ctrl_only() == Some('a')
            || matches!(key_char(k), Some(c) if c.to_ascii_lowercase() == 'a');
        if wants_picker {
            match from {
                // le duo referme la fenetre modale d'ou il vient d'etre lance
                Mode::Picker => self.close_overlay(),
                _ => self.open_picker(),
            }
            return;
        }
        self.mode = from;
        if from != Mode::Live {
            return;
        }
        // ce n'etait pas « Ctrl+b a » : on rend les deux touches a l'app, comme
        // si rscreen ne les avait pas vues passees
        if !pend.is_empty() {
            self.send_input(&pend);
        }
        self.forward(k);
    }

    /// le delai est passe sans deuxieme touche : Ctrl+b retourne a l'app
    fn expire_chord(&mut self) {
        if self.mode != Mode::Chord {
            self.chord_at = None;
            return;
        }
        let Some(dl) = self.chord_at else {
            return;
        };
        if Instant::now() < dl {
            return;
        }
        let from = self.chord_from;
        self.mode = from;
        self.chord_at = None;
        if from == Mode::Live {
            let pend = std::mem::take(&mut self.chord_pending);
            if !pend.is_empty() {
                self.send_input(&pend);
            }
        }
    }

    fn close_overlay(&mut self) {
        self.mode = Mode::Live;
        self.send(&CMsg::Repaint);
    }

    fn exit_history(&mut self) {
        self.mode = Mode::Live;
        self.send(&CMsg::Scroll {
            op: ScrollOp::Bottom,
            amount: 0,
        });
        self.send(&CMsg::Repaint);
    }

    // ------------------------------------------------------- sélecteur sessions

    fn refresh_picker(&mut self) {
        let sessions = meta::all_sessions();
        let mut items: Vec<Item> = Vec::new();
        for s in &sessions {
            let cur = s.name == self.session;
            let expanded = self.picker.expanded.iter().any(|e| e == &s.name);
            let label = format!(
                "{} {}{}",
                if expanded { "▾" } else { "▸" },
                s.name,
                if cur { "  ← courante" } else { "" }
            );
            let info = format!(
                "{}×{} · {} fen. · pid {} · {}",
                s.rows,
                s.cols,
                s.wins.len(),
                s.pid,
                if s.attached > 0 {
                    "JOINTE"
                } else {
                    "ARRIÈRE-PLAN"
                }
            );
            items.push(Item {
                kind: ItemKind::Session,
                session: s.name.clone(),
                win: None,
                label,
                info,
                cur,
            });
            if expanded {
                for w in &s.wins {
                    items.push(Item {
                        kind: ItemKind::Window,
                        session: s.name.clone(),
                        win: Some(w.idx),
                        label: format!(
                            "     {} {:<3} {}",
                            if w.active { "•" } else { " " },
                            w.idx,
                            truncate(w.display(), 28)
                        ),
                        info: String::new(),
                        cur,
                    });
                }
            }
        }
        items.push(Item {
            kind: ItemKind::NewSession,
            session: String::new(),
            win: None,
            label: " + nouvelle session…".into(),
            info: String::new(),
            cur: false,
        });
        items.push(Item {
            kind: ItemKind::ReturnShell,
            session: self.session.clone(),
            win: None,
            label: " ← retour au shell".into(),
            info: "Entrée".into(),
            cur: true,
        });
        items.push(Item {
            kind: ItemKind::Background,
            session: self.session.clone(),
            win: None,
            label: " ⤳ rester en arrière-plan (détacher ce terminal)".into(),
            info: "Ctrl+a d".into(),
            cur: true,
        });
        // conserver la selection d'un rafraichissement a l'autre
        let remember = self
            .picker
            .items
            .get(self.picker.sel)
            .map(|i| (i.kind as u8, i.session.clone(), i.win));
        self.picker.items = items;
        let mut sel = match remember {
            Some((k, ses, win)) => self
                .picker
                .items
                .iter()
                .position(|i| i.kind as u8 == k && i.session == ses && i.win == win)
                .unwrap_or(0),
            None => 0,
        };
        if self.picker.first_open {
            sel = self
                .picker
                .items
                .iter()
                .position(|i| i.kind == ItemKind::Session && i.session == self.session)
                .unwrap_or(0);
            self.picker.first_open = false;
        }
        self.picker.sel = sel;
        self.picker.sel = self
            .picker
            .sel
            .min(self.picker.items.len().saturating_sub(1));
        self.clamp_picker();
    }

    fn picker_key(&mut self, k: &Key) {
        let n = self.picker.items.len();
        if n == 0 {
            self.close_overlay();
            return;
        }
        if k.kind == Kind::Escape {
            self.close_overlay();
            return;
        }
        if k.ctrl_only() == Some('b') {
            // Ctrl+b puis a referme le selecteur
            self.start_chord(k, Mode::Picker);
            return;
        }
        let vis = self.picker_vis().max(1);
        let mut pick = false;
        let mut act = None;
        match k.kind {
            Kind::Down => self.picker.sel = (self.picker.sel + 1).min(n - 1),
            Kind::Up => self.picker.sel = self.picker.sel.saturating_sub(1),
            Kind::PageDown => self.picker.sel = (self.picker.sel + vis).min(n - 1),
            Kind::PageUp => self.picker.sel = self.picker.sel.saturating_sub(vis),
            Kind::Home => self.picker.sel = 0,
            Kind::End => self.picker.sel = n - 1,
            Kind::Right | Kind::Left => {
                let it = &self.picker.items[self.picker.sel.min(n - 1)];
                let name = it.session.clone();
                let want_open = k.kind == Kind::Right;
                let is_open = self.picker.expanded.iter().any(|e| e == &name);
                if want_open != is_open {
                    if want_open {
                        self.picker.expanded.push(name);
                    } else {
                        self.picker.expanded.retain(|e| e != &name);
                    }
                    self.refresh_picker();
                }
            }
            Kind::Enter => {
                act = Some(self.picker.items[self.picker.sel.min(n - 1)].clone());
            }
            // Tab n'est pas une touche du selecteur : on l'ignore purement
            _ => {}
        }
        if let Some(c) = key_char(k) {
            match c {
                'j' => self.picker.sel = (self.picker.sel + 1).min(n - 1),
                'k' => self.picker.sel = self.picker.sel.saturating_sub(1),
                'g' => self.picker.sel = 0,
                'G' => self.picker.sel = n - 1,
                'q' => {
                    self.close_overlay();
                    return;
                }
                ' ' | 'h' | 'l' => {
                    let it = self.picker.items[self.picker.sel.min(n - 1)].clone();
                    let name = it.session.clone();
                    let is_open = self.picker.expanded.iter().any(|e| e == &name);
                    if it.kind == ItemKind::Session {
                        if is_open || c == 'l' {
                            if c != 'h' {
                                self.picker.expanded.push(name);
                            } else {
                                self.picker.expanded.retain(|e| e != &name);
                            }
                        } else {
                            self.picker.expanded.push(name);
                        }
                        self.refresh_picker();
                    }
                }
                'x' => {
                    let it = self.picker.items[self.picker.sel.min(n - 1)].clone();
                    self.kill_session(&it.session);
                    self.refresh_picker();
                }
                'd' => {
                    let it = self.picker.items[self.picker.sel.min(n - 1)].clone();
                    self.detach_session(&it.session);
                    self.refresh_picker();
                }
                'n' => {
                    self.mode = Mode::Live;
                    self.ask(PromptKind::NewSession, "nom de la nouvelle session : ", 0);
                    return;
                }
                _ => {}
            }
            pick = true;
        }
        let _ = pick;
        if let Some(it) = act {
            self.picker_activate(&it);
            return;
        }
        self.clamp_picker();
    }

    fn picker_activate(&mut self, it: &Item) {
        match it.kind {
            ItemKind::Session => {
                if it.session == self.session {
                    self.close_overlay();
                    return;
                }
                match self.connect(&it.session, true, None) {
                    Ok(()) => {
                        self.notify(format!("bascule sur la session « {} »", it.session));
                        self.close_overlay();
                    }
                    Err(e) => {
                        self.notify(format!("{e}"));
                        self.refresh_picker();
                    }
                }
            }
            ItemKind::Window => {
                let w = it.win.unwrap_or(0);
                if it.session == self.session {
                    self.send(&CMsg::Switch(w));
                    self.close_overlay();
                } else {
                    match self.connect(&it.session, true, Some(w)) {
                        Ok(()) => {
                            self.notify(format!("session « {} » · fenêtre {w}", it.session));
                            self.close_overlay();
                        }
                        Err(e) => {
                            self.notify(format!("{e}"));
                            self.refresh_picker();
                        }
                    }
                }
            }
            ItemKind::Background => {
                self.detach();
                self.mode = Mode::Live;
                self.notify(format!(
                    "session « {} » laissée en arrière-plan",
                    self.session
                ));
                self.draw();
                self.flush_out();
                self.dead = true;
            }
            ItemKind::NewSession => {
                self.mode = Mode::Live;
                self.ask(PromptKind::NewSession, "nom de la nouvelle session : ", 0);
            }
            ItemKind::ReturnShell => {
                // on ne quitte pas la session : on referme juste la fenetre
                // modale, l'ecran du shell revient
                self.close_overlay();
            }
        }
    }

    fn kill_session(&mut self, name: &str) {
        if let Ok(mut s) = UnixStream::connect(paths::socket_path(name)) {
            let mut buf = Vec::new();
            CMsg::KillSession.write_to(&mut buf);
            let _ = s.write_all(&buf);
            let _ = s.flush();
        }
        self.notify(format!("session « {name} » tuée"));
    }

    fn detach_session(&mut self, name: &str) {
        if let Ok(mut s) = UnixStream::connect(paths::socket_path(name)) {
            let mut buf = Vec::new();
            CMsg::Detach.write_to(&mut buf);
            let _ = s.write_all(&buf);
            let _ = s.flush();
        }
        self.notify(format!("session « {name} » mise en arrière-plan"));
    }

    fn notify(&mut self, s: impl Into<String>) {
        self.msg = Some((s.into(), Instant::now()));
    }

    // ------------------------------------------------------------- affichage

    fn draw(&mut self) {
        let m = self.shown_mode();
        if m == Mode::Picker {
            self.draw_picker();
        } else if m == Mode::Help {
            self.draw_help();
        }
        self.draw_status();
    }

    /// save/restore du curseur: le serveur envoie des deplacements relatifs
    /// calcules a partir de l'ecran emule, il ne faut surtout pas le perturber
    fn save_cur(&mut self) {
        self.out.extend_from_slice(b"\x1b7");
    }

    fn rest_cur(&mut self) {
        self.out.extend_from_slice(b"\x1b8");
    }

    fn goto(&mut self, row: u16, col: u16) {
        let s = format!("\x1b[{};{}H", row + 1, col + 1);
        self.out.extend_from_slice(s.as_bytes());
    }

    fn draw_status(&mut self) {
        let row = self.status_row();
        if row == self.status_cache {
            return;
        }
        self.status_cache = row;
        let cached = self.status_cache.clone();
        self.save_cur();
        self.goto(self.rows.saturating_sub(1), 0);
        self.out.extend_from_slice(&cached);
        self.rest_cur();
    }

    /// ligne de statut (barre fenetree) : session, etat, liste des fenetres,
    /// messages, horloge. Le rendu est mis en cache pour ne pas saturer le tty.
    fn status_row(&self) -> Vec<u8> {
        let cols = self.cols as usize;
        let mut o: Vec<u8> = Vec::new();
        if self.mode == Mode::Prompt {
            let nlabel = self.prompt_label.chars().count();
            let curcell = nlabel
                + self.prompt_buf[..self.prompt_pos.min(self.prompt_buf.len())]
                    .chars()
                    .count();
            let line = format!("{}{}", self.prompt_label, self.prompt_buf);
            o.extend_from_slice(BAR_TITLE.as_bytes());
            let mut w = 0usize;
            let chars: Vec<char> = line.chars().collect();
            for (i, c) in chars.iter().enumerate() {
                let cw = c.width().unwrap_or(0);
                if w + cw + 1 > cols {
                    break;
                }
                if i == curcell {
                    o.extend_from_slice(b"\x1b[7m");
                }
                let mut b = [0u8; 4];
                o.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                if i == curcell {
                    o.extend_from_slice(b"\x1b[27m");
                }
                w += cw;
            }
            if curcell >= chars.len() && w < cols {
                o.extend_from_slice(b"\x1b[7m \x1b[27m");
                w += 1;
            }
            pad_spaces(&mut o, w, cols);
            o.extend_from_slice(b"\x1b[0m");
            return o;
        }

        let attached = if self.meta.attached > 0 {
            "jointe"
        } else {
            "libre"
        };
        let mut left = format!(" rscreen « {} » [{}] ", self.session, attached);
        match self.mode {
            Mode::Prefix => left.push_str(" Ctrl+a… "),
            Mode::Chord => left.push_str(" Ctrl+b… "),
            Mode::History => left.push_str(" HISTORIQUE "),
            Mode::Confirm => left.push_str(" CONFIRMATION "),
            Mode::Picker => left.push_str(" SELECTION "),
            Mode::Help => left.push_str(" AIDE "),
            _ => {}
        }
        let right = format!(" {} {}×{} ", clock(), self.cols, self.rows);
        let lw = display_width(&left);
        let rw = display_width(&right);
        styled(&mut o, &left, BAR_TITLE, lw);
        let mut used = lw;
        let msg = self
            .msg
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default();
        if !msg.is_empty() {
            let w = cols.saturating_sub(used + rw).max(1);
            styled(&mut o, &msg, BAR_MSG, w);
            used += w;
        } else {
            for w in &self.meta.wins {
                let t = format!(" {} {} ", w.idx, truncate(w.display(), 16));
                let tw = display_width(&t);
                if used + tw + rw >= cols {
                    break;
                }
                styled(&mut o, &t, if w.active { BAR_HI } else { BAR }, tw);
                used += tw;
            }
            if used + rw < cols {
                o.extend_from_slice(BAR.as_bytes());
                o.extend(std::iter::repeat_n(b' ', cols - used - rw));
                o.extend_from_slice(b"\x1b[0m");
                used = cols - rw;
            }
        }
        if used < cols {
            styled(&mut o, &right, BAR, cols - used);
        }
        o
    }

    fn picker_box_h(&self) -> usize {
        let want = self.picker.items.len() + PICKER_CHROME;
        want.clamp(9, self.pty_rows as usize)
    }

    fn picker_vis(&self) -> usize {
        self.picker_box_h().saturating_sub(PICKER_CHROME).max(1)
    }

    fn clamp_picker(&mut self) {
        let vis = self.picker_vis();
        if self.picker.sel < self.picker.top {
            self.picker.top = self.picker.sel;
        }
        if self.picker.sel >= self.picker.top + vis {
            self.picker.top = self.picker.sel + 1 - vis;
        }
    }

    /// « fenêtre » modale : liste des sessions + fenêtres + choix d'arrière-plan
    fn draw_picker(&mut self) {
        let h = self.picker_box_h() as u16;
        let w = (self.cols as usize).saturating_sub(2).min(92);
        if w < 24 || (h as usize) < 8 {
            return;
        }
        let top = (self.pty_rows.saturating_sub(h)) / 2;
        let left = (self.cols.saturating_sub(w as u16)) / 2;
        let vis = self.picker_vis();
        let nsess = self
            .picker
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Session)
            .count();
        self.save_cur();
        self.border(
            top,
            left,
            w,
            true,
            &format!(
                "sélecteur de sessions — {} session{} · ligne {}/{}",
                nsess,
                if nsess > 1 { "s" } else { "" },
                self.picker.sel + 1,
                self.picker.items.len()
            ),
            BAR_TITLE,
        );
        for i in 0..vis {
            self.goto(top + 1 + i as u16, left);
            let idx = self.picker.top + i;
            let content = w - 2;
            if idx >= self.picker.items.len() {
                self.row_frame("", ROW, w);
                continue;
            }
            let it = self.picker.items[idx].clone();
            let sel = idx == self.picker.sel;
            let sgr = if sel {
                ROW_SEL
            } else if it.cur {
                ROW_CUR
            } else {
                ROW
            };
            let infow = display_width(&it.info);
            let labelw = content.saturating_sub(infow + 1);
            let mut line: Vec<u8> = Vec::new();
            styled(
                &mut line,
                &format!("{}{}", if sel { "▸" } else { " " }, it.label),
                sgr,
                labelw,
            );
            if infow > 0 {
                styled(&mut line, &it.info, DIM, content - labelw);
            }
            self.row_frame_bytes(&line, sgr);
        }
        self.goto(top + 1 + vis as u16, left);
        {
            let mut line: Vec<u8> = Vec::new();
            styled(
                &mut line,
                " ↑↓ choisir · Entrée valider · ←→ déplier · d/x · n · Échap/Ctrl+b a refermer ",
                ROW_CUR,
                w - 2,
            );
            self.row_frame_bytes(&line, ROW_CUR);
        }
        self.border(
            top + h - 1,
            left,
            w,
            false,
            &format!("session courante : {}", self.session),
            DIM,
        );
        self.rest_cur();
    }

    /// bordure supérieure/inférieure d'un cadre : coins + étiquette + tirets
    fn border(&mut self, row: u16, left: u16, w: usize, top: bool, label: &str, sgr: &str) {
        self.goto(row, left);
        let (lch, rch) = if top { ('┌', '┐') } else { ('└', '┘') };
        let mut line = String::new();
        line.push(lch);
        line.push(' ');
        line.push_str(label);
        while display_width(&line) + 1 < w {
            line.push('─');
        }
        line.push(rch);
        styled(&mut self.out, &line, sgr, w);
    }

    fn row_frame(&mut self, txt: &str, sgr: &str, w: usize) {
        let mut line: Vec<u8> = Vec::new();
        styled(&mut line, txt, sgr, w - 2);
        self.row_frame_bytes(&line, sgr);
    }

    /// une ligne d'un cadre : │ ... │
    fn row_frame_bytes(&mut self, content: &[u8], sgr: &str) {
        self.out.extend_from_slice(sgr.as_bytes());
        self.out.extend_from_slice("│".as_bytes());
        self.out.extend_from_slice(b"\x1b[0m");
        self.out.extend_from_slice(content);
        self.out.extend_from_slice(sgr.as_bytes());
        self.out.extend_from_slice("│".as_bytes());
        self.out.extend_from_slice(b"\x1b[0m");
    }

    fn draw_help(&mut self) {
        let h = (HELP.len() + 3).min(self.pty_rows as usize) as u16;
        let w = (self.cols as usize).saturating_sub(2).min(94);
        if w < 24 || (h as usize) < 6 {
            return;
        }
        let top = (self.pty_rows.saturating_sub(h)) / 2;
        let left = (self.cols.saturating_sub(w as u16)) / 2;
        let vis = h as usize - 3;
        self.save_cur();
        self.border(
            top,
            left,
            w,
            true,
            &format!(
                "aide rscreen — ligne {}/{} (↑↓ PgUp PgDn, Échap fermer)",
                self.help_top + 1,
                HELP.len()
            ),
            BAR_TITLE,
        );
        for i in 0..vis {
            self.goto(top + 1 + i as u16, left);
            let txt = HELP.get(self.help_top + i).copied().unwrap_or("");
            let mut line: Vec<u8> = Vec::new();
            styled(&mut line, txt, ROW, w - 2);
            self.row_frame_bytes(&line, ROW);
        }
        self.goto(top + h - 2, left);
        self.row_frame(" Échap ou Ctrl+a ? pour fermer ", ROW_CUR, w);
        self.border(top + h - 1, left, w, false, "rscreen", DIM);
        self.rest_cur();
    }
}

/// lignes non comptées dans la zone de liste du sélecteur
const PICKER_CHROME: usize = 3;

fn pad_spaces(o: &mut Vec<u8>, w: usize, cols: usize) {
    o.extend(std::iter::repeat_n(b' ', cols.saturating_sub(w)));
}

/// ecrit `text` tronque/padde sur `maxw` colonnes avec les attributs `sgr`
fn styled(o: &mut Vec<u8>, text: &str, sgr: &str, maxw: usize) {
    o.extend_from_slice(sgr.as_bytes());
    let mut w = 0usize;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > maxw {
            break;
        }
        let mut b = [0u8; 4];
        o.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        w += cw;
    }
    for _ in w..maxw {
        o.push(b' ');
    }
    o.extend_from_slice(b"\x1b[0m");
}

fn key_char(k: &Key) -> Option<char> {
    if k.kind == Kind::Text && k.mods & (CTRL | keys::SUPER | keys::HYPER | keys::META) == 0 {
        let c = k.ch?;
        Some(if k.mods & SHIFT != 0 && c.is_ascii_alphabetic() {
            c.to_ascii_uppercase()
        } else {
            c
        })
    } else {
        None
    }
}

fn session_alive(name: &str) -> bool {
    match std::fs::read_to_string(paths::pid_path(name)) {
        Ok(s) => meta::alive(s.trim().parse().unwrap_or(0)),
        Err(_) => false,
    }
}

fn parse_kitty_reply(raw: &[u8]) -> Option<u32> {
    // \x1b[?<flags>u
    if raw.len() < 4
        || raw[0] != 0x1b
        || raw[1] != b'['
        || raw[2] != b'?'
        || raw[raw.len() - 1] != b'u'
    {
        return None;
    }
    let body = std::str::from_utf8(&raw[3..raw.len() - 1]).ok()?;
    body.split(';').next()?.parse().ok()
}

fn clock() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let t = now as libc::time_t;
    let lt = unsafe { libc::localtime(&t as *const libc::time_t) };
    if lt.is_null() {
        return String::new();
    }
    unsafe { format!("{:02}:{:02}", (*lt).tm_hour, (*lt).tm_min) }
}

pub fn display_width(s: &str) -> usize {
    let mut w = 0usize;
    for c in s.chars() {
        if c == '\u{0}' {
            continue;
        }
        w += c.width().unwrap_or(0);
    }
    w
}

fn truncate(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

fn prev_char(s: &str, pos: usize) -> usize {
    let mut p = pos.saturating_sub(1);
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

fn next_char(s: &str, pos: usize) -> usize {
    let mut p = pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

/// point d'entrée: boucle d'attachement, avec bascule de session à la volée
pub fn attach(cfg: Config) -> Result<()> {
    if unsafe { libc::isatty(0) } != 1 {
        return Err(msg(
            "rscreen a besoin d'un terminal pour s'y raccorder\n\t(pour démarrer une session sans terminal : rscreen -S nom -dm -c 'commande')",
        ));
    }
    let _raw = term::RawTTY::enable(0)?;
    let mut c = Client::new(cfg)?;
    c.setup_term();
    let first = c.session.clone();
    let create = c.cfg.create;
    let win = c.cfg.first_win;
    if let Err(e) = c.connect(&first, create, win) {
        c.teardown();
        return Err(e);
    }
    let res = c.run();
    c.teardown();
    res
}

// --------------------------------------------------------------- commandes utiles

/// démarre un serveur de session sans s'y raccorder (usage `rscreen -S nom -dm -c cmd`)
pub fn spawn_background(cfg: &Config) -> Result<()> {
    if SessionMeta::load(&cfg.session).is_some() {
        return Err(msg(format!(
            "la session « {} » tourne déjà (rscreen -ls pour la voir)",
            cfg.session
        )));
    }
    let (rows, cols) = term::term_size(0).unwrap_or((24, 80));
    let exe = std::env::current_exe()?;
    let mut args: Vec<String> = vec![
        "--__server".to_string(),
        cfg.session.clone(),
        rows.max(4).to_string(),
        cols.max(12).to_string(),
        cfg.scrollback.to_string(),
    ];
    if !cfg.cmd.is_empty() {
        args.push("--cmd".to_string());
        args.push(cfg.cmd.join("\u{1}"));
    }
    if !cfg.title.is_empty() {
        args.push("--title".to_string());
        args.push(cfg.title.clone());
    }
    if std::env::var("RSCREEN_DEBUG").is_ok() {
        args.push("--log".to_string());
        args.push(
            paths::base_dir()
                .join("debug.log")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let pid = pty::spawn_detached(&exe, &args)?;
    println!(
        "session « {} » démarrée en arrière-plan (pid {pid})",
        cfg.session
    );
    // on laisse au serveur le temps de créer sa socket
    for _ in 0..60 {
        if SessionMeta::load(&cfg.session).is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err(msg("le serveur de session ne répond pas"))
}

pub fn list_sessions() -> Result<()> {
    let sessions = meta::all_sessions();
    println!("Sessions (répertoire : {})", paths::base_dir().display());
    if sessions.is_empty() {
        println!("  aucune session en cours");
        return Ok(());
    }
    for s in sessions {
        println!(
            "  {:<16} pid {:<7} {:<12} {:>3}×{:<3} {} fen.  {}",
            s.name,
            s.pid,
            if s.attached > 0 {
                "JOINTE"
            } else {
                "ARRIÈRE-PLAN"
            },
            s.rows,
            s.cols,
            s.wins.len(),
            s.wins
                .iter()
                .map(|w| format!(
                    "[{}{}: {}]",
                    w.idx,
                    if w.active { "*" } else { "" },
                    w.display()
                ))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    Ok(())
}

pub fn list_windows(name: &str) -> Result<()> {
    let s = SessionMeta::load(name).ok_or_else(|| msg(format!("pas de session « {name} »")))?;
    for w in s.wins {
        println!(
            "{}{} {}",
            w.idx,
            if w.active { "*" } else { " " },
            w.display()
        );
    }
    Ok(())
}

pub fn remote(name: &str, cmd: &str) -> Result<()> {
    let mut s = UnixStream::connect(paths::socket_path(name)).map_err(|_| {
        msg(format!(
            "aucune session « {name} » à qui envoyer cette commande"
        ))
    })?;
    let m = if let Some(rest) = cmd.strip_prefix("select:") {
        CMsg::Switch(rest.parse().unwrap_or(0))
    } else {
        match cmd {
            "next" => CMsg::Next,
            "prev" => CMsg::Prev,
            "other" => CMsg::Last,
            "new" => CMsg::New {
                cmd: vec![],
                title: String::new(),
            },
            "detach" => CMsg::Detach,
            "kill" => CMsg::KillSession,
            "redraw" => CMsg::Repaint,
            "query" => CMsg::Query,
            _ => return Err(msg(format!("commande inconnue: {cmd}"))),
        }
    };
    let mut buf = Vec::new();
    m.write_to(&mut buf);
    s.write_all(&buf)?;
    s.flush()?;
    s.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut rb = Vec::new();
    let mut last: Option<SessionMeta> = None;
    if let Ok(fl) = proto::read_frames(&mut s, &mut rb) {
        for (t, p) in fl {
            if let Ok(Some(SMsg::Meta(txt))) = SMsg::parse(t, &p) {
                if let Some(mt) = SessionMeta::decode(&txt) {
                    last = Some(mt);
                }
            }
        }
    }
    if let Some(mt) = last {
        for w in mt.wins {
            println!(
                "{}{} {}",
                w.idx,
                if w.active { "*" } else { " " },
                w.display()
            );
        }
    }
    Ok(())
}

pub fn signal_session(name: &str, kill: bool) -> Result<()> {
    let path = paths::socket_path(name);
    let mut s = UnixStream::connect(&path)
        .map_err(|_| msg(format!("aucune session « {name} » en cours")))?;
    let mut buf = Vec::new();
    (if kill {
        CMsg::KillSession
    } else {
        CMsg::Detach
    })
    .write_to(&mut buf);
    s.write_all(&buf)?;
    let _ = s.flush();
    Ok(())
}
