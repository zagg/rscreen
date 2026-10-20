//! rscreen — multiplexeur de terminal façon gnu-screen, écrit en Rust.
//!
//! sessions -> serveurs en arrière-plan, chacun hébergeant des *fenêtres*
//! (des shells dans des ptys). `Tab` change de fenêtre, `Ctrl+Tab` ouvre la
//! fenêtre de sélection des sessions (avec bascule en arrière-plan).

mod client;
mod err;
mod keys;
mod meta;
mod paths;
mod proto;
mod pty;
mod server;
mod term;

use std::process::ExitCode;

use crate::err::{msg, Result};

const VERSION: &str = concat!("0.1.0 (", env!("CARGO_PKG_NAME"), ")");

fn usage() {
    println!(
        "rscreen {v} — terminal multiplexer (fenêtres + sessions), façon screen\n\n",
        v = VERSION
    );
    println!(
        "usage : rscreen [options] [commande]\n\
         \n\
         \t-d                détacher la session (la laisser en arrière-plan)\n\
         \t-D                déconnecter les autres clients puis se raccorder\n\
         \t-ls               lister les sessions\n\
         \t-L                lister les fenêtres d'une session\n\
         \t-r                se raccorder à une session existante (sans en créer)\n\
         \t-S NOM            nom de la session (défaut : « {} »)\n\
         \t-c COMMANDE       commande lancée dans la première fenêtre\n\
         \t-t TITRE          titre de la première fenêtre\n\
         \t-x                tuer la session\n\
         \t-X COMMANDE       envoyer une commande à la session (next prev other new\n\
         \t                  select:N detach kill redraw query)\n\
         \t-R                reprendre ou créer la session\n\
         \t-H                historique par fenêtre (défaut 2000 lignes)\n\
         \t-h, --help        cette aide\n\
         \t-V                version\n\
         \n\
         \tpendant la session :\n\
         \t  Tab / Maj+Tab     fenêtre suivante / précédente\n\
         \t  Ctrl+Tab          sélecteur de sessions : Entrée = basculer sur la\n\
         \t                    session (ou la fenêtre) choisie, ou rester en\n\
         \t                    arrière-plan\n\
         \t  Ctrl+Entrée       détacher (arrière-plan)\n\
         \t  Ctrl+a            préfixe à la gnu-screen : a c C k x d D n p 0-9 w \" r [ l t ? h\n\
         \n\
         \texemples :\n\
         \t  rscreen -S dev              session « dev », 1 fenêtre shell\n\
         \t  rscreen -S dev -r           s'y raccorder (une autre fenêtre ouverte)\n\
         \t  rscreen -ls                 voir toutes les sessions\n\
         \t  rscreen -S build -c 'make -j8'\n",
        paths::DEFAULT_SESSION
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // si stdout n'est pas un tty, le terminal a déjà été restauré
            eprintln!("rscreen: {e}");
            ExitCode::from(1)
        }
    }
}

struct Opts {
    session: String,
    cmd: Vec<String>,
    title: String,
    list: bool,
    list_win: bool,
    resume: bool,
    detach: bool,
    detach_then_attach: bool,
    kill: bool,
    remote: Option<String>,
    scrollback: usize,
    positional: Vec<String>,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            session: std::env::var("RSCREEN_SESSION")
                .or_else(|_| std::env::var("STY"))
                .unwrap_or_else(|_| paths::DEFAULT_SESSION.to_string()),
            cmd: Vec::new(),
            title: String::new(),
            list: false,
            list_win: false,
            resume: false,
            detach: false,
            detach_then_attach: false,
            kill: false,
            remote: None,
            scrollback: 2000,
            positional: Vec::new(),
        }
    }
}

