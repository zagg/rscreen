#!/usr/bin/env python3
"""Banc d'essai « r » : renommer une session depuis la fenêtre modale du
sélecteur. Le serveur renomme son propre répertoire ; on vérifie que le socket
suit, que le client courant garde son shell, et que les refus sont propres."""
import glob
import os
import time

from drive import BIN as RAW_BIN, Drive, sh

BIN = os.path.abspath(RAW_BIN)

BASE = "/tmp/rscreen-test/rscreen-%d" % os.getuid()
RESULTS = []


def check(name, cond):
    RESULTS.append((name, bool(cond)))
    print(("  OK    " if cond else "  ECHEC ") + name)


def dirs():
    return sorted(os.path.basename(p) for p in glob.glob(BASE + "/*"))


def sock(n):
    return os.path.exists(f"{BASE}/{n}/sock")


def main():
    for n in ("alpha", "beta", "gamma", "delta"):
        sh(BIN, "-S", n, "-x")
    time.sleep(0.3)
    d = Drive(["-S", "alpha"], rows=24, cols=110)
    d.read_for(2.0)
    sh(BIN, "-S", "beta", "-m")
    time.sleep(1.0)
    check("deux sessions vivantes", dirs() == ["alpha", "beta"])

    def open_pick():
        d.send("\x1b")  # au cas où une modale est déjà ouverte
        d.read_for(0.4)
        d.clear()
        d.send("\x02a")  # Ctrl+b puis a
        ok = d.expect(b"lecteur", 3)
        d.read_for(0.6)
        return ok

    def last():
        return d.visible().split("\n")[-1]

    def bar():
        return last() if "rscreen «" in last() else ""

    def goto_first():
        d.send("\x1b[H")  # Début : première ligne, une session
        d.read_for(0.4)

    def press_r(label):
        d.send("r")
        d.read_for(0.5)
        return label in last() and "renommer" in last() and "en :" in last()

    def type_name(name):
        d.send(name + "\r")
        d.read_for(1.4)

    # ------------------------------------------------ renommer une autre session
    assert open_pick()
    d.send("\x1b[H\x1b[B\x1b[B")  # Début puis 2 fois BAS : la ligne « beta »
    d.read_for(0.4)
    check("« r » ouvre l'invite de renommage", press_r("beta"))
    type_name("gamma")
    check("répertoire rebaptisé", dirs() == ["alpha", "gamma"])
    check("le socket suit le répertoire", sock("gamma") and not sock("beta"))
    check("le sélecteur affiche le nouveau nom", "gamma" in d.visible())
    check("message de succès", "renommée" in bar() and "gamma" in bar())
    check("l'autre session n'a pas bougé", "alpha" in bar())

    # -------------------------------------- renommer la session que l'on tient
    check("le shell de la session courante répond",
          echo(d, "AVANT-RENAME"))
    assert open_pick()
    goto_first()
    check("invite pour la session courante", press_r("alpha"))
    type_name("delta")
    check("répertoire de la courante rebaptisé", dirs() == ["delta", "gamma"])
    check("le client sait qu'il s'appelle delta", "« delta »" in d.visible())
    d.send("\x1b")  # Échap : retour au shell
    d.read_for(0.8)
    check("le shell de la session renommée vit toujours", echo(d, "APRES-RENAME"))

    # ------------------------------------------------------------- les refus
    assert open_pick()
    goto_first()
    press_r("gamma")
    type_name("gamma")
    check("refus si le nom est déjà pris", "porte déjà" in bar())
    check("rien n'a été renommé", dirs() == ["delta", "gamma"])

    assert open_pick()
    goto_first()
    press_r("delta")
    type_name("delta")
    check("refus si le nom est le même", "déjà sous ce nom" in bar())

    assert open_pick()
    goto_first()
    press_r("delta")
    type_name("   ")
    check("refus si le nom est vide", "vide" in bar())

    # Échap dans l'invite : rien ne change
    assert open_pick()
    goto_first()
    press_r("delta")
    d.send("x\x1b")
    d.read_for(0.8)
    check("Échap dans l'invite annule", dirs() == ["delta", "gamma"])
    d.send("\x1b")
    d.read_for(0.6)
    check("retour au shell après Échap", echo(d, "FIN-COURANTE"))

    # ---------------------- la session renommée reste raccordable par son nom
    assert open_pick()
    d.send("\x1b[F\x1b[A\x1b[A\x1b[A")  # Fin, 3 fois Haut : la ligne « gamma »
    d.read_for(0.4)
    d.send("\x1b[C")  # Right : on la déplie, preuve que la ligne est bien elle
    d.read_for(0.5)
    d.send("\x1b[D")  # Left : on la replie
    d.read_for(0.3)
    check("la ligne visée est bien gamma", "gamma" in d.visible())
    d.send("\r")
    d.read_for(1.4)
    check("bascule sur la session renommée", "« gamma »" in last())
    check("la session renommée est pilotable", echo(d, "TEMOIN-GAMMA"))
    check("le CLI la voit sous son nouveau nom", "gamma" in sh(BIN, "-ls"))
    check("plus aucune trace de beta", "beta" not in sh(BIN, "-ls"))
    d.kill()
    for n in ("alpha", "beta", "gamma", "delta"):
        sh(BIN, "-S", n, "-x")
    good = sum(1 for _, c in RESULTS if c)
    print(f"\nsoldes : {good} / {len(RESULTS)}")
    return 0 if good == len(RESULTS) else 1


def echo(d, mark):
    """le shell rendu par la session en cours affiche-t-il `mark` ?"""
    d.send("\x1b")  # ferme la modale ouverte, sinon elle boit la sortie
    d.read_for(0.4)
    d.clear()
    d.send(f"echo {mark}\r")
    return d.expect(mark.encode(), 3)


if __name__ == "__main__":
    raise SystemExit(main())
