#!/usr/bin/env python3
"""Chemin chaud du serveur, bout-en-bout : une fenetre qui debite sans
s'arreter (seq 1 100000000) pendant que N clients 48x160 sont joints.

On mesure ce que le serveur recache de sortie applicative par seconde (rchar,
octets lus par read()) et le % CPU correspondant.

Rappel du cout a optimiser : `Screen::clone()` par client et par frame copie
les deux grilles ET tout le scrollback (~10 Mo, ~2000 Vec avec -H 2000), alors
qu'une frame ne montre que 48x160 cellules (245 Ko).

  RSCREEN=./target/release/rscreen python3 tests/bench_cpu.py [scrollback] [nclients...]

sans argument : balayage 1 puis 2 puis 3 clients, historique 2000 (le defaut).
"""
import os, shutil, sys, time

sys.path.insert(0, "tests")
import drive
from drive import Drive, sh

BIN = os.environ.get("RSCREEN", "./target/release/rscreen")
HZ = os.sysconf("SC_CLK_TCK")
DIR = f"/tmp/rscreen-test/rscreen-{os.geteuid()}"
SECS = 6.0
ROWS, COLS = 48, 160


def cpu(pid):
    with open(f"/proc/{pid}/stat") as f:
        f_ = f.read().rsplit(") ")[1].split()
    return int(f_[11]) + int(f_[12])  # utime + stime (ticks)


def rchar(pid):
    with open(f"/proc/{pid}/io") as f:
        for line in f:
            if line.startswith("rchar:"):
                return int(line.split()[1])
    return 0


def measure(sb, nclients=1, secs=SECS):
    # (re)demarrer une session propre : le serveur efface son repertoire a la
    # sortie, et un menage tardif effacerait la session qui vient de naitre
    pid = None
    for _ in range(3):
        sh(BIN, "-S", "cpu", "-x")
        for _ in range(40):
            if not os.path.exists(f"{DIR}/cpu"):
                break
            time.sleep(0.1)
        shutil.rmtree(f"{DIR}/cpu", ignore_errors=True)
        out = sh(BIN, "-S", "cpu", "-dm", "-H", str(sb), "-c", "seq 1 100000000")
        if "marrée" in out or "marré" in out:
            for _ in range(60):
                pf = f"{DIR}/cpu/pid"
                if os.path.exists(pf):
                    cand = int(open(pf).read().strip())
                    if os.path.exists(f"/proc/{cand}"):
                        pid = cand
                        break
                time.sleep(0.1)
            break
    if pid is None:
        return None
    ds = [Drive(["-S", "cpu", "-r"], rows=ROWS, cols=COLS) for _ in range(nclients)]
    time.sleep(1.0)  # les clients se raccordent, le scrollback s'emplit
    if not os.path.exists(f"/proc/{pid}"):
        for d in ds:
            d.kill()
        return None
    t0, c0, r0 = time.time(), cpu(pid), rchar(pid)
    end, alive = t0 + secs, True
    while time.time() < end and alive:
        # pas de court-circuit : il faut vider les sockets des N clients
        ok = [d.pump(0.05) for d in ds]
        alive = all(ok)
    w = max(time.time() - t0, 0.1)
    mb = (rchar(pid) - r0) / 2**20 / w
    pct = 100.0 * ((cpu(pid) - c0) / HZ) / w
    for d in ds:
        d.kill()
    sh(BIN, "-S", "cpu", "-x")
    return mb, pct


def main():
    args = sys.argv[1:]
    if args:
        sb = int(args[0])
        ns = [int(x) for x in (args[1:] or ["1"])]
        print(f"historique {sb}, {ROWS}x{COLS}")
        for n in ns:
            r = measure(sb, n)
            print(f"  {n} client : " + ("  —" if r is None else f"{r[0]:5.2f} Mo/s, {r[1]:5.1f} % CPU"))
        return
    print(f"clients joints sur une fenetre 48x160 qui debite, historique 2000, {SECS:.0f} s")
    print("  clients   Mo recachées/s   % CPU serveur")
    for n in (1, 2, 3):
        r = measure(2000, n)
        print(f"  {n:<8}   " + ("    —       —" if r is None else f"{r[0]:9.2f}    {r[1]:9.1f} %"))
        time.sleep(0.3)


if __name__ == "__main__":
    main()
