# Emory

Statut : vue d'ensemble décidée avec Kris le 2026-10-10, le poste et l'architecture ajoutés le 2026-10-11.

Emory est l'assistant de Kris. Tout ce qui fait l'assistant est dans ce projet : les organes et
l'esprit. Sa mémoire à long terme n'y est pas : c'est Hippocampe, un projet à part.

## Ce qui est dans Emory

| Organe | Rôle | Spec |
|---|---|---|
| **Les oreilles** | transforment ce qui est dit en événements pour l'esprit | [`oreilles.md`](oreilles.md) (brouillon) |
| **La bouche** | fait parler Emory, et se tait quand Kris reprend la parole | [`bouche.md`](bouche.md) (brouillon) |
| **Les yeux** | ce qu'Emory voit | à écrire |
| **Les mains** | ce qu'Emory fait dans le monde (appareils, services) | [`mains.md`](mains.md) (brouillon) |
| **L'esprit** | reçoit les événements des organes, décide, lit et écrit dans Hippocampe, répond | [`esprit.md`](esprit.md) (brouillon) |

Les sujets du bus et le format des messages : [`contrat.md`](contrat.md).

## Sur chaque appareil : le poste et la main

**Toute l'intelligence vit sur le serveur ; un appareil ne fait que recevoir, envoyer et
exécuter** (décidé le 2026-10-11). Chaque appareil (le PC de Kris d'abord, plus tard un
Raspberry avec un micro et une enceinte) fait tourner deux petits programmes, branchés au bus
avec le nom de l'appareil, sans modèle, sans état, sans décision :

- **le poste**, pour le son : il capte le micro (en version 1 : entre deux appuis sur un bouton,
  un raccourci Hyprland sur le PC), envoie ce flux au serveur, et joue le flux qui revient ;
- **la main**, pour agir : elle exécute les commandes de l'esprit sur l'appareil (voir
  [`mains.md`](mains.md)).

Les deux sont **faits de modules, et chaque appareil n'active que les siens**, dans sa
configuration (`micro`, `haut-parleur` pour le poste ; `mpris` pour la main…). Un module non
activé ne tourne pas ; il peut ne pas être compilé (features Cargo). Au démarrage, chacun annonce
sur le bus ce qu'il sait faire, et l'esprit ne propose au LLM que ce qui existe. Ajouter une
capacité, c'est écrire un module une fois, puis l'activer là où il sert.

Les séparer garde le son, qui est en temps réel, à l'abri d'une action qui plante ou qui traîne.

Le serveur sait quel poste a entendu Kris, et c'est par lui qu'Emory répond. Les oreilles et la
bouche restent des organes à part, sur le serveur : le poste n'est que leur prise dans la pièce.
Un site web pour le téléphone pourra plus tard se brancher au bus ; il ne remplace pas le poste
sur le PC, parce qu'un onglet ne capte pas un raccourci quand il n'a pas le focus.

## Comment ça tourne

Décidé le 2026-10-11.

**Sur le serveur, des conteneurs Docker** déployés par Dokploy (depuis `~/Projects/home-infra`) :

| Conteneur | Rôle |
|---|---|
| `nats` | le bus de messages |
| `oreilles` | le centre auditif : reçoit l'audio des postes, transcrit, publie le tour de parole |
| `esprit` | l'esprit : garde la conversation, appelle le LLM et les outils, envoie les phrases à dire |
| `bouche` | la bouche : synthétise les phrases et renvoie l'audio au poste |
| un conteneur par moteur hors processus | voir « Les moteurs » |

**Sur chaque appareil, le poste et la main**, branchés au bus.

**Le bus : NATS.** Un courtier de messages, de la famille de RabbitMQ mais minimaliste : chaque
programme publie sur des sujets nommés (`oreilles.tour`, `bouche.dire`…) et s'abonne à ceux qui
l'intéressent ; NATS livre aussitôt, en moins d'une milliseconde sur le réseau local, et fait
aussi les demandes avec réponse. Les organes ne se connaissent pas : ils ne connaissent que le
bus et les sujets. Le poste s'y branche en TCP (client `async-nats`) ; un site web passera par le
WebSocket que NATS sait aussi servir. La persistance (JetStream) existe si un jour il en faut.

**Tout passe par NATS en version 1, le son compris** (décidé le 2026-10-11) : le poste publie le
micro en morceaux de 20 ms (`poste.<nom>.audio`), la bouche publie la voix d'Emory
(`poste.<nom>.audio.retour`). Un flux de voix fait environ 256 kbit/s, rien pour NATS. Le
transport du son est isolé dans un module du poste et des oreilles : s'il accroche à l'usage, il
passe sur une connexion dédiée (WebSocket, WebRTC) sans toucher au reste.

**Le bus est verrouillé dès le départ.** Le serveur NATS ne tourne que sur le serveur ; un
appareil n'a qu'une bibliothèque cliente, et c'est lui qui se connecte : rien n'est ouvert sur le
PC. Toutes les connexions sont en TLS ; chaque appareil et chaque organe a sa propre clé (nkey),
avec des droits par sujet : le poste du bureau ne publie que sur `poste.bureau.*`, la main du
bureau n'écoute que `main.bureau.*`, seul `esprit` publie sur `main.*`. Un appareil volé ou
compromis ne sort pas de ses sujets.

Un échange, avec l'oreille à bouton :

```
1. appui    poste → poste.<nom>.bouton (début), puis poste.<nom>.audio (le flux)
   pendant  oreilles découpe en phrases, transcrit chacune → oreilles.phrase
            esprit cherche déjà dans Hippocampe
2. appui    poste → poste.<nom>.bouton (fin)
3. oreilles transcrit la dernière phrase → oreilles.tour {texte, locuteur, poste…}
4. esprit   ajoute à la conversation, appelle le LLM en flux avec ses outils ;
            chaque phrase écrite → bouche.dire {poste, texte}
5. bouche   synthétise → poste.<nom>.audio.retour
6. poste    joue le son
```

Un appui pendant qu'Emory parle : le poste coupe le son de lui-même, `esprit` arrête le LLM et
envoie `bouche.stop`. Les noms des sujets et le format de chaque message sont dans `contrat/` ([`contrat.md`](contrat.md)),
la seule chose que les programmes partagent.

**Nos programmes sont en Rust** : `poste`, `main`, `oreilles`, `esprit`, `bouche`. Pas de Python dans notre code.

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
  contrat/      les sujets du bus et le format des messages
  poste/        le poste : le son de l'appareil, un module par micro et par haut-parleur
  oreilles/     le centre auditif
  bouche/       la bouche
  esprit/       l'esprit
  moteurs/      les moteurs hors processus, un dossier chacun
  main/         la main : les actions sur l'appareil, un module par capacité
  yeux/         plus tard
```

## Hippocampe, à part

Hippocampe est la mémoire à long terme : il stocke, valide et retrouve, et ne fait tourner aucune
IA. Il sert tous les agents de Kris ; Emory n'est qu'un de ses clients.

- **La mémoire de travail est dans Emory, la mémoire à long terme dans Hippocampe.** Restent dans
  Emory : la mémoire tampon des oreilles, les conversations en attente d'identification, les
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
