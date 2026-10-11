# Le contrat

Statut : décidé avec Kris le 2026-10-11.

Le contrat est la seule chose que les programmes d'Emory partagent : les sujets du bus et le format
de chaque message. Il vit dans `contrat/`, une crate Rust sans réseau (des types `serde`, des
constantes, des fonctions qui fabriquent les sujets) ; chaque programme en dépend et ne connaît
rien d'autre des autres.

## Les règles communes

- **Les messages sont en JSON**, lisibles tels quels avec `nats sub`. Les noms de champs sont en
  français, en `snake_case`. Les dates sont en RFC 3339 avec le décalage
  (`2026-10-11T21:14:03.120+02:00`).
- **Le son n'est pas en JSON** : la charge d'un message audio est le son brut, ses informations
  sont dans les en-têtes NATS (plus bas).
- **Le tour relie tout.** Le poste donne un identifiant à chaque appui (`tour`, un ULID) ; il suit
  le tour d'un bout à l'autre : le son, les phrases, le tour transcrit, les phrases à dire, la voix
  qui revient. C'est lui qui permet d'arrêter la bonne réponse, et de retrouver un échange dans
  les journaux.
- **Un nom d'appareil** (`bureau`) est fait de minuscules, de chiffres et de tirets : il entre dans
  les sujets.
- **Le contrat grandit sans casser** : un champ nouveau est facultatif, et un programme ignore les
  champs qu'il ne connaît pas. Changer le sens d'un champ, ou en retirer un, change la version
  majeure de la crate, et tous les programmes suivent.
- NATS livre au plus une fois, sans rejouer : un message perdu est perdu. Le son est numéroté
  pour qu'un trou se voie ; le reste est petit et passe sur le réseau local.

## Le son

| | Format | Morceau |
|---|---|---|
| Du poste aux oreilles | PCM 16 bits signé, petit-boutiste, mono, 16 kHz | 20 ms (640 octets) |
| De la bouche au poste | PCM 16 bits signé, petit-boutiste, mono, à la fréquence de la voix (24 kHz avec Pocket) | libre |

Pas de compression en version 1 : 256 kbit/s par flux, rien pour NATS ni pour le réseau local.
16 kHz est ce que demandent le VAD et Parakeet ; le poste rééchantillonne son micro. La bouche
envoie la voix à sa fréquence d'origine, et le poste la joue telle qu'annoncée.

Les en-têtes d'un message audio :

| En-tête | Valeur |
|---|---|
| `Emory-Tour` | l'identifiant du tour |
| `Emory-Seq` | le numéro du morceau dans le flux, à partir de 0 |
| `Emory-Format` | `s16le;16000;1` (encodage ; fréquence ; canaux) |
| `Emory-Phrase` | au retour seulement : le numéro de la phrase dite |
| `Emory-Fin` | au retour seulement : `1` sur le dernier morceau d'une phrase |

## Les sujets

| Sujet | Qui publie | Qui écoute | Message |
|---|---|---|---|
| `poste.<nom>.bouton` | le poste | oreilles, esprit | `Bouton` |
| `poste.<nom>.audio` | le poste | oreilles | son |
| `poste.<nom>.audio.retour` | bouche | le poste | son |
| `poste.<nom>.annonce` | le poste | esprit | `Annonce` |
| `oreilles.phrase` | oreilles | esprit | `Phrase` |
| `oreilles.tour` | oreilles | esprit | `Tour` |
| `bouche.dire` | esprit | bouche | `Dire` |
| `bouche.stop` | esprit | bouche | `Stop` |
| `bouche.<nom>.parle` | bouche | la main de l'appareil | `Parle` |
| `main.<nom>.annonce` | la main | esprit | `Annonce` |
| `main.<nom>.<action>` | esprit | la main | demande avec réponse |
| `annonces.demande` | esprit | postes et mains | vide |

### `Bouton`

```json
{ "tour": "01K…", "etat": "debut", "quand": "2026-10-11T21:14:03.120+02:00" }
```

`etat` vaut `debut` ou `fin`. Le son du tour part entre les deux. Un poste qui meurt entre les
deux n'enverra jamais `fin` : les oreilles ferment le tour d'elles-mêmes après 3 s sans son.

### `Phrase`

Une phrase fermée par le VAD, transcrite pendant que Kris parle.

```json
{ "tour": "01K…", "poste": "bureau", "numero": 0, "texte": "on en est où sur emory",
  "debut": "…", "fin": "…" }
```

### `Tour`

Le tour entier, au second appui, une fois la dernière phrase transcrite. C'est l'événement de
[`oreilles.md`](oreilles.md), réduit à ce que la version 1 sait faire :

```json
{
  "tour": "01K…",
  "poste": "bureau",
  "debut": "…",
  "fin": "…",
  "texte": "on en est où sur emory et les oreilles",
  "reveil": "bouton",
  "locuteur": { "voix": "kris", "personne": "kris", "origine": "connue", "confiance": 1.0 },
  "contexte": []
}
```

- `texte` est vide quand rien n'a été entendu : l'esprit sait quand même que le tour est fini.
- En version 1, c'est le casque de Kris : `locuteur` est Kris, d'origine `connue`, et `reveil`
  vaut `bouton`. Les autres champs de `oreilles.md` (`confiance_transcription`, `contexte`) et
  les autres voix arrivent avec l'oreille de pièce, sans casser le contrat. `locuteur` vaut
  `null` quand la voix n'est pas reconnue.

### `Dire` et `Stop`

```json
{ "tour": "01K…", "poste": "bureau", "numero": 0, "texte": "Les oreilles sont spécifiées." }
{ "tour": "01K…", "poste": "bureau" }
```

L'esprit envoie la réponse phrase par phrase, numérotées dans le tour ; la bouche les dit dans
l'ordre, sur le poste nommé. `Stop` jette tout ce que la bouche n'a pas encore dit pour ce tour.
Le poste, lui, coupe le son de lui-même dès qu'on appuie.

### `Parle`

```json
{ "etat": "debut" }
```

Publié par la bouche quand elle commence et finit de parler sur un appareil : la main de cet
appareil baisse la musique, puis la remonte. La bouche ne commande pas la main : elle dit ce
qu'elle fait, et la main en fait ce qu'elle veut. Seul l'esprit publie sur `main.*`.

### `Annonce`

```json
{
  "appareil": "bureau",
  "programme": "main",
  "version": "0.1.0",
  "modules": ["mpris"],
  "actions": [
    { "nom": "mpris.pause", "description": "Met la musique en pause.", "arguments": {} }
  ]
}
```

Au démarrage, et en réponse à `annonces.demande` que l'esprit envoie au sien : un esprit qui
redémarre retrouve ainsi tout ce qui tourne. `arguments` est un schéma JSON ; l'esprit en fait les
outils du LLM, et seulement ceux-là. Le poste annonce ses modules (`micro`, `haut-parleur`), sans
actions.

### Les actions de la main

Une action est une demande avec réponse NATS sur `main.<nom>.<action>` (`main.bureau.mpris.pause`),
avec les arguments en JSON. La réponse :

```json
{ "ok": true, "resultat": { "titre": "One More Time", "artiste": "Daft Punk" } }
{ "ok": false, "erreur": "aucun lecteur ouvert" }
```

L'esprit attend 2 s au plus ; un appareil éteint ne répond pas, et Emory le dit simplement.

## Hors du contrat

Le verrouillage du bus (TLS, une nkey par programme et par appareil, les droits par sujet) se
règle dans la configuration de NATS, pas dans la crate. Les droits suivent la colonne « Qui
publie » du tableau des sujets.
