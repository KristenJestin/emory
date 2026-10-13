# Le contrat

Statut : décidé avec Kris le 2026-10-11. Noms passés en anglais le 2026-10-11 ; `last` ajouté à `Say` le 2026-10-11. `hearing.vocabulary.add` ajouté le 2026-10-13. `voice.cue` ajouté le 2026-10-13. Proposés le 2026-10-13 (brouillon, à relire avec Kris) : les actions dont le nom porte un point et `confirm`, `timeout_ms` dans l'annonce, `mind.event.<source>`, `mind.timer.changed` et `mind.timer.fired`.

Le contrat est la seule chose que les programmes d'Emory partagent : les sujets du bus et le format
de chaque message. Il vit dans `contract/`, une crate Rust sans réseau (des types `serde`, des
constantes, des fonctions qui fabriquent les sujets) ; chaque programme en dépend et ne connaît
rien d'autre des autres.

## Les règles communes

- **Les messages sont en JSON**, lisibles tels quels avec `nats sub`. Les noms de champs sont en
  anglais, en `snake_case`, et les énumérations aussi. Les dates sont en RFC 3339 avec le décalage
  (`2026-10-11T21:14:03.120+02:00`).
- **Le son n'est pas en JSON** : la charge d'un message audio est le son brut, ses informations
  sont dans les en-têtes NATS (plus bas).
- **Le tour relie tout.** `emory-ear` donne un identifiant à chaque appui (`turn`, un ULID) ; il suit
  le tour d'un bout à l'autre : le son, les phrases, le tour transcrit, les phrases à dire, la voix
  qui revient. C'est lui qui permet d'arrêter la bonne réponse, et de retrouver un échange dans
  les journaux.
- **Un nom d'appareil** (`office`) est fait de minuscules, de chiffres et de tirets : il entre dans
  les sujets.
- **Le contrat grandit sans casser** : un champ nouveau est facultatif, et un programme ignore les
  champs qu'il ne connaît pas. Changer le sens d'un champ, ou en retirer un, change la version
  majeure de la crate, et tous les programmes suivent.
- NATS livre au plus une fois, sans rejouer : un message perdu est perdu. Le son est numéroté
  pour qu'un trou se voie ; le reste est petit et passe sur le réseau local.

## Le son

| | Format | Morceau |
|---|---|---|
| De `emory-ear` à `hearing` | PCM 16 bits signé, petit-boutiste, mono, 16 kHz | 20 ms (640 octets) |
| De `voice` à `emory-mouth` | PCM 16 bits signé, petit-boutiste, mono, à la fréquence de la voix (24 kHz avec Pocket) | libre |

Pas de compression en version 1 : 256 kbit/s par flux, rien pour NATS ni pour le réseau local.
16 kHz est ce que demandent le VAD et Parakeet ; `emory-ear` rééchantillonne son micro. `voice`
envoie la voix à sa fréquence d'origine, et `emory-mouth` la joue telle qu'annoncée.

Les en-têtes d'un message audio :

| En-tête | Valeur |
|---|---|
| `Emory-Turn` | l'identifiant du tour |
| `Emory-Seq` | le numéro du morceau dans le flux, à partir de 0 |
| `Emory-Format` | `s16le;16000;1` (encodage ; fréquence ; canaux) |
| `Emory-Sentence` | au retour seulement : le numéro de la phrase dite |
| `Emory-End` | au retour seulement : `1` sur le dernier morceau d'une phrase |

## Les sujets

| Sujet | Qui publie | Qui écoute | Message |
|---|---|---|---|
| `ear.<nom>.button` | `emory-ear` | `hearing`, `mind` | `Button` |
| `ear.<nom>.audio` | `emory-ear` | `hearing` | son |
| `ear.<nom>.announce` | `emory-ear` | `mind` | `Announce` |
| `mouth.<nom>.audio` | `voice` | `emory-mouth` | son |
| `mouth.<nom>.announce` | `emory-mouth` | `mind` | `Announce` |
| `hearing.sentence` | `hearing` | `mind` | `Sentence` |
| `hearing.turn` | `hearing` | `mind` | `Turn` |
| `hearing.vocabulary.add` | `mind` | `hearing` | `VocabularyAdd`, demande avec réponse `VocabularyReply` |
| `voice.say` | `mind` | `voice` | `Say` |
| `voice.stop` | `mind` | `voice` | `Stop` |
| `voice.cue` | `mind` | `voice` | `Cue` |
| `voice.<nom>.speaking` | `voice` | `emory-hands` de l'appareil | `Speaking` |
| `hands.<nom>.announce` | `emory-hands` | `mind` | `Announce` |
| `hands.<nom>.<module>.<action>` | `mind` | `emory-hands` | demande avec réponse `ActionReply` |
| `mind.event.<source>` | un programme déclaré (jamais un organe) | `mind` | `Event`, en publication ou demande avec réponse `EventReply` |
| `mind.timer.changed` | `mind` | personne aujourd'hui (un écran plus tard) | `Timers` |
| `mind.timer.fired` | `mind` | personne aujourd'hui | `TimerFired` |
| `announce.request` | `mind` | `emory-ear`, `emory-mouth`, `emory-hands` | vide |