fn dispatch(args: &[String]) -> Result<()> {
    // mode serveur caché : rscreen --__server NOM ROWS COLS SCROLLBACK [--cmd CMD] [--title T]
    if args.first().map(|s| s.as_str()) == Some("--__server") {
        return run_server_hidden(args);
    }
    if args.iter().any(|a| a == "--help" || a == "-help") {
        usage();
        return Ok(());
    }

    let mut o = Opts::default();
    let mut i = 0usize;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            o.positional.extend(args[i + 1..].iter().cloned());
            break;
        }
        if a.starts_with("--") {
            match a.as_str() {
                "--ls" | "--list" => o.list = true,
                "--version" => {
                    println!("rscreen {VERSION}");
                    return Ok(());
                }
                "--kill" => o.kill = true,
                "--detach" => o.detach = true,
                "--attach" => {}
                other => return Err(msg(format!("option inconnue: {other}"))),
            }
            i += 1;
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            let chars: Vec<char> = a.chars().skip(1).collect();
            let mut j = 0usize;
            while j < chars.len() {
                let c = chars[j];
                // -ls et -list
                if c == 'l' && j + 1 < chars.len() && chars[j + 1] == 's' {
                    o.list = true;
                    j += 2;
                    continue;
                }
                // valeur collée (-Sdev) ou argument suivant (-S dev)
                let takes_value = matches!(c, 'S' | 'c' | 't' | 'X' | 'H');
                let value: Option<String> = if !takes_value {
                    None
                } else if j + 1 < chars.len() {
                    let v: String = chars[j + 1..].iter().collect();
                    j = chars.len();
                    Some(v)
                } else if i + 1 < args.len() {
                    i += 1;
                    Some(args[i].clone())
                } else {
                    return Err(msg(format!("l'option -{c} attend une valeur")));
                };
                let _ = &value;
                match c {
                    'S' => o.session = value.unwrap(),
                    'c' => o.cmd = pty::split_command(&value.unwrap()),
                    't' => o.title = value.unwrap(),
                    'X' => o.remote = Some(value.unwrap()),
                    'H' => {
                        o.scrollback = value.unwrap().parse().unwrap_or(2000usize);
                    }
                    'l' => o.list_win = true,
                    'L' => o.list_win = true,
                    's' => o.list = true,
                    'r' => o.resume = true,
                    'd' => o.detach = true,
                    'D' => o.detach_then_attach = true,
                    'R' => o.resume = true,
                    'x' => o.kill = true,
                    'V' => {
                        println!("rscreen {VERSION}");
                        return Ok(());
                    }
                    'h' => {
                        usage();
                        return Ok(());
                    }
                    other => return Err(msg(format!("option inconnue: -{other} (rscreen -h)"))),
                }
                j += 1;
            }
            i += 1;
            continue;
        }
        o.positional.push(a.clone());
        i += 1;
    }

    if o.cmd.is_empty() && !o.positional.is_empty() {
        o.cmd = o.positional.clone();
    }

    let session = paths::sanitize(&o.session);

    if o.list {
        return client::list_sessions();
    }
    if o.list_win {
        return client::list_windows(&session);
    }
    if o.kill {
        return client::signal_session(&session, true);
    }
    if o.detach && !o.detach_then_attach {
        return client::signal_session(&session, false);
    }
    if let Some(cmd) = &o.remote {
        return client::remote(&session, cmd);
    }

    let cfg = client::Config {
        session: session.clone(),
        cmd: o.cmd.clone(),
        title: o.title.clone(),
        create: !o.resume,
        scrollback: o.scrollback,
        first_win: None,
    };
    if o.detach_then_attach {
        let _ = client::signal_session(&session, false);
    }
    client::attach(cfg)
}

fn run_server_hidden(args: &[String]) -> Result<()> {
    if args.len() < 5 {
        return Err(msg(
            "usage interne: rscreen --__server NOM ROWS COLS SCROLLBACK",
        ));
    }
    let name = paths::sanitize(&args[1]);
    let rows: u16 = args[2].parse().unwrap_or(24);
    let cols: u16 = args[3].parse().unwrap_or(80);
    let scrollback: usize = args[4].parse().unwrap_or(2000);
    let mut cmd: Vec<String> = Vec::new();
    let mut title = String::new();
    let mut i = 5usize;
    while i < args.len() {
        match args[i].as_str() {
            "--cmd" => {
                let v = args.get(i + 1).cloned().unwrap_or_default();
                cmd = v
                    .split('\u{1}')
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                i += 1;
            }
            "--title" => {
                title = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    let opts = server::Opts {
        name: name.clone(),
        cmd,
        title,
        rows,
        cols,
        scrollback,
    };
    let mut srv = server::Server::new(opts)?;
    srv.run()
}
