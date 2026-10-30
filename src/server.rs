//! Serveur de session : il tourne en arrière-plan, héberge les fenêtres
//! (chacune dans son propre pty) et envoie au(x) client(s) joint(s) le rendu
//! de la fenêtre visualisée (diffs de grille, comme tmux).

use std::os::fd::{AsRawFd, OwnedFd};
use std::rc::Rc;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::err::{msg, Result};
use crate::meta::{SessionMeta, WinMeta};
use crate::paths;
use crate::proto::{self, CMsg, SMsg, ScrollOp};
use crate::pty;
use crate::term;

#[derive(Clone)]
pub struct Opts {
    pub name: String,
    pub cmd: Vec<String>,
    pub title: String,
    pub rows: u16,
    pub cols: u16,
    pub scrollback: usize,
}

#[derive(Default)]
struct WinCb {
    bell: bool,
    title: Option<String>,
}

impl vt100::Callbacks for WinCb {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, t: &[u8]) {
        self.title = Some(String::from_utf8_lossy(t).into_owned());
    }
}

/// détecte dans la sortie de l'application les séquences du protocole clavier
/// kitty : `CSI > N u` (activer), `CSI < u` (désactiver), `CSI ? u` (requête).
#[derive(Default)]
struct KittyScan {
    st: u8,
    priv_mark: u8,
    num: u32,
    have_num: bool,
    stack: Vec<u32>,
    query: bool,
    changed: bool,
}

impl KittyScan {
    fn feed(&mut self, bytes: &[u8], flags: &mut u32) {
        for &b in bytes {
            match self.st {
                0 => {
                    if b == 0x1b {
                        self.st = 1;
                    }
                }
                1 => {
                    self.st = if b == b'[' { 2 } else { 0 };
                    self.priv_mark = 0;
                    self.num = 0;
                    self.have_num = false;
                }
                2 => match b {
                    b'0'..=b'9' => {
                        self.num = self.num.saturating_mul(10) + u32::from(b - b'0');
                        self.have_num = true;
                    }
                    b';' | b':' => {}
                    b'?' | b'<' | b'>' | b'=' => self.priv_mark = b,
                    0x20..=0x2f => {}
                    b'u' => {
                        self.st = 0;
                        match self.priv_mark {
                            b'?' if !self.have_num => self.query = true,
                            b'>' => {
                                self.stack.push(*flags);
                                *flags |= if self.have_num { self.num } else { 1 };
                                self.changed = true;
                            }
                            b'<' => {
                                *flags = self.stack.pop().unwrap_or(0);
                                self.changed = true;
                            }
                            _ => {}
                        }
                        self.priv_mark = 0;
                        self.num = 0;
                        self.have_num = false;
                    }
                    0x40..=0x7e => self.st = 0,
                    _ => self.st = 0,
                },
                _ => self.st = 0,
            }
        }
    }
}

struct Win {
    idx: u32,
    name: String,
    app_title: String,
    master: OwnedFd,
    child: libc::pid_t,
    parser: vt100::Parser<WinCb>,
    /// etat de reference de chaque vue rendue de cette fenetre : (defilement,
    /// etat d'ecran envoye aux clients de cette vue a la passe precedente).
    shots: Vec<(usize, Rc<vt100::Screen>)>,
    kitty: u32,
    scan: KittyScan,
}

/// Les clients d'une meme vue (fenetre + defilement) recoivent la meme trame.
struct Group {
    wi: usize,
    scroll: usize,
    /// etat de reference de la vue, laisse par la passe precedente
    base: Option<Rc<vt100::Screen>>,
    /// clients deja a jour de `base` : ils recoivent le diff
    sync: Vec<usize>,
    /// clients en retard : ils recoivent un redraw total
    lagging: Vec<usize>,
}

/// Une vue rendue : la trame toute encodee pour les clients a jour (`diff`),
/// celle pour les clients en retard (`full`), et l'etat que l'une ou l'autre
/// produit a l'ecran (la reference de la prochaine passe).
struct View {
    diff: Vec<u8>,
    full: Vec<u8>,
    snap: Rc<vt100::Screen>,
}

/// une trame `Data` vide ne part pas sur le socket
fn encode(txt: String) -> Vec<u8> {
    if txt.is_empty() {
        return Vec::new();
    }
    let mut payload = Vec::with_capacity(txt.len() + 5);
    SMsg::Data(txt.into_bytes()).write_to(&mut payload);
    payload
}

