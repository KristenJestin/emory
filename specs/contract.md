# Le contrat

Statut : décidé avec Kris le 2026-10-11. Noms passés en anglais le 2026-10-11 ; `last` ajouté à `Say` le 2026-10-11.

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
| `voice.say` | `mind` | `voice` | `Say` |
| `voice.stop` | `mind` | `voice` | `Stop` |
| `voice.<nom>.speaking` | `voice` | `emory-hands` de l'appareil | `Speaking` |
| `hands.<nom>.announce` | `emory-hands` | `mind` | `Announce` |
| `hands.<nom>.<action>` | `mind` | `emory-hands` | demande avec réponse |
| `announce.request` | `mind` | `emory-ear`, `emory-mouth`, `emory-hands` | vide |

Dans le code, les sujets et les abonnements viennent de `contract::subjects` :
`subjects::button(&device)`, `subjects::ear_audio`, `subjects::ear_announce`,
`subjects::mouth_audio`, `subjects::mouth_announce`, `subjects::hands_announce`,
`subjects::speaking`, `subjects::action`, les constantes `SENTENCE`, `TURN`, `SAY`, `STOP`,
`ANNOUNCE_REQUEST`, et les abonnements à tous les appareils `ALL_BUTTONS`, `ALL_EAR_AUDIO`,
`ALL_EAR_ANNOUNCES`, `ALL_MOUTH_ANNOUNCES`, `ALL_HANDS_ANNOUNCES`. `device_of_subject` retrouve
l'appareil d'un sujet qui commence par `ear`, `mouth`, `hands` ou `voice`.

### `Button`

```json
{ "turn": "01K…", "state": "start", "at": "2026-10-11T21:14:03.120+02:00" }
```

`state` vaut `start` ou `end`. Le son du tour part entre les deux. Une oreille qui meurt entre les
deux n'enverra jamais `end` : `hearing` ferme le tour de lui-même après 3 s sans son.

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

`program` vaut `ear`, `mouth` ou `hands`. Au démarrage, et en réponse à `announce.request` que
`mind` envoie au sien : un esprit qui redémarre retrouve ainsi tout ce qui tourne. `arguments` est
un schéma JSON ; `mind` en fait les outils du LLM, et seulement ceux-là. `emory-ear` et
`emory-mouth` annoncent leurs modules (`microphone`, `shortcut`, `speaker`…), sans actions.

### Les actions de `emory-hands`

Une action est une demande avec réponse NATS sur `hands.<nom>.<action>` (`hands.office.mpris.pause`),
avec les arguments en JSON. La réponse :

```json
{ "ok": true, "result": { "title": "One More Time", "artist": "Daft Punk" } }
{ "ok": false, "error": "aucun lecteur ouvert" }
```

`mind` attend 2 s au plus (`ACTION_TIMEOUT`) ; un appareil éteint ne répond pas, et Emory le dit simplement.

## Hors du contrat

Le verrouillage du bus (TLS, une nkey par programme et par appareil, les droits par sujet) se
règle dans la configuration de NATS, pas dans la crate. Les droits suivent la colonne « Qui
publie » du tableau des sujets.
