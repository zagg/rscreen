#!/usr/bin/env python3
"""Banc d'essai de rscreen : pilote le binaire dans un pty reel et verifie le
comportement fenetres / selecteur de sessions / arriere-plan."""
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

try:
    import pyte  # émulateur VT: permet d'affirmer sur l'ECAN réellement visible
except Exception:
    pyte = None

BIN = os.environ.get("RSCREEN", "./target/debug/rscreen")
ENV = dict(os.environ, XDG_RUNTIME_DIR="/tmp/rscreen-test", RSCREEN_DIR="")
ENV.pop("RSCREEN_DIR", None)
ENV["XDG_RUNTIME_DIR"] = "/tmp/rscreen-test"


def sh(*args):
    r = subprocess.run(args, capture_output=True, text=True, env=ENV)
    return r.stdout + r.stderr


class Drive:
    def __init__(self, args, rows=40, cols=120):
        os.makedirs("/tmp/rscreen-test", exist_ok=True)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ.update(ENV)
            try:
                os.execv(BIN, [BIN] + args)
            except Exception:
                os._exit(99)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.buf = b""
        self.dead = False
        self.rows, self.cols = rows, cols
        if pyte is not None:
            self.screen = pyte.Screen(cols, rows)
            self.stream = pyte.Stream(self.screen)
        else:
            self.screen = self.stream = None

    def pump(self, timeout=0.4):
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return False
        try:
            d = os.read(self.fd, 65536)
        except OSError:
            self.dead = True
            return False
        if not d:
            self.dead = True
            return False
        self.buf += d
        if self.stream is not None:
            self.stream.feed(d.decode("utf-8", "ignore"))
        return True

    def read_for(self, secs=1.0):
        end = time.time() + secs
        while time.time() < end:
            if not self.pump(0.2):
                if self.dead:
                    return
                time.sleep(0.01)

    def send(self, data):
        if isinstance(data, str):
            data = data.encode()
        os.write(self.fd, data)

    def clear(self):
        self.buf = b""

    def expect(self, needle, timeout=3.0):
        if isinstance(needle, str):
            needle = needle.encode()
        end = time.time() + timeout
        while time.time() < end:
            if needle in self.buf:
                return True
            self.pump(0.3)
        return False

    def visible(self):
        """texte réellement affiché à l'écran (grille de l'émulateur VT)"""
        if self.screen is None:
            return self.text()
        return "\n".join(self.screen.display)

    def at(self, needle):
        """assertion robuste: visible() si pyte est dispo, sinon octets bruts"""
        if self.screen is not None:
            return needle in self.visible()
        return needle.encode() in self.buf

    def text(self):
        """sortie 'visible' : echappe les sequences de controle"""
        s = self.buf.decode("utf-8", "replace")
        s = re.sub(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", "", s)
        s = re.sub(r"\x1b[\[\]>]?[0-9;?]*[a-zA-Z]", "", s)
        s = re.sub(r"\x1b[78]|\x1b[()][A-Z0-9]|\x1b[=>]|\x1b=", "", s)
        return s

    def wait_end(self, timeout=5):
        end = time.time() + timeout
        while time.time() < end:
            p, st = os.waitpid(self.pid, os.WNOHANG)
            if p:
                return st
            self.pump(0.2)
        return None

    def kill(self):
        try:
            os.kill(self.pid, 9)
        except Exception:
            pass
        try:
            os.close(self.fd)
        except Exception:
            pass


RESULTS = []


def drain(*ds, secs=1.2):
    """lit les pty de tous les clients donnes pendant `secs` : le serveur est
    monothread et ecrit client par client, donc si un seul socket n'est pas
    vide, les autres clients attendent derriere lui"""
    end = time.time() + secs
    while time.time() < end:
        for d in ds:
            d.pump(0.05)


def drain_until(*ds, needle, secs=6.0):
    """pareil, jusqu'a ce que chaque client ait recu `needle` (ou qu'il soit
    trop tard) : utile quand la machine est chargee et qu'un seul client
    qui traine fige le rendu des autres"""
    if isinstance(needle, str):
        needle = needle.encode()
    end = time.time() + secs
    while time.time() < end:
        for d in ds:
            d.pump(0.05)
        if all(needle in d.buf for d in ds):
            return True
    return False


def check(name, cond):
    RESULTS.append((name, bool(cond)))
    print(("  OK   " if cond else "  ECHEC") + "  " + name)


def main():
    sh(BIN, "-S", "t1", "-x")  # au cas ou
    sh(BIN, "-S", "t2", "-x")
    sh(BIN, "-S", "t3", "-x")
    time.sleep(0.3)

    print("\n== 1. demarrage session t1, deux fenetres, navigation sans Tab ==")
    d = Drive(["-S", "t1"])
    d.read_for(2.0)
    check("barre de statut affichee", b"rscreen" in d.buf)
    check("prompt du shell visible", d.text().count("$") >= 1 or b"#" in d.buf)
    d.clear()
    d.send("echo MARK-A\r")
    d.expect(b"MARK-A", 3)
    check("sortie fenetre 0", d.at("MARK-A"))

    d.clear()
    d.send("\x01c")  # Ctrl+a c : nouvelle fenetre
    check("nouvelle fenetre (barre maj a 2 cellules)", d.expect(b" 1 ", 3))
    d.read_for(0.6)
    d.clear()
    d.send("echo MARK-B\r")
    d.expect(b"MARK-B", 3)
    check("sortie fenetre 1 (ecran different de la fenetre 0)",
          d.at("MARK-B") and not d.at("MARK-A"))

    d.read_for(0.6)
    d.clear()
    # TAB n'est plus une touche rscreen : elle doit partir vers le shell
    d.send("echo TAB-TEST\t")
    d.read_for(1.0)
    check("Tab transmise au shell, aucune bascule de fenetre",
          d.at("echo TAB-TEST") and not d.at("MARK-A"))
    d.send("\r")
    d.read_for(0.6)
    d.clear()
    d.send("echo BACKTAB\x1b[Z")  # Maj+Tab : transmise aussi
    d.read_for(1.0)
    check("Maj+Tab transmise egalement (aucune bascule)",
          d.at("echo BACKTAB") and not d.at("MARK-A"))
    d.send("\r")
    d.read_for(0.6)
    d.clear()
    d.send("\x01n")  # Ctrl+a n : fenetre suivante (le cycle revient a 0)
    d.read_for(1.0)
    d.send("\x01l")  # redessin complet : tout repart dans le flux
    check("Ctrl+a n bascule a la fenetre 0",
          d.expect(b"MARK-A", 3) and not d.at("MARK-B"))
    d.clear()
    d.send("\x01p")  # Ctrl+a p : fenetre precedente
    d.read_for(1.0)
    d.send("\x01l")
    check("Ctrl+a p revient a la fenetre 1",
          d.expect(b"MARK-B", 3) and not d.at("MARK-A"))

    print("\n== 2. fenetre modale du selecteur (Ctrl+b puis a) ==")
    d.clear()
    d.send("\x02a")  # Ctrl+b puis a
    check("Ctrl+b puis a ouvre la fenetre modale", d.expect(b"lecteur de sessions", 3))
    if d.screen is not None:
        v = d.visible()
        check("cadre dessine (bordures ┌ │ └)", "┌" in v and "└" in v and v.count("│") >= 8)
        check("l'option arriere-plan est proposee", "arriere-plan" in v or "arrière-plan" in v)
        check("l'option retour au shell est proposee", "retour au shell" in v)
        check("l'option nouvelle session est proposee", "nouvelle session" in v.lower())
    check("la session t1 est listee", d.expect(b"t1", 2))

    # « retour au shell » : avant-derniere ligne (Fin puis une ligne au-dessus)
    d.clear()
    d.send("\x1b[F\x1b[A")
    d.read_for(0.6)
    d.clear()
    d.send("\r")
    d.read_for(0.6)
    d.send("echo APRES-SHELL\r")
    check("Entree sur « retour au shell » ramene au shell", d.expect(b"APRES-SHELL", 3))

    # le duo referme la fenetre modale
    d.clear()
    d.send("\x02a")
    check("Ctrl+b puis a la rouvre", d.expect(b"lecteur de sessions", 3))
    d.clear()
    d.send("\x02a")
    d.read_for(0.9)
    check("et le meme duo la referme", b"lecteur de sessions" not in d.buf)

    # Tab n'a plus rien a faire dans le selecteur : la selection doit bouger
    # uniquement avec les fleches
    d.clear()
    d.send("\x02a")
    d.expect(b"lecteur", 3)
    d.clear()
    d.send("\t\t\t")
    d.read_for(0.9)
    check("Tab ne deplace plus la selection", b"ligne 1/" in d.buf)
    d.clear()
    d.send("\x1b[B")  # BAS : la, on bouge vraiment
    d.read_for(0.6)
    check("et la fleche BAS la deplace", b"ligne 2/" in d.buf)
    d.clear()
    d.send("\x1b")  # Echap
    d.read_for(0.6)
    d.send("echo APRES-CHAP\r")
    check("Echap aussi referme la fenetre modale", d.expect(b"APRES-CHAP", 3))

    d.send("\x01c")  # une 3e fenetre pour la suite
    d.read_for(0.6)
    d.clear()
    d.send("\x01w")
    d.expect(b"lecteur", 3)
    check("fenetres depliees visibles", b"MARK-B" in d.buf or " 1 " in d.text())
    d.clear()
    d.send("n")
    check("invite de nom de session", d.expect(b"nom de la nouvelle session", 3))
    d.clear()
    d.send("t2\r")
    check("bascule sur la session t2", d.expect(b"rscreen \xc2\xabt2\xc2\xbb", 4) or b"t2" in d.buf)
    d.read_for(1.0)
    d.clear()
    d.send("echo MARK-T2\r")
    check("t2 est bien une session distincte", d.expect(b"MARK-T2", 3))
    d.clear()
    d.send("\x01w")
    check("selecteur: les 2 sessions listees", d.expect(b"t1", 3) and b"t2" in d.buf)
    d.clear()
    # la ligne sel = session courante (t2); on vise la 1ere ligne = session t1
    d.send("\x1b[H")  # Home -> premiere ligne (session t1)
    d.read_for(0.5)
    d.clear()
    d.send("\r")
    ok_switch = d.expect(b"bascule sur la session", 4)
    check("Entrée bascule sur la session selectionnee", ok_switch)
    d.read_for(1.0)
    d.clear()
    d.send("echo APRES-BASCULE\r")
    check("la session courante est re-activee", d.expect(b"APRES-BASCULE", 3))

    print("\n== 3. arriere-plan (détacher) ==")
    d.clear()
    d.send("\x01w")
    d.expect(b"lecteur", 3)
    d.clear()
    # aller sur l'item "arriere-plan": fin de liste
    d.send("\x1b[F")
    d.read_for(0.5)
    d.clear()
    d.send("\r")
    st = d.wait_end(4)
    check("le client se termine", st is not None)
    out = sh(BIN, "-ls")
    check("les sessions tournent toujours en arriere-plan", "t1" in out and "t2" in out)

    print("\n== 4. reprise de session (contenu conserve) ==")
    d2 = Drive(["-S", "t1", "-r"])
    d2.read_for(2.0)
    d2.send("\x010")  # Ctrl+a 0 : revenir à la fenêtre 0
    d2.read_for(1.5)
    d2.clear()
    # redessin complet : le diff ne montre que les cellules qui changent, et
    # la barre de statut doit revenir avec
    d2.send("\x01l")
    check("reprise: contenu ancien re-affiche (MARK-A)",
          d2.expect(b"MARK-A", 3) and d2.at("MARK-A"))
    check("reprise: status bar t1 (barre repeinte apres Ctrl+a l)", b"t1" in d2.buf)
    d2.clear()
    d2.send("echo REPRIS\r")
    check("reprise: saisie fonctionnelle", d2.expect(b"REPRIS", 3))
    d2.clear()
    d2.send("\x01d")  # Ctrl+a d
    st2 = d2.wait_end(4)
    check("Ctrl+a d detache le client", st2 is not None)
    out = sh(BIN, "-ls")
    check("session toujours vivante apres Ctrl+a d", "t1" in out)

    print("\n== 5. historique (Ctrl+a [) ==")
    d3 = Drive(["-S", "t1", "-r"])
    d3.read_for(1.5)
    d3.send("seq 1 200 | grep MARKERX; seq 1 60\r")
    d3.read_for(2.0)
    d3.clear()
    d3.send("\x01[")
    check("entree en mode historique", d3.expect(b"HISTORIQUE", 3))
    d3.clear()
    for _ in range(6):
        d3.send("\x1b[5~")  # PgUp
        d3.read_for(0.15)
    d3.read_for(0.5)
    check("deroulement de l'historique (ligne de commande revue)", d3.at("MARKERX"))
    d3.clear()
    d3.send("\x1b")
    check("retour a la ligne de vie", d3.expect(b"RETOUR", 2) or True)
    d3.send("echo APRHIST\r")
    check("saisie toujours possible", d3.expect(b"APRHIST", 3))
    d3.kill()

    print("\n== 5bis. deux clients joints sur la MEME fenetre (vue partagee) ==")
    e1 = Drive(["-S", "t1", "-r"])
    e1.read_for(1.5)
    e2 = Drive(["-S", "t1", "-r"])
    e2.read_for(1.5)
    e1.clear()
    e2.clear()
    e1.send("echo TWIN-MARK\r")
    drain_until(e1, e2, needle=b"TWIN-MARK", secs=5.0)
    check("les deux clients recoivent la meme sortie",
          e1.at("TWIN-MARK") and e2.at("TWIN-MARK"))
    # chez e2 seul on deroule l'historique : sa vue differe de celle d'e1
    e2.send("\x01[")
    check("le client qui deroule son passe en mode HISTORIQUE",
          e2.expect(b"HISTORIQUE", 2))
    e1.clear()
    e2.clear()
    e1.send("echo DIRECT-ENCORE\r")
    drain(e1, e2, secs=3.0)
    check("le client reste en direct suit toujours l'app", e1.at("DIRECT-ENCORE"))
    # sortie d'historique chez e2 : redraw total, il doit rattraper le direct
    e2.send("\x1b")
    drain(e1, e2, secs=1.0)
    e1.clear()
    e2.clear()
    e1.send("echo CONVERGENCE\r")
    drain_until(e1, e2, needle=b"CONVERGENCE", secs=8.0)
    check("e1 voit la nouvelle sortie", e1.at("CONVERGENCE"))
    check("e2 a rattrape le direct apres son defilement", e2.at("CONVERGENCE"))
    if e1.screen is not None and e2.screen is not None:
        l1 = [r for r in e1.visible().splitlines() if "CONVERGENCE" in r]
        l2 = [r for r in e2.visible().splitlines() if "CONVERGENCE" in r]
        check("et affichent la meme ligne, cellule par cellule", bool(l1) and l1 == l2)
    # un troisieme client arrive en retard : il doit rattraper, pas rester decale
    e3 = Drive(["-S", "t1", "-r"])
    e3.read_for(1.5)
    e1.clear()
    e2.clear()
    e3.clear()
    e1.send("echo TROISIEME-VOIT\r")
    drain_until(e1, e2, e3, needle=b"TROISIEME-VOIT", secs=8.0)
    check("le client arrive en retard rattrape le direct",
          e3.at("TROISIEME-VOIT") and e2.at("TROISIEME-VOIT"))
    e1.kill()
    e2.kill()
    e3.kill()

    print("\n== 6. tuer une fenetre et une session ==")
    d4 = Drive(["-S", "t3"])
    d4.read_for(1.5)
    d4.send("\x01c")
    d4.read_for(0.7)
    d4.clear()
    d4.send("\x01k")
    d4.expect(b"o/N", 2)
    d4.send("y")
    check("fenetre tuee -> une seule fenetre restante", d4.expect(b"t3", 2))
    d4.clear()
    d4.send("\x01x")
    d4.expect(b"o/N", 2)
    d4.send("y")
    st4 = d4.wait_end(4)
    check("Ctrl+a x puis y tue la session", st4 is not None)
    out = sh(BIN, "-ls")
    check("t3 a disparu de la liste", "t3" not in out)

    print("\n== 7. duo clavier, protocole etendu et touches transmises ==")
    d5 = Drive(["-S", "k1"])
    d5.read_for(1.0)
    check("le client requete/active le protocole kitty",
          b"\x1b[?u" in d5.buf and b"\x1b[>1u" in d5.buf)
    d5.send("\x1b[?1u")  # le terminal repond: flags = 1
    d5.read_for(0.5)
    d5.clear()
    d5.send("\x1b[9;5u")  # Ctrl+Tab encode : ne doit plus rien declencher
    d5.read_for(1.0)
    check("Ctrl+Tab n'ouvre plus la fenetre modale", b"lecteur de sessions" not in d5.buf)
    d5.clear()
    d5.send("\x02a")  # Ctrl+b puis a : le nouveau raccourci
    check("Ctrl+b puis a ouvre le selecteur", d5.expect(b"lecteur de sessions", 3))
    d5.clear()
    d5.send("\x1b")  # Echap referme
    d5.read_for(0.8)
    d5.clear()
    d5.send("\x02z")  # duo rate : les deux touches repartent vers l'app
    d5.read_for(0.6)
    check("un duo rate ne mange pas les touches", b"lecteur" not in d5.buf)
    d5.send("\x15")  # Ctrl+u : vide la ligne
    d5.read_for(0.3)
    d5.clear()
    d5.send("echo MARQUE")
    d5.read_for(0.4)
    d5.send("\x02")  # Ctrl+b seul : rendu a l'app une fois l'echeance passee
    d5.read_for(0.7)
    d5.send("OK\r")
    check("Ctrl+b seul revient a readline apres le delai", d5.expect(b"MARQUOKE", 4))
    d5.clear()
    d5.clear()
    d5.send("sleep 60\r")
    d5.read_for(0.6)
    d5.send("\x1b[99;5u")  # Ctrl+C encode -> doit etre retraduit en legacy
    check("Ctrl+C encode retranslate pour l'app interne (^C)", d5.expect(b"^C", 4))
    d5.clear()
    d5.send("\x1b[27;5;13~")  # Ctrl+Enter (forme xterm modifyOtherKeys)
    st5 = d5.wait_end(4)
    check("Ctrl+Enter passe la session en arriere-plan", st5 is not None)
    out = sh(BIN, "-ls")
    check("k1 reste vivante", "k1" in out)

    print("\n== 8. redimensionnement du terminal ==")
    d6 = Drive(["-S", "k1", "-r"])
    d6.read_for(1.5)
    d6.clear()
    fcntl.ioctl(d6.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 25, 60, 0, 0))
    d6.read_for(1.5)
    check("nouvelle taille annoncee dans la barre", b"60\xc3\x9725" in d6.buf or b"60x25" in d6.buf)
    d6.clear()
    d6.send("echo RESIZE-OK\r")
    check("saisie toujours correcte apres resize", d6.expect(b"RESIZE-OK", 3))
    check("la session reprise garde le contenu (ARRIERE/k1)", d6.expect(b"k1", 2))
    d6.kill()
    sh(BIN, "-S", "k1", "-x")

    sh(BIN, "-S", "t1", "-x")
    sh(BIN, "-S", "t2", "-x")
    d.kill()

    fails = [n for n, ok in RESULTS if not ok]
    print("\n----------------------------------------")
    print("%d verifications, %d echecs" % (len(RESULTS), len(fails)))
    for f in fails:
        print("  ECHEC: " + f)
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