struct Client {
    sock: UnixStream,
    writer: UnixStream,
    rbuf: Vec<u8>,
    out: Vec<u8>,
    prev: Option<Rc<vt100::Screen>>,
    need_full: bool,
    viewing: u32,
    scroll: usize,
    kitty_flags: u32,
    alive: bool,
    /// le terminal du client a disparu (écriture en EPIPE) : on ne lui envoie
    /// plus rien, mais on lit malgré tout ce qu'il a pu laisser dans le socket
    silent: bool,
}

pub struct Server {
    opts: Opts,
    listener: UnixListener,
    wins: Vec<Win>,
    clients: Vec<Client>,
    zombies: Vec<libc::pid_t>,
    active: u32,
    last_active: u32,
    next_idx: u32,
    rows: u16,
    cols: u16,
    render_needed: bool,
    meta_dirty: bool,
    created: i64,
}

/// trace de debug (RSCREEN_DEBUG=1)
fn dbgln(x: &str) {
    if std::env::var("RSCREEN_DEBUG").is_ok() {
        eprintln!("[rscreen serveur] {x}");
    }
}

fn nz(v: u16) -> core::num::NonZeroU16 {
    core::num::NonZeroU16::new(v.max(1)).unwrap()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn base_name(cmd: &[String]) -> String {
    cmd.first()
        .map(|s| {
            let b = s.rsplit('/').next().unwrap_or(s).to_string();
            // shell de connexion: "-bash" -> "bash"
            b.trim_start_matches('-').to_string()
        })
        .unwrap_or_default()
}

impl Server {
    pub fn new(opts: Opts) -> Result<Server> {
        let dir = paths::ensure_session_dir(&opts.name)?;
        let sockp = dir.join("sock");
        if let Ok(md) = std::fs::metadata(&sockp) {
            if md.file_type().is_socket() {
                if UnixStream::connect(&sockp).is_ok() {
                    let pid = std::fs::read_to_string(dir.join("pid"))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    return Err(msg(format!(
                        "une session « {} » tourne déjà (pid {pid}).\n\
                         \t pour s'y raccorder : rscreen -r {n}\n\
                         \t pour la tuer       : rscreen -S {n} -x",
                        opts.name,
                        n = opts.name
                    )));
                }
                let _ = std::fs::remove_file(&sockp);
            }
        }
        let listener = UnixListener::bind(&sockp)?;
        listener.set_nonblocking(true)?;
        std::fs::write(dir.join("pid"), format!("{}\n", std::process::id()))?;
        Ok(Server {
            opts: opts.clone(),
            listener,
            wins: Vec::new(),
            clients: Vec::new(),
            zombies: Vec::new(),
            active: 0,
            last_active: 0,
            next_idx: 0,
            rows: opts.rows.max(4),
            cols: opts.cols.max(12),
            render_needed: true,
            meta_dirty: true,
            created: now(),
        })
    }

    fn winpos(&self, idx: u32) -> Option<usize> {
        self.wins.iter().position(|w| w.idx == idx)
    }

    fn add_window(&mut self, cmd: Vec<String>, title: String) -> Result<u32> {
        let cmd = if cmd.is_empty() {
            pty::default_shell_cmd()
        } else {
            cmd
        };
        let envs = vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("LINES".to_string(), self.rows.to_string()),
            ("COLUMNS".to_string(), self.cols.to_string()),
            ("STY".to_string(), self.opts.name.clone()),
            ("RSCREEN_SESSION".to_string(), self.opts.name.clone()),
            ("RSCREEN_PID".to_string(), std::process::id().to_string()),
        ];
        let p = match pty::spawn_in_pty(&cmd, &envs, self.rows, self.cols) {
            Ok(p) => p,
            Err(e) => {
                let t = format!("impossible de lancer « {} »: {e}", cmd.join(" "));
                for c in &mut self.clients {
                    SMsg::Msg(t.clone()).write_to(&mut c.out);
                }
                return Err(e.into());
            }
        };
        let idx = self.next_idx;
        self.next_idx += 1;
        let name = if title.is_empty() {
            let n = base_name(&cmd);
            if n.is_empty() {
                p.tty.clone()
            } else {
                n
            }
        } else {
            title
        };
        self.wins.push(Win {
            idx,
            name,
            app_title: String::new(),
            master: p.master,
            child: p.pid,
            parser: vt100::Parser::new_with_callbacks(
                nz(self.rows),
                nz(self.cols),
                self.opts.scrollback,
                WinCb::default(),
            ),
            kitty: 0,
            shots: Vec::new(),
            scan: KittyScan::default(),
        });
        if self.wins.len() == 1 {
            self.active = idx;
        } else {
            // la nouvelle fenetre devient la fenetre courante (comme screen)
            self.last_active = self.active;
            self.active = idx;
            for c in &mut self.clients {
                c.viewing = idx;
                c.scroll = 0;
            }
        }
        dbgln(&format!(
            "fenêtre {idx} créée : pid {} cmd «{}»",
            self.wins[self.wins.len() - 1].child,
            cmd.join(" ")
        ));
        self.render_needed = true;
        self.meta_dirty = true;
        Ok(idx)
    }

    fn kill_window(&mut self, idx: u32) {
        let i = match self.winpos(idx) {
            Some(i) => i,
            None => return,
        };
        let w = self.wins.remove(i);
        dbgln(&format!("fenêtre {idx} fermée (pid {})", w.child));
        unsafe {
            libc::kill(w.child, libc::SIGHUP);
        }
        self.zombies.push(w.child);
        self.meta_dirty = true;
        self.render_needed = true;
        if self.wins.is_empty() {
            return;
        }
        let pick = self
            .wins
            .iter()
            .min_by_key(|w| (w.idx as i64 - idx as i64).abs())
            .map(|w| w.idx)
            .unwrap_or(self.wins[0].idx);
        self.last_active = idx;
        self.active = pick;
        for c in &mut self.clients {
            if c.viewing == idx {
                c.viewing = pick;
                c.need_full = true;
                c.scroll = 0;
            }
        }
    }

    fn select(&mut self, idx: u32) {
        if self.winpos(idx).is_none() || self.active == idx {
            return;
        }
        self.last_active = self.active;
        self.active = idx;
        for c in &mut self.clients {
            c.viewing = idx;
            c.scroll = 0;
        }
        self.meta_dirty = true;
        self.render_needed = true;
    }

    fn cycle(&mut self, dir: i32) {
        if self.wins.len() < 2 {
            return;
        }
        let cur = self.winpos(self.active).unwrap_or(0);
        let n = self.wins.len();
        let next = if dir > 0 {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        let idx = self.wins[next].idx;
        self.select(idx);
    }

    fn go_last(&mut self) {
        if self.winpos(self.last_active).is_none() {
            return;
        }
        std::mem::swap(&mut self.last_active, &mut self.active);
        let t = self.active;
        for c in &mut self.clients {
            c.viewing = t;
            c.scroll = 0;
        }
        self.meta_dirty = true;
        self.render_needed = true;
    }

    fn set_size(&mut self, rows: u16, cols: u16) {
        let rows = rows.clamp(2, 512);
        let cols = cols.clamp(4, 2048);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        for w in &mut self.wins {
            let _ = pty::set_pty_size(w.master.as_raw_fd(), rows, cols);
            w.parser.set_size(nz(rows), nz(cols));
        }
        for c in &mut self.clients {
            c.need_full = true;
        }
        self.meta_dirty = true;
        self.render_needed = true;
    }

    fn meta(&self) -> SessionMeta {
        SessionMeta {
            name: self.opts.name.clone(),
            pid: std::process::id() as i32,
            created: self.created,
            rows: self.rows,
            cols: self.cols,
            attached: self.clients.iter().filter(|c| c.alive).count(),
            shell: pty::shell_name(),
            wins: self
                .wins
                .iter()
                .map(|w| WinMeta {
                    idx: w.idx,
                    active: w.idx == self.active,
                    name: w.name.clone(),
                    title: w.app_title.clone(),
                    pid: w.child,
                    app_cursor: w.parser.screen().application_cursor(),
                    kitty: w.kitty,
                })
                .collect(),
        }
    }

    fn publish_meta(&mut self) {
        let enc = self.meta().encode();
        for c in &mut self.clients {
            if c.alive {
                SMsg::Meta(enc.clone()).write_to(&mut c.out);
            }
        }
        let _ = self.meta().save();
    }

    fn accept(&mut self) {
        let (sock, _) = match self.listener.accept() {
            Ok(x) => x,
            Err(e) => {
                if e.raw_os_error() != Some(libc::EAGAIN)
                    && e.raw_os_error() != Some(libc::EWOULDBLOCK)
                {
                    eprintln!("rscreen serveur: accept: {e}");
                }
                return;
            }
        };
        let _ = sock.set_write_timeout(Some(Duration::from_secs(5)));
        let writer = match sock.try_clone() {
            Ok(w) => w,
            Err(_) => return,
        };
        let mut c = Client {
            sock,
            writer,
            rbuf: Vec::new(),
            out: Vec::new(),
            prev: None,
            need_full: true,
            viewing: self.active,
            scroll: 0,
            kitty_flags: 0,
            alive: true,
            silent: false,
        };
        SMsg::Meta(self.meta().encode()).write_to(&mut c.out);
        SMsg::Msg(format!("session « {} » jointe", self.opts.name)).write_to(&mut c.out);
        self.clients.push(c);
        dbgln("client raccordé à la session");
        self.meta_dirty = true;
        self.render_needed = true;
    }

    /// lit la sortie d'une fenêtre et met à jour son émulation
    fn pump_window(&mut self, wi: usize) {
        let mut buf = [0u8; 64 * 1024];
        let fd = self.wins[wi].master.as_raw_fd();
        let idx = self.wins[wi].idx;
        let r = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if r == 0 {
            self.kill_window(idx);
            return;
        }
        if r < 0 {
            let e = std::io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EINTR) => return,
                // EIO: le processus est sorti
                _ => {
                    self.kill_window(idx);
                    return;
                }
            }
        }
        let n = r as usize;
        {
            let w = &mut self.wins[wi];
            w.scan.feed(&buf[..n], &mut w.kitty);
            w.parser.process(&buf[..n]);
        }
        let title = self.wins[wi].parser.callbacks().title.clone();
        if let Some(t) = title {
            self.wins[wi].app_title = t.clone();
            self.wins[wi].parser.callbacks_mut().title = None;
            self.meta_dirty = true;
            for c in &mut self.clients {
                SMsg::Title {
                    idx,
                    title: t.clone(),
                }
                .write_to(&mut c.out);
            }
        }
        if self.wins[wi].scan.changed {
            self.wins[wi].scan.changed = false;
            let f = self.wins[wi].kitty;
            for c in &mut self.clients {
                if c.viewing == idx {
                    SMsg::Kitty(f).write_to(&mut c.out);
                }
            }
        }
        if self.wins[wi].scan.query {
            self.wins[wi].scan.query = false;
            let flags = self
                .clients
                .iter()
                .map(|c| c.kitty_flags)
                .max()
                .unwrap_or(0);
            let ans = format!("\x1b[?{flags}u");
            let fd = self.wins[wi].master.as_raw_fd();
            unsafe {
                libc::write(fd, ans.as_ptr() as *const libc::c_void, ans.len());
            }
        }
        if self.wins[wi].parser.callbacks().bell {
            self.wins[wi].parser.callbacks_mut().bell = false;
            for c in &mut self.clients {
                if c.viewing == idx {
                    SMsg::Bell.write_to(&mut c.out);
                }
            }
        }
        self.render_needed = true;
    }

    fn render(&mut self) {
        // Une vue = (fenetre, degre de defilement). Tous les clients d'une vue
        // doivent recevoir exactement la meme trame : clone d'ecran, diff et
        // encodage ne sont donc faits qu'une fois par vue et par passe, au lieu
        // d'une fois par client. Le clone d'un `Screen` copie les deux grilles
        // ET tout le scrollback (~10 Mo et ~2000 `Vec` avec -H 2000) pour
        // n'afficher que 48x160 cellules (245 Ko) : ce cout etait jusqu'ici
        // multiplie par le nombre de clients joints.
        //
        // `Win::shots` conserve l'etat de reference de chaque vue : ce que la
        // passe precedente a envoye a *tous* les clients de cette vue. Un
        // client dont le « prev » est ce meme objet (`Rc::ptr_eq`, temoin
        // d'identite et non de contenu) est a jour : il recoit le diff. Un
        // client en retard (nouvelle venue, changement de fenetre, de taille,
        // de defilement, ou simple desynchronisation) recoit un redraw total :
        // comme les deux trames produisent le meme etat, la vue se converge
        // d'elle-meme et tout le monde repart du meme instantane a la passe
        // suivante.
        let mut groups: Vec<Group> = Vec::new();
        for ci in 0..self.clients.len() {
            if !self.clients[ci].alive {
                continue;
            }
            let viewing = self.clients[ci].viewing;
            let wi = match self.winpos(viewing) {
                Some(w) => w,
                None => continue,
            };
            let scroll = self.clients[ci].scroll;
            let gi = match groups.iter().position(|g| g.wi == wi && g.scroll == scroll) {
                Some(i) => i,
                None => {
                    let base = self.wins[wi]
                        .shots
                        .iter()
                        .find(|(s, _)| *s == scroll)
                        .map(|(_, p)| p.clone());
                    groups.push(Group {
                        wi,
                        scroll,
                        base,
                        sync: Vec::new(),
                        lagging: Vec::new(),
                    });
                    groups.len() - 1
                }
            };
            let uptodate = match (&self.clients[ci].prev, &groups[gi].base) {
                (Some(p), Some(b)) => Rc::ptr_eq(p, b),
                _ => false,
            };
            if self.clients[ci].need_full || !uptodate {
                groups[gi].lagging.push(ci);
            } else {
                groups[gi].sync.push(ci);
            }
        }
        // des qu'aucun client ne regarde une fenetre (ou un defilement donne),
        // sa reference est lachee : c'est ce qui rend l'historique a un gros
        // scrollback quand la session passe en arriere-plan.
        for w in self.wins.iter_mut() {
            w.shots.clear();
        }
        for g in groups.iter() {
            let view = self.render_view(
                g.wi,
                g.base.as_deref(),
                g.scroll,
                !g.sync.is_empty(),
                !g.lagging.is_empty(),
            );
            let View { diff, full, snap } = view;
            for (buf, list) in [(&diff, &g.sync), (&full, &g.lagging)] {
                for &ci in list.iter() {
                    let c = &mut self.clients[ci];
                    if !buf.is_empty() {
                        c.out.extend_from_slice(buf);
                    }
                    c.prev = Some(snap.clone());
                    c.need_full = false;
                }
            }
            self.wins[g.wi].shots.push((g.scroll, snap));
        }
        self.render_needed = false;
    }

    /// la trame d'une vue, calculee depuis `base` (None = repartir d'un ecran
    /// vide) vers l'etat courant de la fenetre `wi` au defilement `scroll`.
    /// `want_diff`/`want_full` evitent de calculer une trame que personne ne
    /// demande, et les deux aboutissent au meme `snap` partage par `Rc`.
    fn render_view(
        &mut self,
        wi: usize,
        base: Option<&vt100::Screen>,
        scroll: usize,
        want_diff: bool,
        want_full: bool,
    ) -> View {
        if scroll > 0 {
            let len = self.wins[wi].parser.screen().scrollback_len();
            self.wins[wi]
                .parser
                .screen_mut()
                .set_scrollback(len.min(scroll));
        }
        let (diff, full, snap) = {
            let s = self.wins[wi].parser.screen();
            let (rows, cols) = s.size();
            let mut diff = String::new();
            let mut full = String::new();
            if want_diff {
                if let Some(prev) = base {
                    diff.push_str(&s.state_diff(prev));
                }
            }
            if want_full {
                let blank = vt100::Parser::new(rows, cols, 0);
                full.push_str("\x1b[0m\x1b[39;49m\x1b[?7h\x1b[H\x1b[2J");
                full.push_str(&s.state_diff(blank.screen()));
                // un ecran vide est cense avoir le curseur visible : le diff
                // ne dirait jamais rien si l'app le cache. On impose etat.
                full.push_str(if s.hide_cursor() {
                    "\x1b[?25l"
                } else {
                    "\x1b[?25h"
                });
            }
            (
                encode(diff),
                encode(full),
                Rc::new(s.clone()),
            )
        };
        if scroll > 0 {
            self.wins[wi].parser.screen_mut().set_scrollback(0);
        }
        View { diff, full, snap }
    }

    /// rebaptiser la session : son répertoire devient son nouveau nom. Le
    /// socket suit son répertoire (un `rename` garde le même inode), donc les
    /// clients déjà branchés vivent toujours et les nouveaux se branchent sur
    /// le nouveau chemin. Les fenêtres déjà ouvertes gardent leur `STY`
    /// hérité, c'est cosmétique.
    fn rename_session(&mut self, want: &str) {
        let from = self.opts.name.clone();
        let to = paths::sanitize(want);
        if want.trim().is_empty() {
            self.msg_all("nom de session vide : renommage annulé");
            return;
        }
        if to == from {
            self.msg_all(format!("session « {from} » déjà sous ce nom"));
            return;
        }
        if std::fs::metadata(paths::session_dir(&to)).is_ok() {
            self.msg_all(format!("« {to} » porte déjà le nom d'une session"));
            return;
        }
        if let Err(e) = std::fs::rename(paths::session_dir(&from), paths::session_dir(&to)) {
            self.msg_all(format!("renommage impossible : {e}"));
            return;
        }
        self.opts.name = to.clone();
        // le fichier de pid suit le répertoire, on le repeint quand même
        let _ = std::fs::write(paths::pid_path(&to), format!("{}\n", std::process::id()));
        self.meta_dirty = true;
        self.msg_all(format!("session « {from} » renommée « {to} »"));
    }

    /// un message court pour tous les clients joints
    fn msg_all(&mut self, s: impl Into<String>) {
        let t = s.into();
        for c in &mut self.clients {
            if c.alive {
                SMsg::Msg(t.clone()).write_to(&mut c.out);
            }
        }
    }

    fn flush_clients(&mut self) {
        for i in 0..self.clients.len() {
            let c = &mut self.clients[i];
            if !c.alive || c.out.is_empty() || c.silent {
                c.out.clear();
                continue;
            }
            if let Err(e) = proto::write_all_frames(&mut c.writer, &mut c.out) {
                dbgln(&format!("écriture client perdue ({e}) — on draine ses messages"));
                c.silent = true;
                c.out.clear();
            }
        }
        let before = self.clients.len();
        self.clients.retain(|c| c.alive);
        if self.clients.len() != before {
            self.meta_dirty = true;
        }
    }

    fn handle_client_msg(&mut self, ci: usize, m: CMsg) {
        match m {
            CMsg::Input(b) => {
                let viewing = self.clients[ci].viewing;
                if let Some(wi) = self.winpos(viewing) {
                    let fd = self.wins[wi].master.as_raw_fd();
                    let mut off = 0usize;
                    while off < b.len() {
                        let r = unsafe {
                            libc::write(fd, b[off..].as_ptr() as *const libc::c_void, b.len() - off)
                        };
                        if r <= 0 {
                            break;
                        }
                        off += r as usize;
                    }
                }
            }
            CMsg::Resize { rows, cols } => self.set_size(rows, cols),
            CMsg::Switch(i) => self.select(i),
            CMsg::Next => self.cycle(1),
            CMsg::Prev => self.cycle(-1),
            CMsg::Last => self.go_last(),
            CMsg::New { cmd, title } => {
                let _ = self.add_window(cmd, title);
            }
            CMsg::Kill(i) => self.kill_window(i),
            CMsg::Detach => {
                self.flush_clients();
                for c in &mut self.clients {
                    c.alive = false;
                }
                self.clients.clear();
                self.meta_dirty = true;
            }
            CMsg::KillSession => {
                self.flush_clients();
                for c in &mut self.clients {
                    c.alive = false;
                }
                self.clients.clear();
                let idxs: Vec<u32> = self.wins.iter().map(|w| w.idx).collect();
                for i in idxs {
                    self.kill_window(i);
                }
            }
            CMsg::Rename { idx, name } => {
                if let Some(wi) = self.winpos(idx) {
                    self.wins[wi].name = name;
                    self.wins[wi].app_title = String::new();
                    self.meta_dirty = true;
                }
            }
            CMsg::RenameSession { name } => self.rename_session(&name),
            CMsg::Repaint => {
                if ci < self.clients.len() {
                    self.clients[ci].need_full = true;
                    self.render_needed = true;
                }
            }
            CMsg::Scroll { op, amount } => {
                let viewing = self.clients[ci].viewing;
                let wi = match self.winpos(viewing) {
                    Some(w) => w,
                    None => return,
                };
                let len = self.wins[wi].parser.screen().scrollback_len();
                let c = &mut self.clients[ci];
                let old = c.scroll;
                c.scroll = match op {
                    ScrollOp::Rel => (c.scroll as i64 + amount).clamp(0, len as i64) as usize,
                    ScrollOp::Set => amount.clamp(0, len as i64) as usize,
                    ScrollOp::Top => len,
                    ScrollOp::Bottom => 0,
                };
                if c.scroll != old {
                    c.need_full = true;
                    self.render_needed = true;
                }
            }
            CMsg::Query => {
                let enc = self.meta().encode();
                SMsg::Meta(enc).write_to(&mut self.clients[ci].out);
            }
            CMsg::KittyFlags(f) => {
                self.clients[ci].kitty_flags = f;
            }
        }
    }

    /// lit les trames arrivées d'un client ; marque le client mort si besoin
    /// (la session, elle, continue en arrière-plan)
    fn on_client_event(&mut self, idx: usize) {
        let frames = {
            let c = &mut self.clients[idx];
            proto::read_frames(&mut c.sock, &mut c.rbuf)
        };
        let fl = match frames {
            Ok(fl) => fl,
            Err(_) => {
                self.drop_client(idx);
                return;
            }
        };
        if fl.is_empty() {
            // simple déconnexion : la session, elle, continue en arrière-plan
            self.drop_client(idx);
            return;
        }
        if self.clients[idx].silent {
            dbgln("message reçu d'un client déjà parti");
        }
        for (t, p) in fl {
            match CMsg::parse(t, &p) {
                Ok(Some(m)) => self.handle_client_msg(idx, m),
                Ok(None) => {}
                Err(_) => {
                    self.drop_client(idx);
                    return;
                }
            }
            // la commande a pu fermer la session : le client n'existe plus
            if idx >= self.clients.len() {
                break;
            }
        }
    }

    fn drop_client(&mut self, idx: usize) {
        if idx < self.clients.len() {
            self.clients[idx].alive = false;
        }
        dbgln("client déconnecté — la session reste en arrière-plan");
    }

    fn reap(&mut self) {
        if self.zombies.is_empty() {
            return;
        }
        let mut st = 0;
        self.zombies.retain(|pid| {
            let r = unsafe { libc::waitpid(*pid, &mut st, libc::WNOHANG) };
            r == 0
        });
    }

    pub fn run(&mut self) -> Result<()> {
        term::ignore_signal(libc::SIGHUP);
        term::ignore_signal(libc::SIGPIPE);
        self.add_window(self.opts.cmd.clone(), self.opts.title.clone())?;
        self.publish_meta();

        let mut poller = term::Poller::default();
        let mut last_render = Instant::now() - Duration::from_millis(100);

        while !self.wins.is_empty() {
            let timeout = if self.render_needed {
                let since = last_render.elapsed();
                let want = Duration::from_millis(11);
                if since >= want {
                    0
                } else {
                    (want - since).as_millis() as i32
                }
            } else {
                300
            };

            poller.clear();
            poller.add(self.listener.as_raw_fd(), term::TOK_LISTEN, 0);
            for (i, c) in self.clients.iter().enumerate() {
                if c.alive {
                    poller.add(c.sock.as_raw_fd(), term::TOK_CLIENT, i);
                }
            }
            for (i, w) in self.wins.iter().enumerate() {
                poller.add(w.master.as_raw_fd(), term::TOK_WINDOW, i);
            }
            let ready = poller.wait(timeout)?;

            for (tok, idx, _rev) in ready {
                match tok {
                    term::TOK_LISTEN => self.accept(),
                    term::TOK_WINDOW => self.pump_window(idx),
                    term::TOK_CLIENT if idx < self.clients.len() => self.on_client_event(idx),
                    _ => {}
                }
            }
            let nclients = self.clients.len();
            self.clients.retain(|c| c.alive);
            if self.clients.len() != nclients {
                // le fichier d'état doit refléter la session en arrière-plan
                self.meta_dirty = true;
            }
            self.reap();

            if self.render_needed {
                self.render();
                last_render = Instant::now();
            }
            if self.meta_dirty {
                self.publish_meta();
                self.meta_dirty = false;
            }
            self.flush_clients();
        }

        for w in &self.wins {
            unsafe {
                libc::kill(w.child, libc::SIGHUP);
            }
        }
        self.flush_clients();
        self.clients.retain(|c| c.alive);
        for c in &mut self.clients {
            SMsg::Bye.write_to(&mut c.out);
        }
        self.flush_clients();
        paths::cleanup_session_dir(&self.opts.name);
        Ok(())
    }
}
