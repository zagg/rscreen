#!/usr/bin/env python3
"""Ciblage des sessions depuis la CLI : nom, pid, `pid.machin`, `-r` seul.

`rscreen -ls` affiche un pid ; le client doit pouvoir le/recopier tel quel
(`rscreen -r 4213`, `rscreen -R 4213`) comme le fait gnu-screen, sans devoir
retrouver le nom `-S`."""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from drive import BIN, Drive, check, sh  # noqa: E402


def pid_of(name):
    out = sh(BIN, "-ls")
    m = re.search(re.escape(name) + r"\s+pid\s+(\d+)", out)
    return int(m.group(1)) if m else None


def main():
    print("== ciblage de session depuis la CLI ==")
    for n in ("zoulou", "cibled"):
        sh(BIN, "-S", n, "-x")

    sh(BIN, "-S", "zoulou", "-m", "-c", "sleep 300")
    out = sh(BIN, "-ls")
    check("la session demarree apparait dans -ls", "zoulou" in out)
    pid = pid_of("zoulou")
    check("-ls affiche bien un pid exploitable", pid is not None and pid > 0)
    if pid is None:
        print("  ECHEC  (sans pid, la suite n'a plus de sens)")
        return 1

    # 1. `rscreen -S zoulou -r` : la facon historique
    d = Drive(["-S", "zoulou", "-r"], rows=24, cols=100)
    check("-S nom -r se raccorde", d.expect("zoulou", 5))
    d.kill()

    # 2. par le pid, sans -S : `rscreen -r 4213`
    d = Drive(["-r", str(pid)], rows=24, cols=100)
    check("-r <pid> se raccorde (sans connaitre le nom)", d.expect("zoulou", 5))
    d.kill()

    # 3. `-R <pid>` : reprendre, y compris par son pid
    d = Drive(["-R", str(pid)], rows=24, cols=100)
    check("-R <pid> se raccorde", d.expect("zoulou", 5))
    d.kill()

    # 4. forme gnu-screen « pid.hote/pty »
    d = Drive(["-r", "%d.pts-3.homepage" % pid], rows=24, cols=100)
    check("-r <pid>.suffixe est reconnu", d.expect("zoulou", 5))
    d.kill()

    # 5. nom seul, sans aucune option : `rscreen zoulou`
    d = Drive(["zoulou"], rows=24, cols=100)
    check("un nom existant vise la session (pas une commande)", d.expect("zoulou", 5))
    d.kill()

    # 6. `-r` tout seul : une seule session detachee, il est evident
    d = Drive(["-r"], rows=24, cols=100)
    check("-r seul reprend la seule session detachee", d.expect("zoulou", 5))
    d.kill()

    # 7. une erreur qui explique, pas un « defaut : default »
    d = Drive(["-S", "nexistante", "-r"], rows=24, cols=100)
    d.read_for(2.5)
    err = d.text()
    check("la cible inexistante est nommee dans l'erreur", "nexistante" in err)
    check("la session vivante est proposee", "zoulou" in err)
    check("le bon geste est indique (`-r`)", "-r zoulou" in err)
    d.kill()

    # 8. ne pas deviner entre deux cibles
    out = sh(BIN, "-S", "autre", "-r", str(pid))
    check("-S + une cible deja identifiee : on refuse de trancher",
          "deux cibles" in out or "n'est pas une session" in out)

    # 9. tant qu'il y a -S, les positionnels restent une commande
    sh(BIN, "-S", "cmdx", "-x")
    d = Drive(["-S", "cmdx", "sleep", "400"], rows=24, cols=100)
    check("-S nom + commande : la commande tient dans la session", d.expect("sleep", 5))
    d.kill()
    check("et la session la porte sous ce nom", "cmdx" in sh(BIN, "-ls")
          and "sleep" in sh(BIN, "-L", "cmdx"))
    sh(BIN, "-S", "cmdx", "-x")

    # 10. `-x <pid>` tue la session designee par son pid
    sh(BIN, "-S", "zoulou", "-x", str(pid))
    check("-x <pid> tue la session visee", "zoulou" not in sh(BIN, "-ls"))

    fails = [n for n, ok in __import__("drive").RESULTS if not ok]
    print("\n----------------------------------------")
    print("%d verifications, %d echecs" % (len(__import__("drive").RESULTS), len(fails)))
    for f in fails:
        print("  ECHEC: " + f)
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
