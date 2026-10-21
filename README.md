# rscreen

**Un clone de `gnu-screen` écrit en Rust** : chaque session survit aux
déconnexions SSH, chaque session abrite des fenêtres, et une fenêtre modale
les liste toutes — le tout dans le terminal, sans clignotement.

![rscreen : sélecteur de sessions par-dessus htop](docs/screenshot.png)

*(rscreen dans un terminal : `htop` dans la fenêtre « benchmarking », le
sélecteur de sessions ouvert par-dessus.)*

`rscreen`, c'est `screen` qu'on *veut bien* relancer : une binaire de quelques
mégo, trois dépendances (`libc`, `vt100`, `unicode-width`), zèle de configuration.

| | |
|---|---|
| 💨 Déconnecté ? La session vit | un serveur par session, détaché du terminal |
| 🪟 Fenêtres multiples | un shell, un build, `htop`… chacun dans son pty |
| 🗂️ Sélecteur de sessions | `Ctrl+b` puis `a` : liste modale, plier/déplier, renommer (`r`) |
| ⚡ Sans clignotement | rendu **différentiel** : seul l'écran change, pas l'écran entier |
| ⇥ `Tab` rendue à l'app | complétion bash, TAB vim, navigation… tout reste aux applis |
| 🔁 Reprise à chaud | basculer `dev` → `build` ne réinitialise pas le terminal |
| 🪶 Léger | `libc` + `vt100` + `unicode-width`, c'est tout |

## Installation

```sh
cargo build --release            # une binaire : target/release/rscreen
make install                     # → /usr/local/bin/rscreen
```

Linux uniquement (ptys, `signalfd`, `TIOCGPTPEER`). Rien à configurer :
les sessions sont stockées dans `$XDG_RUNTIME_DIR/rscreen-<uid>`
(ou `/tmp/rscreen-<uid>`, ou `$RSCREEN_DIR`).

## Démarpage rapide

```sh
rscreen                          # session « default », crée si besoin
rscreen -S dev                   # session « dev »
rscreen                          # depuis un autre poste : « dev » vous retrouve
rscreen -ls                      # lister les sessions
rscreen -S dev -c 'make -j8'     # première fenêtre : une commande précise
rscreen -S cron -dm -c './nightly.sh'   # démarrée en arrière-plan, pas de terminal
rscreen -S dev -x                # tuer « dev »
```

Dans une session, les fenêtres se pilotent au préfixe `Ctrl+a` :

| après `Ctrl+a` | effet |
|---|---|
| `n` / `p` / `0`…`9` | fenêtre suivante / précédente / la N |
| `c` | nouvelle fenêtre |
| `d` | détacher (la session tourne seule) |
| `w` | **sélecteur de sessions** — comme `Ctrl+b` puis `a` |
| `k` / `x` | tuer la fenêtre / la session |
| `[` | historique (`↑ ↓ PgUp PgDn`) |
| `t` | état · `?` aide |

Et le sélecteur (`Ctrl+b` `a` ou `w`) est une vraie fenêtre de navigation :
`↑` `↓` pour choisir, `→` pour déplier les fenêtres d'une session, `Entrée`
pour rejoindre, `r` pour renommer, `d` pour mettre en arrière-plan,
`x` pour tuer, `n` pour une nouvelle. Rien de plus — `Tab` reste à l'application.

## Comment ça tient debout

```
terminal réel ⟷ client rscreen ⟷ socket unix ⟷ serveur de session ⟷ pty des fenêtres
```

Le serveur tient pour chaque fenêtre un **émulateur VT100** (`vt100`) : il
connaît donc l'état exact de chaque écran. Il ne résume jamais une fenêtre —
il n'envoie au client que le **différentiel** de son écran à lui. D'où la
reprise instantanée, les basculements sans clignotement, et ce fait que
déconnecter une session n'y change rien : rien n'est « rejoué », tout est déjà là.

Les sessions sont autonomes : chacune écrit `sock`, `pid` et `state` dans son
répertoire, et c'est le client qui les dénombre en balayant ce répertoire
(au passage, il purge les sessions mortes). Renommer une session est un vrai
`rename(2)` sur son répertoire — le socket suit sans changer d'inode, les
clients déjà branchés vivent toujours.

<details>
<summary>Plus de détails techniques (clavier, kitty, rendu…)</summary>

* **Curseur** : sa visibilité est celle de **l'application** (`DECSI 25`), pas
  celle de rscreen — le serveur l'impose à chaque rendu complet. rscreen ne le
  masque que dans ses propres fenêtres modales.
* **Clavier** : les deux seules prises en charge « métier » sont `Ctrl+a`
  (préfixe, comme gnu-screen) et le duo `Ctrl+b` puis `a` (sélecteur) — deux
  octets de contrôle classiques, donc dispo sur n'importe quel terminal.
  `Tab` n'étant plus capturée, `Ctrl+b` est rendu à l'application dès que la
  deuxième touche tarde (300 ms) : readline garde sa flèche gauche.
* **Protocole kitty** : le client le détecte (`CSI ? u`) et l'active
  (`CSI > 1 u`) — reconnaissance de `Ctrl+Entrée` (détacher) et traduction des
  touches étendues en octets legacy pour les applis internes. Si une fenêtre
  demande elle-même le protocole, la traduction est désactivée pour elle.
* **Barre de statut** : la dernière ligne est réservée par le client ; la
  région de défilement du terminal réel est bornée (`CSI 1;<n> r`) pour qu'elle
  ne soit jamais touchée par les rendus.
* **Sessions** : basculer d'une session à l'autre se fait à chaud, sans
  réinitialiser le terminal.
</details>

## Tests

```sh
make test       # tests unitaires (rendu, clavier, états…)
make itest      # bout-en-bout : rscreen piloté dans un vrai pty
make dump       # rejoue l'écran du client dans un émulateur VT (pyte)
```

## Limites assumées

* pas de `split` (plusieurs panneaux sur un écran) — les fenêtres prennent
  tout l'espace, comme gnu-screen ;
* pas de sélection au caractère : l'historique est consultable, pas copiable ;
* plusieurs clients sur une même session se suivent (même fenêtre active),
  comme `screen` en multi-terminaux.

## Licence

MIT — voir [LICENSE](LICENSE).
