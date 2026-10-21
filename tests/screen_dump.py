#!/usr/bin/env python3
"""Vérification visuelle : pilote rscreen dans un pty et rend l'écran avec
pyte (émulateur VT), pour contrôler l'alignement de la barre de statut et la
fenêtre du sélecteur de sessions."""
import fcntl
import os
import pty
import select
import struct
import sys
import termios
import time

import pyte

BIN = os.environ.get("RSCREEN", "./target/debug/rscreen")
ENV = dict(os.environ, XDG_RUNTIME_DIR="/tmp/rscreen-visual")
ROWS, COLS = 30, 100


class Screen:
    def __init__(self, args, rows=ROWS, cols=COLS):
        os.makedirs("/tmp/rscreen-visual", exist_ok=True)
        self.rows, self.cols = rows, cols
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ.update(ENV)
            try:
                os.execv(BIN, [BIN] + args)
            except Exception:
                os._exit(99)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def pump(self, timeout=0.4):
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return False
        try:
            d = os.read(self.fd, 65536)
        except OSError:
            return False
        if not d:
            return False
        self.stream.feed(d.decode("utf-8", "ignore"))
        return True

    def read_for(self, secs=1.0):
        end = time.time() + secs
        while time.time() < end:
            if not self.pump(0.2):
                return

    def send(self, s):
        os.write(self.fd, s.encode() if isinstance(s, str) else s)

    def dump(self, title):
        print("\n----- {} -----".format(title))
        for line in self.screen.display:
            print("|" + line.rstrip("\n"))
        print("+" + "-" * self.cols)


def main():
    s = Screen(["-S", "v1"])
    s.read_for(2)
    s.send("echo bienvenue dans la fenetre 0\r")
    s.send("\x01c")
    s.read_for(1)
    s.send("echo fenetre numero un\r")
    s.read_for(1)
    s.dump("session v1, fenetre 1 (barre de statut en bas)")
    s.send("\x01n")
    s.read_for(1)
    s.dump("apres Ctrl+a n -> fenetre 0")
    s.send("\x02a")
    s.read_for(1.2)
    s.dump("selecteur de sessions (Ctrl+b puis a)")
    s.send("\x1b[B")
    s.read_for(0.6)
    s.dump("selecteur: ligne descendue")
    s.send("\x02a")  # le meme duo la referme
    s.read_for(0.8)
    s.dump("selecteur ferme par le duo")
    s.send("\x01?")
    s.read_for(1)
    s.dump("aide (Ctrl+a ?)")
    s.send("\x1b")
    s.read_for(0.5)
    s.kill = None
    os.kill(s.pid, 9)
    return 0


if __name__ == "__main__":
    sys.exit(main())