Dans le code, les sujets et les abonnements viennent de `contract::subjects` :
`subjects::button(&device)`, `subjects::ear_audio`, `subjects::ear_announce`,
`subjects::mouth_audio`, `subjects::mouth_announce`, `subjects::hands_announce`,
`subjects::speaking`, `subjects::action`, les constantes `SENTENCE`, `TURN`, `VOCABULARY_ADD`, `SAY`, `STOP`, `CUE`,
`ANNOUNCE_REQUEST`, `TIMER_CHANGED`, `TIMER_FIRED`, `subjects::event(&source)`, et les abonnements à tous les appareils `ALL_BUTTONS`, `ALL_EAR_AUDIO`,
`ALL_EAR_ANNOUNCES`, `ALL_MOUTH_ANNOUNCES`, `ALL_HANDS_ANNOUNCES`, `ALL_EVENTS` (`mind.event.*`, proposé le 2026-10-13). `device_of_subject` retrouve
l'appareil d'un sujet qui commence par `ear`, `mouth`, `hands` ou `voice`.

### `Button`

```json
{ "turn": "01K…", "state": "start", "at": "2026-10-11T21:14:03.120+02:00" }
```

`state` vaut `start` ou `end`. Le son du tour part entre les deux. Une oreille qui meurt entre les
deux n'enverra jamais `end` : `hearing` ferme le tour de lui-même après 3 s sans son. Une oreille
qui continue d'envoyer du son sans jamais envoyer `end` ne tient pas le tour ouvert pour toujours :
`hearing` le ferme aussi après `max_turn` (120 s par défaut, ajouté le 2026-10-11), comme avec `end`,
avec un avertissement dans son journal.

### `Sentence`

Une phrase fermée par le VAD, transcrite pendant que Kris parle.

```json
{ "turn": "01K…", "device": "office", "index": 0, "text": "on en est où sur emory",
  "start": "…", "end": "…" }
```

### `Turn`

Le tour entier, au second appui, une fois la dernière phrase transcrite. C'est l'événement de
[`hearing.md`](hearing.md), réduit à ce que la version 1 sait faire :

```json
{
  "turn": "01K…",
  "device": "office",
  "start": "…",
  "end": "…",
  "text": "on en est où sur emory et les oreilles",
  "wake": "button",
  "speaker": { "voice": "kris", "person": "kris", "origin": "known", "confidence": 1.0 },
  "context": []
}
```

- `start` et `end` suivent l'horloge de l'oreille quand elle en a une (`start` du bouton `start`,
  `end` du bouton `end`), et celle du serveur sinon : silence, remplacement par un nouveau tour,
  durée maximale, arrêt de `hearing` (ajouté le 2026-10-11).
- `text` est vide quand rien n'a été entendu : l'esprit sait quand même que le tour est fini.
- En version 1, c'est le casque de Kris : `speaker` est Kris, d'origine `known`, et `wake`
  vaut `button`. Les autres champs de `hearing.md` (`transcription_confidence`, `context`) et
  les autres voix arrivent avec l'oreille de pièce, sans casser le contrat. `speaker` vaut
  `null` quand la voix n'est pas reconnue. `wake` vaut `button`, `word` ou `follow_up` ; `origin`
  vaut `known` ou `inferred`.

### `Say` et `Stop`

```json
{ "turn": "01K…", "device": "office", "index": 0, "text": "Les oreilles sont spécifiées.", "last": false }
{ "turn": "01K…", "device": "office" }
```

`mind` envoie la réponse phrase par phrase, numérotées dans le tour ; `voice` les dit dans
l'ordre, sur l'appareil nommé. `Stop` jette tout ce que `voice` n'a pas encore dit pour ce tour.
`emory-mouth`, lui, coupe le son de lui-même dès qu'on appuie.

