# rscreen

`rscreen` est une réécriture de **gnu-screen** (le multiplexeur de terminal) en
**Rust**, sans dépendance externe autre que `libc`, `vt100` (émulation VT100) et
`unicode-width` :

* chaque **session** tourne dans un processus serveur détaché : elle survit à la
  fermeture du terminal, aux déconnexions, à `SIGHUP` ;
* chaque session héberge des **fenêtres** (un shell, un build, `htop`… chacun
  dans son propre pty) ;
* **`Tab`** et **`Maj+Tab`** ne sont **pas capturées** : elles partent telles
  quelles vers l'application tenue par la fenêtre (complétion du shell, TAB
  d'un éditeur, touches de navigation…). Changer de fenêtre se fait avec
  **`Ctrl+a n`** / **`Ctrl+a p`** ou les chiffres **`0`…`9`** ;
* **`Ctrl+b` puis `a`** ouvre une **fenêtre modale listant les sessions** : on
  se déplace avec les touches **`↑`** et **`↓`**, on valide avec **`Entrée`**.
  La liste propose les sessions actives (et leurs fenêtres), « + nouvelle
  session… », « ← retour au shell » et « ⤳ rester en arrière-plan » (détacher).

```
┌ sélecteur de sessions — 2 sessions · ligne 1/7──────────────────────┐
│▸▾ dev   ← courante                48×160 · 3 fen. · pid 4242 · JOINTE│
│      • 0   bash                                                     │
│        1   cargo watch                                              │
│      ▸ build                        48×160 · 1 fen. · pid 4300 · ARRIÈRE-PLAN
│  + nouvelle session…                                                │
│  ← retour au shell                                           Entrée  │
│  ⤳ rester en arrière-plan (détacher ce terminal)            Ctrl+a d │
└─────────────────────────────────────────────────────────────────────┘
 rscreen « dev » [jointe]  0 bash  1 cargo watch  2 htop     18:42 160×48
```

## Construction

```sh
cargo build --release     # cible : target/release/rscreen
make install              # /usr/local/bin/rscreen
make test                 # 11 tests unitaires
make itest                # 49 vérifications bout-en-bout (53 avec pyte)
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
rscreen -S cron -dm -c 'make -j8'   # session démarrée sans terminal (arrière-plan)
rscreen -D -S dev            # éjecter les autres clients puis se raccorder
rscreen -S dev -x            # tuer la session
rscreen -S dev -X next       # piloter à distance (next prev other new select:N
                             #  detach kill redraw query)
```

Sessions et sockets sont rangés dans `$XDG_RUNTIME_DIR/rscreen-<uid>`
(ou `/tmp/rscreen-<uid>`), ou `$RSCREEN_DIR` si définie.

Avec `RSCREEN_DEBUG=1`, le serveur d'une session écrit ses traces dans
`$RSCREEN_DIR/debug.log` (pratique pour comprendre une session récalcitrante).

## Raccourcis

| touche          | effet                                                    |
|-----------------|----------------------------------------------------------|
| `Ctrl+b` puis `a` | **fenêtre modale : sélecteur de sessions** (↑ ↓ puis Entrée) |
| `Ctrl+Entrée`   | détacher : la session repasse en arrière-plan             |
| `Ctrl+a`        | préfixe à la gnu-screen (puis une des touches ci-dessous) |
| `Tab` / `Maj+Tab` | **rendues à l'application** : rscreen ne les capture plus |

Après le préfixe `Ctrl+a` :

| touche     | effet                                            |
|------------|--------------------------------------------------|
| `a`        | envoyer un vrai `Ctrl+a`                          |
| `Tab`      | envoyer une vraie tabulation (complétion shell)   |
| `n`/`p`    | fenêtre suivante / précédente                     |
| `0`…`9`    | aller à la fenêtre N                              |
| `Ctrl+a`   | revenir à la fenêtre précédente (`other`)         |
| `w` ou `"` | sélecteur de sessions — même effet que `Ctrl+b` puis `a` |
| `c` / `C`  | nouvelle fenêtre / nouvelle fenêtre avec commande  |
| `k` / `x`  | tuer la fenêtre / tuer la session (confirmation)  |
| `r`        | renommer la fenêtre                               |
| `d` / `D`  | détacher / éjecter les autres clients              |
| `[`        | mode historique (`↑ ↓ PgUp PgDn Début`, Échap sortir) |
| `l`        | redessiner l'écran                                |
| `t`        | état de la session                                |
| `?` / `h`  | aide                                              |

### Dans la fenêtre modale du sélecteur

La liste, dans l'ordre :

1. les **sessions actives** — déplier une session avec `→` donne ses fenêtres ;
2. **`+ nouvelle session…`** — nom à saisir, la session est créée puis rejointe ;
3. **`← retour au shell`** — referme la fenêtre sans rien changer ;
4. **`⤳ rester en arrière-plan`** — détache ce terminal, la session continue.

On se déplace avec **`↑` / `↓`** (et `PgPr` `PgSu` `Début` `Fin`, ou `j` `k`),
**`Entrée` valide** la ligne surlignée. Les autres touches : `←` `→` pour
replier/déplier, `d` pour mettre une session en arrière-plan, `x` pour la tuer,
`n` pour en créer une nouvelle, `Échap` (ou `Ctrl+b` puis `a` une seconde fois)
pour refermer.

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
* **Curseur** : la visibilité du curseur est celle de **l'application** (`DEC
  private mode 25`) — le serveur l'impose à chaque rendu complet et le client la
  suit dans le flux. rscreen ne le masque que pour les fenêtres modales
  (sélecteur, aide, saisie, historique) et le rend à la fermeture.
* **Clavier** : les deux seules touches « métier » du client sont `Ctrl+a`
  (préfixe) et le duo `Ctrl+b` puis `a` (sélecteur) — toutes deux composées
  d'octets de contrôle classiques, donc disponibles sur **n'importe quel**
  terminal, y compris ceux qui ne savent pas envoyer `Ctrl+Tab`. Puisque `Tab`
  n'est plus capturée, rscreen peut rendre `Ctrl+b` à l'application dès que le
  deuxième touche tarde (300 ms) : readline garde sa flèche gauche.
  Le client interroge tout de même le *kitty keyboard protocol* (`CSI ? u`) et
  l'active (`CSI > 1 u`) si le terminal le parle : cela permet de reconnaître
  `Ctrl+Entrée` (détacher) et de retraduire les touches étendues en octets
  legacy pour les applications internes (bash, vim…). Si l'application d'une
  fenêtre demande elle-même le protocole kitty, la traduction est désactivée
  pour cette fenêtre et la requête de l'application reçoit du serveur la
  réponse adaptée.
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
