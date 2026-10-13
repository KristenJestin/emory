# Les mains d'Emory : version 1

Statut : brouillon du 2026-10-11, à valider par Kris. Noms passés en anglais le 2026-10-11. Le
module `commands`, l'exemple de `poste music`, la confirmation et la sécurité ajoutés le
2026-10-13 (brouillon, à relire avec Kris).

Les mains sont ce qu'Emory fait dans le monde. `mind` décide ; `emory-hands` exécute, sans réfléchir.
(Le programme s'appelle `emory-hands`, jamais `main` : en anglais, « main » se lit comme notre mot
« principal ».)

## Deux sortes d'actions

- **Un service réseau** (chercher dans la bibliothèque musicale de Navidrome, plus tard Matter et
  d'autres API) : c'est un module d'un `emory-hands` qui tourne **sur le serveur**, sous le nom
  d'appareil `server`. Rien sur l'appareil de Kris.
- **Une action sur un appareil** (piloter le lecteur qui joue sur le PC, régler son volume) : c'est
  l'`emory-hands` de cet appareil.

## `emory-hands` sur un appareil

Un seul programme, `emory-hands`, le même sur chaque appareil (un par appareil), à côté de
`emory-ear` et `emory-mouth` (voir le [README](README.md)). Il est fait de modules, **un par
standard** ; chaque appareil n'active que les siens dans sa configuration, et les modules non
activés ne sont pas compilés :

```toml
# PC de Kris
device = "office"
modules = ["mpris", "commands"]
```

- Au démarrage, `emory-hands` annonce sur le bus les modules actifs et leurs actions ; `mind` en fait
  des outils pour le LLM, et seulement ceux-là. Demander de la musique là où il n'y a pas de
  lecteur donne une réponse simple, pas une erreur.
- Une commande arrive par le bus (demande avec réponse NATS, sujets `hands.<appareil>.<action>`),
  `emory-hands` l'exécute et répond avec le résultat. Un appareil éteint répond par un délai dépassé ;
  Emory le dit simplement.
- **Un module parle un standard, pas un logiciel.** Il marche avec tout ce qui respecte ce
  standard ; on n'écrit pas un module par application.

## Le premier module : MPRIS

MPRIS est le standard des lecteurs multimédia sous Linux (par D-Bus) : `poste music`, Spotify,
un navigateur le parlent. Avec lui, Emory sait sur le PC :

- lecture, pause, suivant, précédent, arrêt, volume ;
- dire ce qui joue : « c'est quoi ce morceau ? » ;
- **ouvrir un morceau, un album, une playlist** (`OpenUri`), après que `mind` l'a trouvé dans
  Navidrome par l'API Subsonic : « mets du Daft Punk ».
- **baisser la musique quand Emory parle**, et la remonter après : `voice` prévient `emory-hands`
  de l'appareil où elle parle (`voice.<appareil>.speaking`, voir [`contract.md`](contract.md)).

Il manque une chose côté `poste music` (dépôt `poste`, module `music`) : accepter `OpenUri` avec
une adresse de morceau, d'album ou de playlist Navidrome. C'est une évolution de ce dépôt, pas
d'Emory, utile à tout lecteur qui voudrait l'ouvrir.

Mis à jour le 2026-10-13 : ouvrir un album ou une playlist dans `poste music` passe désormais par
le module `commands` (plus bas), qui appelle sa ligne de commande. MPRIS garde ce qui est commun à
tous les lecteurs (lecture, pause, suivant, précédent, ce qui joue, la baisse pendant qu'Emory
parle) ; `OpenUri` n'est plus nécessaire pour `poste music`, et la question ouverte qui en dépendait
est close.

## Le module `commands` (2026-10-13)

Décidé avec Kris le 2026-10-13 : un module générique qui transforme des lignes de configuration en
actions. La configuration de l'appareil déclare chaque action (son nom, sa description pour le LLM,
ses arguments typés, la commande à lancer) ; `emory-hands` les annonce comme toute action, et
`mind` en fait des outils. **Aucun programme n'est nommé dans le code** : `poste music` devient
quelques lignes de configuration sur le PC de Kris (exemple plus bas).

Ce que Kris pourra dire, une fois `poste music` déclaré :

- « Mets l'album Random Access Memories. »
- « Lance ma playlist du soir en aléatoire. »
- « Fais-moi un mix autour de Daft Punk. »
- « Monte le son. » / « Mets le volume à trente. »
- « C'est quoi ce morceau ? »
- « J'aime bien celui-là, mets-le en favori. »

