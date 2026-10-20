# rscreen

`rscreen` est une réécriture de **gnu-screen** (le multiplexeur de terminal) en
**Rust**, sans dépendance externe autre que `libc`, `vt100` (émulation VT100) et
`unicode-width` :

* chaque **session** tourne dans un processus serveur détaché : elle survit à la
  fermeture du terminal, aux déconnexions, à `SIGHUP` ;
* chaque session héberge des **fenêtres** (un shell, un build, `htop`… chacun
  dans son propre pty) ;
* **`Tab`** passe à la fenêtre suivante, **`Maj+Tab`** à la précédente ;
* **`Ctrl+Tab`** ouvre une **fenêtre modale listant les sessions** : on choisit
  une session (ou une fenêtre d'une session) avec les flèches, on y bascule avec
  **`Entrée`**, et on peut aussi **rester en arrière-plan** (détacher) depuis
  cette même liste.

```
┌ sélecteur de sessions — 2 sessions · ligne 1/6──────────────────────┐
│▸▾ dev   ← courante                48×160 · 3 fen. · pid 4242 · JOINTE│
│      • 0   bash                                                     │
│        1   cargo watch                                              │
│      ▸ build                        48×160 · 1 fen. · pid 4300 · ARRIÈRE-PLAN
│  + nouvelle session…                                                │
│  ⤳ rester en arrière-plan (détacher ce terminal)             Entrée │
└─────────────────────────────────────────────────────────────────────┘
 rscreen « dev » [jointe]  0 bash  1 cargo watch  2 htop     18:42 160×48
```

## Construction

```sh
cargo build --release     # cible : target/release/rscreen
make install              # /usr/local/bin/rscreen
make test                 # 10 tests unitaires
make itest                # 38 vérifications bout-en-bout dans un pty réel
```

`rscreen` nécessite Linux (pty, `signalfd`, `TIOCGPTPEER`) et un terminal.

## Utilisation

```sh
rscreen                      # session « default », crée si besoin
rscreen -S dev               # session « dev »
rscreen -S dev -r            # s'y raccorder (sans en créer d'autre)
rscreen -S build -c 'make -j8'   # première fenêtre : une commande précise
rscreen -ls                  # lister les sessions
rscreen -S dev -L            # lister les fenêtres d'une session
rscreen -S dev -d            # détacher (arrière-plan) sans se raccorder
rscreen -D -S dev            # éjecter les autres clients puis se raccorder
rscreen -S dev -x            # tuer la session
rscreen -S dev -X next       # piloter à distance (next prev other new select:N
                             #  detach kill redraw query)
```

Sessions et sockets sont rangés dans `$XDG_RUNTIME_DIR/rscreen-<uid>`
(ou `/tmp/rscreen-<uid>`), ou `$RSCREEN_DIR` si définie.

## Raccourcis

| touche          | effet                                                    |
|-----------------|----------------------------------------------------------|
| `Tab`           | fenêtre suivante                                          |
| `Maj+Tab`       | fenêtre précédente                                        |
| `Ctrl+Tab`      | **sélecteur de sessions** (liste + arrière-plan)          |
| `Ctrl+Entrée`   | détacher : la session repasse en arrière-plan             |
| `Ctrl+a`        | préfixe à la gnu-screen (puis une des touches ci-dessous) |

Après le préfixe `Ctrl+a` :

| touche     | effet                                            |
|------------|--------------------------------------------------|
| `a`        | envoyer un vrai `Ctrl+a`                          |
| `Tab`      | envoyer une vraie tabulation (complétion shell)   |
| `n`/`p`    | fenêtre suivante / précédente                     |
| `0`…`9`    | aller à la fenêtre N                              |
| `Ctrl+a`   | revenir à la fenêtre précédente (`other`)         |
| `w` ou `"` | sélecteur de sessions — même effet que `Ctrl+Tab`  |
| `c` / `C`  | nouvelle fenêtre / nouvelle fenêtre avec commande  |
| `k` / `x`  | tuer la fenêtre / tuer la session (confirmation)  |
| `r`        | renommer la fenêtre                               |
| `d` / `D`  | détacher / éjecter les autres clients              |
| `[`        | mode historique (`↑ ↓ PgUp PgDn Début`, Échap sortir) |
| `l`        | redessiner l'écran                                |
| `t`        | état de la session                                |
| `?` / `h`  | aide                                              |

Dans le sélecteur : `↑↓` (ou `j k`), `PgUp/PgDn`, `Début/Fin` pour choisir,
`Entrée` pour basculer sur la session (ou la fenêtre) surlignée ou pour rester en
arrière-plan, `←→` pour déplier/replier les fenêtres, `d` pour mettre une session
en arrière-plan, `x` pour la tuer, `n` pour en créer une nouvelle, `Échap` ou
`Ctrl+Tab` pour refermer.

## Comment ça marche

```
terminal réel  <->  client rscreen  <-> [socket unix]  serveur de session  <-> ptys des fenêtres
```

* ***Client** (`src/client.rs`) : met le terminal en mode raw, écran alternatif,
  réserve la dernière ligne pour sa barre de statut (fenêtres, session, messages,
  horloge), décode les touches, gère préfixe/sélecteur/historique, et ne fait
  qu'écrire les octets que le serveur lui envoie.
* **Serveur** (`src/server.rs`) : une boucle `poll()` unique, sans thread
  additionnel. Il tient pour chaque fenêtre un émulateur VT100 (`vt100`), donc
  l'état complet de l'écran et un historique de 2000 lignes.
* **Rendu** : le serveur connaît l'état du terminal du client (un `Screen` de
  référence par client) et n'envoie que le **différentiel** — c'est ce qui rend
  le basculement de fenêtre instantané et sans scintillement, et ce qui permet
  de reprendre une session avec tout son contenu. La région de défilement du
  terminal réel est bornée à la zone des fenêtres (`CSI 1;<n> r`) pour que la
  barre de statut ne soit jamais touchée.
* **Clavier étendu** : `Ctrl+Tab` n'existe pas sur les terminaux traditionnels
  (il y est identique à `Tab`). Le client interroge donc le *kitty keyboard
  protocol* (`CSI ? u`) et l'active (`CSI > 1 u`) si le terminal le parle ; il
  retraduit ensuite les touches reçues en octets legacy pour que les
  applications internes (bash, vim…) continuent de comprendre `Ctrl+C`, etc.
  Si l'application d'une fenêtre demande elle-même le protocole kitty, la
  traduction est désactivée pour cette fenêtre et la requête de l'application
  reçoit du serveur la réponse adaptée. Sans terminal compatible, le sélecteur
  reste accessible par `Ctrl+a w` ou `Ctrl+a "`.
* **Sessions** : chaque serveur écrit `sock`, `pid` et `state` dans son
  répertoire ; le client balaie ce répertoire pour construire la liste du
  sélecteur (et purge les sessions mortes). Basculer d'une session à l'autre se
  fait à chaud, sans clignotement ni nouvelle initialisation du terminal.

## Limit assumées

* pas de découpage d'un écran en plusieurs panneaux (`split`) : les fenêtres
  prennent tout l'espace, comme gnu-screen ;
* pas de mode copie/collage avec sélection au caractère (l'historique est
  consultable mais pas copiable) ;
* plusieurs clients raccordés à une même session se suivent (même fenêtre
  active), à la manière de `screen` en mode multi-terminaux.

## Licence

MIT.