`last` (facultatif, `false` par défaut, ajouté le 2026-10-11) vaut `true` sur la dernière phrase de
la réponse. Alors `voice` sait que rien ne suivra : elle envoie `Speaking` `end` dès que cette
phrase a fini d'être jouée, sans attendre. Tant qu'aucune phrase n'est marquée `last`, `voice` garde
un court délai de grâce après le dernier son avant de dire qu'elle a fini, au cas où `mind` enverrait
une phrase de plus ; ce délai reste le repli quand `last` n'arrive jamais.

### `Cue`

Ajouté le 2026-10-13. Le signal d'attente : ce qu'Emory fait entendre pendant que la réponse se
prépare, pour que l'utilisateur sache qu'elle l'a entendu (voir [`mind.md`](mind.md), « Le signal
d'attente »).

```json
{ "turn": "01K…", "device": "office", "kind": "tone" }
{ "turn": "01K…", "device": "office", "kind": "pulse" }
{ "turn": "01K…", "device": "office", "kind": "speech", "text": "Je regarde." }
```

- `kind` vaut `tone` (le son court de deux notes montantes), `pulse` (une pulsation douce que
  `voice` répète jusqu'à la première phrase de la réponse, ou au `Stop`) ou `speech` (une phrase à
  dire, dans `text`, qui est requis dans ce cas et ignoré sinon).
- Ce n'est pas une phrase de la réponse : `voice` le joue avant la première phrase du tour et
  **jamais après** ; un `Cue` qui arrive une fois la première phrase reçue, ou pour un tour
  arrêté ou remplacé, est ignoré. La première `Say` du tour met fin à la pulsation.