### Une action

```toml
[[commands.action]]
name = "music_play"                        # [a-z0-9_], 32 caractères au plus, unique sur l'appareil
description = "Lance un album, une playlist, un artiste ou un morceau trouvé par music_search."
argv = ["poste", "music", "play", "{kind}", "{id}", "{shuffle}"]
timeout_ms = 5000                          # par défaut 5000, de 100 à 30000
output = "none"                            # "none", "text" ou "json" ; par défaut "text"
confirm = false                            # par défaut false
# cwd = "~"                                # par défaut le dossier personnel de l'utilisateur
# env = { LANG = "fr_FR.UTF-8" }           # ajouté à l'environnement réduit (plus bas)

[commands.action.arguments.kind]
type = "enum"
values = ["playlist", "album", "artist", "track"]
description = "La sorte d'élément."

[commands.action.arguments.id]
type = "string"
pattern = "^[A-Za-z0-9]{1,64}$"
description = "L'identifiant donné par music_search, sans la sorte."

[commands.action.arguments.shuffle]
type = "boolean"
when_true = "--shuffle"
optional = true
description = "Lecture aléatoire."
```

**Les arguments.** Quatre types, chacun avec sa `description` (pour le LLM) et `optional` (par
défaut `false`) :

| Type | Réglages | Vérifié par `emory-hands` |
|---|---|---|
| `string` | `max_length` (par défaut 200), `pattern` (expression régulière, facultative), `allow_dash` (par défaut `false`) | longueur, motif, pas de caractère de contrôle ni de saut de ligne, pas de `-` en tête sauf `allow_dash` |
| `integer` | `min`, `max` (obligatoires) | bornes |
| `enum` | `values` (obligatoire) | valeur dans la liste |
| `boolean` | `when_true` (le mot mis dans la ligne quand c'est vrai) | rien d'autre |

Un argument inconnu, absent alors qu'il est requis, ou qui ne passe pas sa vérification : l'action
ne part pas, la réponse dit pourquoi (`ok: false`, l'erreur nomme l'argument), et le LLM peut le
dire ou corriger. `mind` en fait le schéma JSON annoncé (`arguments` de l'`Announce`) ; la
vérification de `emory-hands` est la seule qui compte, `mind` ne fait que relayer.

**Le gabarit `argv`.** Une liste, jamais une chaîne :

- le premier élément est le programme, littéral : un `{…}` y est refusé au démarrage. Il est
  cherché dans le `PATH` au démarrage ; introuvable, l'action n'est pas annoncée et le journal le
  dit (les autres actions marchent) ;
- `{nom}` dans un élément est remplacé par la valeur de l'argument. Un élément peut mêler texte et
  valeurs (`"--size={size}"`) ; un élément dont un argument facultatif est absent est retiré en
  entier ; un booléen faux retire son élément, un booléen vrai le remplace par `when_true` ;
- un `{nom}` qui ne correspond à aucun argument déclaré est refusé au démarrage, comme un argument
  déclaré qui n'apparaît nulle part.

**La sortie.**

- `none` : seul le code de sortie compte ; la réponse est `{ "ok": true }`.
- `text` : la sortie standard, en UTF-8 (les octets illisibles remplacés), sans les séquences de
  couleur, coupée à `max_chars` (par défaut 1000) avec une marque de coupure.
- `json` : la sortie standard lue comme du JSON. `fields` (facultatif) garde ces seules clés de
  l'objet, ou de chaque objet d'une liste ; `max_items` (par défaut 10) coupe une liste ; le
  résultat entier est ensuite plafonné à `max_chars` (par défaut 4000). Une sortie qui n'est pas du
  JSON donne `ok: false` (« sortie illisible »).
- Un code de sortie non nul donne `ok: false`, avec la dernière ligne non vide de la sortie
  d'erreur (200 caractères au plus) comme `error`. La sortie standard est lue jusqu'à
  `max_output_bytes` (par défaut 1 Mo), le reste est jeté.

### L'exécution

- **Sans shell.** `emory-hands` lance le programme directement (`execve`), avec la liste
  d'arguments construite plus haut. Aucune valeur ne passe jamais dans une chaîne interprétée : un
  `;`, un `$(…)` ou une apostrophe restent des caractères d'un argument.
- **Le tiret en tête.** Une valeur qui commence par `-` serait lue comme une option par le
  programme appelé (`--help`, ou pire). Elle est refusée, sauf pour un argument marqué
  `allow_dash = true` avec un `pattern` qui la borne (le volume `-5`). Quand le programme le
  permet, mettre `--` avant les valeurs libres (la recherche, plus bas).
- **Un environnement réduit** : seulement `PATH`, `HOME`, `LANG`, `XDG_RUNTIME_DIR`,
  `DBUS_SESSION_BUS_ADDRESS`, `WAYLAND_DISPLAY` quand ils existent, plus ce que dit `env`.
  L'entrée standard est vide (`/dev/null`).
- **Le délai.** Au-delà de `timeout_ms`, tout le groupe de processus est tué ; la réponse dit
  « délai dépassé ». Une action ne tourne qu'une fois à la fois (une demande pendant qu'elle tourne
  attend son tour, dans le délai).
- **La configuration est à l'utilisateur seul** : `emory-hands` refuse de démarrer si son fichier
  est modifiable par un autre utilisateur ou un groupe, et le dit. Le fichier ne se recharge qu'au
  redémarrage.
- **Les journaux** : le nom de l'action, la durée, le code de sortie, la taille de la sortie ;
  jamais les valeurs des arguments ni la sortie (elles portent ce que l'utilisateur a demandé).

### Ce qui ne s'exécute jamais

- **Aucune commande libre du LLM.** Le LLM choisit une action annoncée et donne des valeurs
  d'arguments ; il ne peut ni écrire une ligne de commande, ni nommer un programme, ni ajouter une
  action. Il n'y a pas d'action « shell » et le module refuse d'en déclarer une dont le programme
  est un shell connu (`sh`, `bash`, `zsh`, `fish`, `dash`, `env`, `xargs`) : une telle ligne
  rendrait tout le reste inutile. C'est un garde-fou, pas une barrière : un programme qui exécute
  ses arguments reste possible, et c'est à la configuration de ne pas en déclarer.
- **Rien qui ne vienne pas de `mind`** : seul `mind` publie sur `hands.*` (droits du bus, voir le
  [README](README.md)).

### La confirmation

Une action marquée `confirm = true` ne part qu'après le oui de l'utilisateur, **tenu par `mind`**,
comme le dictionnaire (voir [`mind.md`](mind.md), « Le dictionnaire »). Une différence pour ne pas
doubler les outils (chacun coûte des jetons à chaque requête) : il n'y a pas d'outil « proposer ».

- Le premier appel d'une action à confirmer ne part pas vers l'appareil : `mind` le note dans la
  conversation (l'action et ses arguments) et répond au LLM « Demande d'abord à l'utilisateur, en
  disant ce que tu vas faire, puis rappelle cet outil avec les mêmes arguments après son oui. »
- Un appel avec **les mêmes arguments**, à **un tour plus tard** de la même conversation, part.
  Des arguments différents, ou le même tour : c'est une nouvelle proposition.
- Les propositions meurent avec la conversation ; une action partie consomme la sienne.
- Le registre est celui du dictionnaire, rendu commun : le dictionnaire passe dessus dans la même
  unité, sans changer ce que voit le LLM.
- Une action à confirmer n'est jamais offerte à un tour qui ne vient pas de l'utilisateur (une
  tâche de l'entrée interne, voir [`events.md`](events.md)) : personne n'est là pour dire oui.

`emory-hands` annonce `confirm` avec l'action ; il ne vérifie rien lui-même (il ne voit pas la
conversation), et fait confiance à `mind`, seul à pouvoir lui parler.

### Ce que voit le LLM

Chaque action annoncée devient un outil `hands_<module>_<action>` : `hands_mpris_pause`,
`hands_commands_music_play`. La description est celle de la configuration (courte : elle part avec
chaque requête). Quand la même action existe sur plusieurs appareils, l'outil gagne un argument
`device` (une énumération des appareils qui l'ont), qui vaut par défaut l'appareil de la
conversation ; sinon l'action part sur le seul appareil qui l'a. Les outils changent quand un
appareil s'annonce : en mode `acp`, à la conversation suivante (la session garde la liste de son
ouverture) ; en mode `api`, à la requête suivante.

`mind` attend la réponse `timeout_ms` de l'action plus 500 ms (l'annonce porte `timeout_ms`, voir
[`contract.md`](contract.md)), 2 s pour une action qui ne le dit pas.

### L'exemple : `poste music` sur le PC de Kris

Écrit ici, dans la spec, et nulle part dans le code ni les exemples livrés. `poste music` lit une
copie locale de la bibliothèque Navidrome ; mesuré le 2026-10-13, `poste music search --json daft`
rend 14 éléments en 13,7 Ko, chacun avec `id` (`album:32JyYwUDTygzp4jq1bxMrl`), `name`
(`Random Access Memories`), `subtitle` (`Album · Daft Punk · 2013`) et des champs pour le lanceur
(icônes, commandes) qu'on ne garde pas. `poste music now --json` rend `state`, `title`, `artist`,
`album`, `volume`, `starred` et d'autres.

```toml
device = "office"
modules = ["mpris", "commands"]

[[commands.action]]
name = "music_search"
description = "Cherche dans la bibliothèque musicale. Rend des éléments dont l'id est sorte:identifiant."
argv = ["poste", "music", "search", "--json", "--", "{query}"]
output = "json"
fields = ["id", "name", "subtitle"]
max_items = 8
[commands.action.arguments.query]
type = "string"
max_length = 100
description = "Un artiste, un album, un morceau ou une playlist."

[[commands.action]]
name = "music_play"
description = "Lance un élément trouvé par music_search."
argv = ["poste", "music", "play", "{kind}", "{id}", "{shuffle}"]
output = "none"
[commands.action.arguments.kind]
type = "enum"
values = ["playlist", "album", "artist", "track"]
description = "La sorte, avant le deux-points de l'id."
[commands.action.arguments.id]
type = "string"
pattern = "^[A-Za-z0-9]{1,64}$"
description = "L'identifiant, après le deux-points."
[commands.action.arguments.shuffle]
type = "boolean"
when_true = "--shuffle"
optional = true
description = "Lecture aléatoire."

[[commands.action]]
name = "music_queue"
description = "Ajoute un élément trouvé par music_search à la fin de la file."
argv = ["poste", "music", "queue", "{kind}", "{id}"]
output = "none"
[commands.action.arguments.kind]
type = "enum"
values = ["playlist", "album", "artist", "track"]
description = "La sorte, avant le deux-points de l'id."
[commands.action.arguments.id]
type = "string"
pattern = "^[A-Za-z0-9]{1,64}$"
description = "L'identifiant, après le deux-points."

[[commands.action]]
name = "music_playlists"
description = "Liste les playlists."
argv = ["poste", "music", "playlists", "--json"]
output = "json"
fields = ["id", "name"]
max_items = 30

[[commands.action]]
name = "music_mix"
description = "Calcule un mix et le lit : perso, redécouvertes, ou autour d'un morceau, album, artiste, genre."
argv = ["poste", "music", "mix", "{kind}", "--", "{around}"]
output = "none"
timeout_ms = 10000
[commands.action.arguments.kind]
type = "enum"
values = ["me", "rediscover", "track", "album", "artist", "genre"]
description = "me : mix perso ; rediscover : albums pas écoutés depuis longtemps."
[commands.action.arguments.around]
type = "string"
max_length = 100
optional = true
description = "Le nom ou l'identifiant, sauf pour me et rediscover."

[[commands.action]]
name = "music_volume"
description = "Règle le volume : un nombre de 0 à 100, ou +5, -5."
argv = ["poste", "music", "volume", "{change}"]
output = "text"
[commands.action.arguments.change]
type = "string"
pattern = "^([0-9]{1,2}|100|[+-][0-9]{1,2})$"
allow_dash = true
description = "0 à 100, ou un écart signé."

[[commands.action]]
name = "music_now"
description = "Ce qui joue : titre, artiste, album, état, volume."
argv = ["poste", "music", "now", "--json"]
output = "json"
fields = ["state", "title", "artist", "album", "volume", "starred"]

[[commands.action]]
name = "music_like"
description = "Met le morceau en cours en favori."
argv = ["poste", "music", "like"]
output = "none"
```

Pour illustrer `confirm` (une action qu'on ne veut pas voir partir sur un malentendu) :

```toml
[[commands.action]]
name = "suspend"
description = "Met le PC en veille."
argv = ["systemctl", "suspend"]
output = "none"
confirm = true
```

À vérifier en construisant : que `poste music search` et `mix` acceptent `--` avant le texte (le
parseur de `poste`, clap, le fait d'ordinaire). Sinon, retirer `--` : le motif interdit déjà le
tiret en tête.

## L'ordre de construction (2026-10-13)

Une unité par fil de travail, chacune petite et vérifiable seule. `emory-hands` n'existe pas
encore : les premières le créent.

1. **H1. Le squelette d'`emory-hands`** (dossier `hands/`) : configuration (`device`, `modules`,
   `[nats]`), connexion au bus, `Announce` au démarrage et sur `announce.request`, abonnement à
   `hands.<appareil>.>`, routage d'une demande vers le module, réponse `ok`/`error`, module de
   test `echo` (feature de test seulement). Vérifié : tests avec un NATS lancé par le test (comme
   `mind`), une demande à une action inconnue répond `ok: false`.
2. **H2. Les mains dans `mind`** : un module `hands` du serveur MCP qui suit les `Announce`
   (`hands.*.announce`), en fait des outils `hands_<module>_<action>` avec l'argument `device`
   quand il le faut, et relaie l'appel (délai de l'annonce plus 500 ms). Les actions
   `confirm = true` ne sont **pas offertes** tant que H4 n'est pas fait. Vérifié : faux appareil
   sur un NATS de test, outil listé, appel relayé, appareil muet donne une erreur dite simplement.
3. **H3. Le module `commands`** : lecture et vérification de la configuration (refus au démarrage
   avec le nom de la clé fautive), gabarit `argv`, types d'arguments, exécution sans shell, délai,
   environnement réduit, sortie `none`/`text`/`json`, droits du fichier. Vérifié : tests unitaires
   sur le gabarit et la validation (un `;`, un `$(…)`, un `-x` en tête, un saut de ligne), tests
   d'exécution sur de petits programmes du système (`true`, `false`, `printf`, `sleep` pour le
   délai), refus des shells.
4. **H4. La confirmation tenue par `mind`** : registre commun avec le dictionnaire, premier appel
   noté, second appel au tour suivant avec les mêmes arguments. Vérifié : tests du registre (même
   tour refusé, arguments différents refusés, conversation fermée), le dictionnaire inchangé vu du
   LLM (ses tests passent tels quels).
5. **H5. Le module `mpris`** : lecture, pause, suivant, précédent, arrêt, volume, ce qui joue, et
   la baisse pendant `voice.<appareil>.speaking`. Vérifié : tests contre un faux lecteur sur un
   bus D-Bus de session lancé par le test, jamais contre le lecteur de Kris.
6. **H6. La configuration de Kris** (hors dépôt) : l'exemple `poste music` plus haut, essayé en
   vrai par Kris.

## Plus tard

- **Les appareils de la maison** : Kris n'en a aucun aujourd'hui. Pas de Home Assistant (décidé
  le 2026-10-11) : on ne veut pas installer un autre assistant pour avoir accès aux appareils.
  On s'inspire de ce qu'il a fait, sans en dépendre. Le jour venu, on choisit des appareils
  **Matter**, le standard commun de la domotique : un seul module Matter les pilote tous, quelle
  que soit la marque.
- D'autres modules standard selon les besoins : volume système, notifications.
- Le module Subsonic sur le serveur (appareil `server`), pour un appareil qui n'a pas
  `poste music`.
- Les minuteurs et rappels d'Emory vivent dans `mind`, pas sur l'appareil (voir
  [`time.md`](time.md)).

## Questions ouvertes

1. ~~Le format des adresses passées à `OpenUri`~~ : close le 2026-10-13, `poste music` est piloté
   par le module `commands`.
2. Le nom des outils : `hands_commands_music_play` est long. Recommandation : le garder tel quel
   (une règle unique, aucune collision possible entre modules) ; raccourcir seulement si le LLM se
   trompe d'outil à l'usage.
3. Faut-il une confirmation pour `music_stop` (qui vide la file) ? Recommandation : ne pas
   déclarer `stop` dans `commands` ; l'arrêt passe par MPRIS, et une pause suffit presque toujours.
4. Faut-il que `emory-hands` relise sa configuration sans redémarrer ? Recommandation : non en
   version 1 ; un redémarrage du service suffit et garde la vérification au démarrage.
5. Le garde-fou contre les shells : liste de noms, contournable. Recommandation : le garder tel
   quel (il attrape l'erreur de bonne foi) et ne rien promettre de plus ; la sécurité réelle est
   que la configuration n'est écrite que par Kris.
