# Emory

Statut : vue d'ensemble décidée avec Kris le 2026-10-10, l'architecture ajoutée le 2026-10-11, les noms passés en anglais le 2026-10-11.

Emory est l'assistant de Kris. Tout ce qui fait l'assistant est dans ce projet : les organes et
l'esprit. Sa mémoire à long terme n'y est pas : c'est Hippocampe, un projet à part.

## Ce qui est dans Emory

| Organe | Rôle | Spec |
|---|---|---|
| **Les oreilles** (`emory-ear` sur l'appareil, `hearing` sur le serveur) | transforment ce qui est dit en événements pour l'esprit | [`hearing.md`](hearing.md) (brouillon) |
| **La bouche** (`emory-mouth` sur l'appareil, `voice` sur le serveur) | fait parler Emory, et se tait quand Kris reprend la parole | [`voice.md`](voice.md) (brouillon) |
| **Les yeux** | ce qu'Emory voit | à écrire |
| **Les mains** (`emory-hands`) | ce qu'Emory fait dans le monde (appareils, services) | [`hands.md`](hands.md) (brouillon) |
| **L'esprit** (`mind`) | reçoit les événements des organes, décide, lit et écrit dans Hippocampe, répond | [`mind.md`](mind.md) (brouillon) |

**Tous les noms dans le code sont en anglais** (décidé le 2026-10-11, pour que le projet puisse
être vendu ou repris par d'autres) : programmes, dossiers, sujets du bus, types et champs JSON du
contrat, options. La métaphore du corps reste, en anglais. Les specs et les échanges avec Kris
restent en français ; les noms qu'elles citent sont ceux du code.

Les sujets du bus et le format des messages : [`contract.md`](contract.md).

## Sur chaque appareil : l'oreille, la bouche et les mains

**Toute l'intelligence vit sur le serveur ; un appareil ne fait que recevoir, envoyer et
exécuter** (décidé le 2026-10-11). Chaque appareil (le PC de Kris d'abord, plus tard un
Raspberry avec un micro et une enceinte) fait tourner jusqu'à trois petits programmes, tous
construits de la même façon partout et configurés par appareil, branchés au bus avec le nom de
l'appareil, sans modèle, sans état, sans décision (décidé le 2026-10-11) :

- **`emory-ear`**, pour entendre : un par microphone. Il capte le micro, envoie ce flux au
  serveur, et son déclencheur est un module : `shortcut` sur le PC, par le standard
  GlobalShortcuts du xdg-desktop-portal, en appui court pour basculer ou en appui maintenu pour
  parler comme la touche de dictée de Kris ; `button`, un vrai bouton sur un Raspberry ; plus
  tard `continuous`, pour un micro de pièce, où c'est le serveur qui trouve le mot de réveil ;
- **`emory-mouth`**, pour parler : un par haut-parleur. Il joue le flux qui revient du serveur, et
  jette le son d'un tour dès que le bouton est appuyé ;
- **`emory-hands`**, pour agir : un par appareil, un module par standard (voir
  [`hands.md`](hands.md)). Il exécute les commandes de l'esprit sur l'appareil. Les services
  réseau, comme la recherche dans Navidrome et plus tard Matter, tournent dans un `emory-hands`
  sur le serveur, sous le nom d'appareil `server`.

Chaque programme est **fait de modules, et chaque appareil n'active que les siens**, dans sa
configuration. Un module qu'un appareil n'utilise pas n'est pas compilé (features Cargo). Au
démarrage, chacun annonce sur le bus ce qu'il sait faire, et l'esprit ne propose au LLM que ce qui
existe. Ajouter une capacité, c'est écrire un module une fois, puis l'activer là où il sert.

Les séparer garde le son, qui est en temps réel, à l'abri d'une action qui plante ou qui traîne.

Côté serveur, les organes sont `hearing` (reçoit l'audio des oreilles et le transcrit), `voice`
(synthétise ce qu'Emory dit) et `mind` (l'esprit). Le serveur sait quelle oreille a entendu Kris,
et c'est par lui qu'Emory répond : `emory-ear` et `emory-mouth` ne sont que la prise de `hearing`
et de `voice` dans la pièce. Un site web pour le téléphone pourra plus tard se brancher au bus ;
il ne remplace pas `emory-ear` sur le PC, parce qu'un onglet ne capte pas un raccourci quand il
n'a pas le focus.

## Comment ça tourne

Décidé le 2026-10-11.

**Sur le serveur, des conteneurs Docker** déployés par Dokploy (depuis `~/Projects/home-infra`) :

| Conteneur | Rôle |
|---|---|
| `nats` | le bus de messages |
| `hearing` | le centre auditif : reçoit l'audio des oreilles, transcrit, publie le tour de parole |
| `mind` | l'esprit : garde la conversation, appelle le LLM et les outils, envoie les phrases à dire |
| `voice` | la voix : synthétise les phrases et renvoie l'audio à la bouche |
| un conteneur par moteur hors processus | voir « Les moteurs » |

**Sur chaque appareil, `emory-ear`, `emory-mouth` et `emory-hands`** (ceux dont l'appareil a
besoin), branchés au bus.

**Le bus : NATS.** Un courtier de messages, de la famille de RabbitMQ mais minimaliste : chaque
programme publie sur des sujets nommés (`hearing.turn`, `voice.say`…) et s'abonne à ceux qui
l'intéressent ; NATS livre aussitôt, en moins d'une milliseconde sur le réseau local, et fait
aussi les demandes avec réponse. Les organes ne se connaissent pas : ils ne connaissent que le
bus et les sujets. Chaque programme d'appareil s'y branche en TCP (client `async-nats`) ; un site web passera par le
WebSocket que NATS sait aussi servir. La persistance (JetStream) existe si un jour il en faut.

**Tout passe par NATS en version 1, le son compris** (décidé le 2026-10-11) : `emory-ear`
publie le micro en morceaux de 20 ms (`ear.<nom>.audio`), `voice` publie la voix d'Emory
(`mouth.<nom>.audio`). Un flux de voix fait environ 256 kbit/s, rien pour NATS. Le
transport du son est isolé dans un module de `emory-ear`, de `emory-mouth` et de `hearing` : s'il accroche à l'usage, il
passe sur une connexion dédiée (WebSocket, WebRTC) sans toucher au reste.

**Le bus est verrouillé dès le départ.** Le serveur NATS ne tourne que sur le serveur ; un
appareil n'a qu'une bibliothèque cliente, et c'est lui qui se connecte : rien n'est ouvert sur le
PC. Toutes les connexions sont en TLS ; chaque appareil et chaque organe a sa propre clé (nkey),
avec des droits par sujet : l'oreille du bureau ne publie que sur `ear.office.*`, les mains du
bureau n'écoutent que `hands.office.*`, seul `mind` publie sur `hands.*`. Un appareil volé ou
compromis ne sort pas de ses sujets.

Un échange, avec l'oreille à bouton :

```
1. appui    emory-ear → ear.<nom>.button (début), puis ear.<nom>.audio (le flux)
   pendant  hearing découpe en phrases, transcrit chacune → hearing.sentence
            mind cherche déjà dans Hippocampe
2. appui    emory-ear → ear.<nom>.button (fin)
3. hearing  transcrit la dernière phrase → hearing.turn {text, speaker, device…}
4. mind     ajoute à la conversation, appelle le LLM en flux avec ses outils ;
            chaque phrase écrite → voice.say {device, text, last}
5. voice    synthétise → mouth.<nom>.audio
6. emory-mouth joue le son
```

Un appui pendant qu'Emory parle : `emory-mouth` coupe le son de lui-même, `mind` arrête le LLM et
envoie `voice.stop`. Les noms des sujets et le format de chaque message sont dans `contract/` ([`contract.md`](contract.md)),
la seule chose que les programmes partagent.

**Nos programmes sont en Rust** : `emory-ear`, `emory-mouth`, `emory-hands`, `hearing`, `mind`, `voice`. Pas de Python dans notre code.

**Les moteurs sont des adaptateurs, dans le langage qu'ils veulent.** Un moteur de modèle
(transcription, synthèse, LLM) se branche derrière l'organe qui s'en sert, de deux façons :

- **dans le binaire Rust**, quand c'est une bibliothèque C/C++ (whisper.cpp par `whisper-rs`,
  sherpa-onnx) : rien à déployer en plus ;
- **hors processus**, dans un conteneur à lui avec une petite API locale, quel que soit son
  langage (un moteur Python comme faster-whisper ou Pocket TTS, le serveur Rust de Kyutai, l'API
  d'un LLM dans le cloud). Un aller-retour local et un conteneur de plus, rien d'autre.

Le langage d'un moteur n'est donc pas un critère de choix ; à qualité égale, on préfère un moteur
dans le binaire, pour un conteneur de moins. Changer de moteur ne touche qu'un adaptateur.

**Tout tourne en local, sauf l'esprit**, pour lequel il n'y aura sans doute pas le choix (décidé
le 2026-10-11).

## Le dépôt

Un seul projet, un monorepo, un workspace Cargo : un dossier par programme, et un **contrat**
commun (les sujets du bus et le format des messages), dans un dossier à lui. Un organe se change
sans toucher aux autres.

```
emory/
  specs/        cette vue d'ensemble, puis une spec par organe
  contract/     les sujets du bus et le format des messages
  dev/          le bus et le moteur de voix pour développer (compose.yml, voice.toml)
  hearing/      le centre auditif (serveur)
  voice/        la voix (serveur)
  mind/         l'esprit (serveur) — à venir
  engines/      les moteurs hors processus, un dossier chacun (pocket/)
  ear/          emory-ear : le micro et son déclencheur, un module par déclencheur — à venir
  mouth/        emory-mouth : le haut-parleur — à venir
  hands/        emory-hands : les actions sur l'appareil, un module par standard — à venir
  eyes/         plus tard
```

## Hippocampe, à part

Hippocampe est la mémoire à long terme : il stocke, valide et retrouve, et ne fait tourner aucune
IA. Il sert tous les agents de Kris ; Emory n'est qu'un de ses clients.

- **La mémoire de travail est dans Emory, la mémoire à long terme dans Hippocampe.** Restent dans
  Emory : la mémoire tampon de `hearing`, les conversations en attente d'identification, les
  empreintes de voix (jamais dans Hippocampe).
- **Emory parle à Hippocampe par MCP, avec sa propre clé** (`emory`, `read,write`) : ce qu'il écrit
  est signé, et la clé se révoque d'une commande.
- **L'esprit lit avant d'agir** (une personne, un projet, ses derniers épisodes) **et écrit après
  coup** : ce que Kris dit (`said_by`, connu), ce qu'Emory suppose (`inferred`), ce qu'Emory a
  fait ou vu lui-même (`seen_by`), un épisode par moment qui compte ; un fait durable va aussi dans
  la fiche qu'il concerne.
- **Les outils MCP d'Hippocampe sont le contrat d'Emory** : la 1.0 d'Hippocampe promet de ne pas
  les casser sans version majeure.
- **Un besoin d'Emory devient un ticket d'Hippocampe**, écrit en termes généraux, jamais « pour
  Emory » : Hippocampe ne connaît pas Emory.
