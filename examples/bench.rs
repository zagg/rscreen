//! chemin chaud du serveur : le cout d'une PASSE DE RENDU, qui se fait
//! desormais par *vue* (fenetre + defilement) et non plus par client, et la
//! part qu'y prend le scrollback copie a chaque instantane.
//!
//! `A` = ce que fait render_view() aujourd'hui (diff + `Screen::clone`, qui
//! copie les deux grilles ET tout l'historique). `B` = la prochaine etape : un
//! instantane « visible uniquement » dans un `Vec<Cell>` plat pre-aloue, dont
//! le cout ne depend plus de la taille de l'historique.
//!
//!   cargo run --release --example bench

use std::hint::black_box;
use std::num::NonZeroU16;
use std::time::Instant;

const N: u32 = 300;

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap()
}

fn mk(rows: u16, cols: u16, sb: usize) -> vt100::Parser {
    let mut p = vt100::Parser::new(nz(rows), nz(cols), sb);
    for i in 0..(rows as usize + sb) {
        p.process(format!("ligne {i} : {}\r\n", "x".repeat(120)).as_bytes());
    }
    p
}

/// A — ce que fait render() aujourd'hui : diff contre un Screen clone,
/// puis re-clone integral (grille + grille auxilliaire + scrollback).
fn today(rows: u16, cols: u16, sb: usize) -> f64 {
    let mut p = mk(rows, cols, sb);
    let mut prev: Option<vt100::Screen> = None;
    let t = Instant::now();
    for i in 0..N {
        p.process(format!("tick {i}\r\n").as_bytes());
        let s = p.screen();
        let txt = match prev.as_ref() {
            Some(q) => s.state_diff(q),
            None => s.contents_formatted(),
        };
        let snap = s.clone();
        black_box(txt);
        black_box(&snap);
        prev = Some(snap);
    }
    t.elapsed().as_secs_f64() * 1000.0 / N as f64
}

/// B — meme chose, mais le « prev » ne garde que la grille visible dans un
/// Vec plate pre-aloue : ni clone du scrollback, ni une allocation par ligne.
fn compact(rows: u16, cols: u16, sb: usize) -> f64 {
    let mut p = mk(rows, cols, sb);
    let blank = p.screen().cell(0, 0).cloned().unwrap();
    let mut prev: Vec<vt100::Cell> = vec![blank.clone(); rows as usize * cols as usize];
    let mut cur: Vec<vt100::Cell> = prev.clone();
    let mut txt = String::new();
    let t = Instant::now();
    for i in 0..N {
        p.process(format!("tick {i}\r\n").as_bytes());
        let s = p.screen();
        black_box(s.hide_cursor());
        for (k, (r, c)) in (0..rows).map(|r| (0..cols).map(move |c| (r, c))).flatten().enumerate() {
            cur[k] = s.cell(r, c).cloned().unwrap_or_else(|| blank.clone());
        }
        txt.clear();
        for k in 0..prev.len() {
            if cur[k] != prev[k] {
                // une sequence par cellule changee, comme le ferait un vrai diff
                let c = &cur[k];
                txt.push_str("\x1b[");
                txt.push_str("0m");
                black_box(c.bold());
                black_box(c.fgcolor());
                txt.push_str(c.contents());
            }
        }
        black_box(&txt);
        std::mem::swap(&mut prev, &mut cur);
    }
    t.elapsed().as_secs_f64() * 1000.0 / N as f64
}

fn main() {
    println!(
        "une Cell = {} octets ; un Screen clone = 2 grilles + tout le scrollback\n",
        std::mem::size_of::<vt100::Cell>()
    );
    println!(
        "{}",
        "                scrollback      0      100     1000     2000    10000"
    );
    let mut line_a = String::from("  A  aujourd'hui (Screen::clone)");
    let mut line_b = String::from("  B  snapshot compact (Vec<Cell>)");
    for sb in [0usize, 100, 1000, 2000, 10000] {
        line_a.push_str(&format!("{:>9.3}", today(48, 160, sb)));
        line_b.push_str(&format!("{:>9.3}", compact(48, 160, sb)));
    }
    println!("ecran 48x160, ms par frame :\n{line_a}\n{line_b}");
    println!("\n(les deux colonnes 0 = meme cout : le scrollback est seul en cause)");
}