- Le son part dans le même flux que la voix (`mouth.<nom>.audio`), avec les mêmes en-têtes : le
  même tour, la numérotation `Emory-Seq` qui continue celle de la réponse, `Emory-Sentence` à
  4294967295 (`u32::MAX`, qu'aucune phrase de la réponse ne porte), `Emory-End` sur chaque son
  d'un coup (le ton, une pulsation) ou sur la fin de la phrase. `emory-mouth` n'a rien de
  nouveau à faire.
- `voice` publie `Speaking` pour une phrase d'attente (la musique baisse), pas pour un ton ni une
  pulsation.
- Un `kind` que `voice` ne connaît pas est refusé et journalisé, comme tout message illisible.

### `Speaking`

```json
{ "state": "start" }
```

Publié par `voice` quand elle commence et finit de parler sur un appareil : `emory-hands` de cet
appareil baisse la musique, puis la remonte. `voice` ne commande pas `emory-hands` : elle dit ce
qu'elle fait, et `emory-hands` en fait ce qu'il veut. Seul `mind` publie sur `hands.*`.

### `Announce`

```json
{
  "device": "office",
  "program": "hands",
  "version": "0.1.0",
  "modules": ["mpris"],
  "actions": [
    { "name": "mpris.pause", "description": "Met la musique en pause.", "arguments": {} }
  ]
}
```

Proposé le 2026-10-13 (voir [`hands.md`](hands.md)) : une action peut porter `confirm` (facultatif,
`false` par défaut : `mind` ne l'envoie qu'après le oui de l'utilisateur) et `timeout_ms`
(facultatif : le temps que l'action peut prendre ; `mind` attend ce temps plus 500 ms, et 2 s
sans lui) :

```json
{ "name": "commands.suspend", "description": "Met le PC en veille.", "arguments": {},
  "confirm": true, "timeout_ms": 5000 }
```

`program` vaut `ear`, `mouth` ou `hands`. Au démarrage, et en réponse à `announce.request` que
`mind` envoie au sien : un esprit qui redémarre retrouve ainsi tout ce qui tourne. `arguments` est
un schéma JSON ; `mind` en fait les outils du LLM, et seulement ceux-là. `emory-ear` et
`emory-mouth` annoncent leurs modules (`microphone`, `shortcut`, `speaker`…), sans actions.

### Les actions de `emory-hands`

Une action est une demande avec réponse NATS sur `hands.<nom>.<action>` (`hands.office.mpris.pause`),
avec les arguments en JSON. Le nom d'une action est `<module>.<action>`, chaque partie en
`[a-z0-9_]` (`commands.music_play`, 32 caractères au plus pour l'action) : le sujet a donc un
élément de plus que l'appareil, et `emory-hands` s'abonne à `hands.<nom>.>` (précisé le
2026-10-13). La réponse (`ActionReply`) :

```json
{ "ok": true, "result": { "title": "One More Time", "artist": "Daft Punk" } }
{ "ok": false, "error": "aucun lecteur ouvert" }
```

`mind` attend 2 s au plus (`ACTION_TIMEOUT`), ou le `timeout_ms` de l'action plus 500 ms quand
l'annonce le donne ; un appareil éteint ne répond pas, et Emory le dit simplement. Un argument
refusé par `emory-hands` donne `ok: false` avec une `error` qui nomme l'argument.

### `VocabularyAdd` et `VocabularyReply`

Ajouté le 2026-10-13. Un mot à ajouter au vocabulaire de `hearing`, une demande avec réponse NATS
sur `hearing.vocabulary.add`. `mind` ne l'envoie qu'une fois que l'utilisateur a dit oui (voir
[`mind.md`](mind.md)).

```json
{ "word": "Grafana", "heard_as": "gras fana" }
{ "ok": true, "known": false, "in_use": true }
{ "ok": false, "known": false, "in_use": false, "error": "caractère refusé dans le mot : '/'" }
```

- `heard_as` (facultatif) : ce que la transcription a donné à la place ; `hearing` le garde en
  commentaire au-dessus du mot.
- `known` : le mot y était déjà (quelle que soit la casse) ; rien n'est écrit, et c'est un succès.
- `in_use` : la transcription s'en sert dès la phrase suivante. `false` quand `hearing` tourne en
  recherche gloutonne, qui ne prend pas de mots : le mot est écrit et compte au prochain démarrage.
- `error` : pourquoi le mot est refusé (vide, plus de 40 caractères, saut de ligne, `/`, `:`, `#`,
  pas de lettre…). `mind` attend 2 s au plus (`[dictionary] timeout_ms`).

### `Event` et `EventReply`

Proposé le 2026-10-13. L'entrée interne : un programme déclaré réveille `mind`, jamais la voix
(voir [`events.md`](events.md)). Publié sur `mind.event.<source>`, où `<source>` suit la règle des
noms d'appareil ; seule la clé de cette source peut y publier.

```json
{ "id": "01K…", "kind": "say", "text": "La sauvegarde de cette nuit a échoué.", "device": "office" }
{ "id": "01K…", "kind": "task", "text": "Le déploiement est fini. Regarde…", "quiet": false, "expires_in_s": 600 }
{ "ok": true, "queued": true }
{ "ok": false, "queued": false, "error": "source non déclarée" }
```

- `id` (obligatoire) : un ULID de l'émetteur ; un `id` déjà vu dans les 10 minutes est ignoré
  (réponse `ok: true`, `queued: false`).
- `kind` : `say` (dire `text` tel quel, 500 caractères au plus) ou `task` (une consigne pour le LLM,
  2 000 caractères au plus).
- `device`, `quiet` (`task` seulement), `expires_in_s` (600 par défaut, 86 400 au plus) : facultatifs.
- La réponse n'est envoyée que pour une demande ; `queued` dit que l'événement attend d'être dit,
  pas qu'il l'est.

### `Timers` et `TimerFired`

Proposé le 2026-10-13 (voir [`time.md`](time.md)). L'état des minuteurs et rappels, publié par
`mind` à chaque changement, pour un affichage :

```json
{ "items": [ { "id": 3, "kind": "timer", "label": "pâtes", "due": "2026-10-13T19:12:11+02:00",
               "device": "office" },
             { "id": 4, "kind": "reminder", "text": "appeler le garage",
               "due": "2026-10-14T09:00:00+02:00", "device": "office" } ] }
{ "id": 3, "kind": "timer", "label": "pâtes", "due": "2026-10-13T19:12:11+02:00", "device": "office",
  "late_s": 0 }
```

`kind` vaut `timer` ou `reminder` ; `label` et `text` sont facultatifs ; `late_s` dit le retard
d'une sonnerie (après un redémarrage de `mind`).

## Hors du contrat

Le verrouillage du bus (TLS, une nkey par programme et par appareil, les droits par sujet) se
règle dans la configuration de NATS, pas dans la crate. Les droits suivent la colonne « Qui
publie » du tableau des sujets. Pour l'entrée interne : chaque source a sa clé, qui ne publie que
sur `mind.event.<source>` (et reçoit sa réponse) ; `mind` s'abonne à `mind.event.*` sans pouvoir y
publier ; aucun organe n'y a de droit.
