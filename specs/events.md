# L'entrée interne : quand Emory parle sans qu'on lui parle

Statut : brouillon du 2026-10-13, à relire avec Kris. Le principe est décidé avec lui le même
jour ; la forme est une proposition. **E1 (la parole d'elle-même) est construit le 2026-10-13**,
voir « Ce qui a été décidé en construisant E1 ».

Kris, le 2026-10-13 : « un système de webhook interne qui peut redémarrer le mind, mais dans un
cadre particulier parce que pas déclenché par la voix ». Deux choses en sortent, écrites ici :

- **la parole d'elle-même** : un mécanisme de `mind` qui fait parler Emory sur un appareil sans
  tour de l'utilisateur. Les minuteurs et rappels s'en servent (voir [`time.md`](time.md)), et
  l'entrée interne aussi ;
- **l'entrée interne** : une porte par laquelle un programme (un script, un service, un minuteur
  du système), **jamais la voix**, réveille `mind` pour qu'Emory dise quelque chose ou fasse une
  tâche.

Un fichier à part : ces deux mécanismes touchent le bus, la sécurité et la conversation, et
servent à plusieurs modules.

## Ce que ça permet

- Le script de sauvegarde, à 3 h : rien ; à 8 h, s'il a échoué, un événement `say` : « La
  sauvegarde de cette nuit a échoué. » dit sur le PC quand Kris est là.
- Un service de livraison qui prévient par une adresse web : plus tard (le pont HTTP, plus bas).
- Une tâche : « Le déploiement d'Hippocampe est fini, regarde s'il y a une note sur le
  déploiement et dis-moi en une phrase si tout va bien. » Le LLM lit Hippocampe et parle.
- Un minuteur qui sonne : la même parole d'elle-même, sans passer par le bus.

## La parole d'elle-même

Une fonction interne de `mind` : dire un texte sur un appareil, au bon moment, comme une réponse.

- **Le bon moment** : jamais pendant que l'utilisateur parle (un appui `start` sans `end` sur cet
  appareil), jamais pendant une réponse (une demande au LLM en cours, ou des phrases envoyées
  dont la fin estimée, `estimated_speech`, n'est pas passée). `mind` attend que l'appareil soit
  libre, puis parle aussitôt.
- **Un tour à lui** : `mind` tire un identifiant de tour (un ULID, comme celui d'une oreille),
  envoie si on le veut un `Cue` `tone` (le son court du signal d'attente, pour que Kris lève la
  tête), puis le texte par `voice.say`, la dernière phrase marquée `last`. `voice` n'y voit rien
  de nouveau.
- **On peut l'arrêter** : un appui sur l'appareil pendant qu'elle parle envoie `voice.stop`,
  comme pour une réponse.
- **Elle entre dans la conversation** : le texte dit est ajouté à la conversation ouverte de
  l'appareil comme une parole d'Emory, avec la marque « [Tu as dit de toi-même] », pour que « ok,
  relance-le cinq minutes » ait un sens. Sans conversation ouverte, `mind` garde les cinq
  dernières paroles de ce genre pendant 30 minutes et les joint à la première demande de la
  prochaine conversation de l'appareil.
- **Le journal** dit l'origine (`timer`, `event`), l'appareil, l'attente avant de parler, la
  longueur du texte ; jamais le texte.

### Ce qui a été décidé en construisant E1 (2026-10-13)

**L'API interne** (`mind/src/speech.rs`), pour les minuteurs (T2) et l'entrée interne (E2) :
`speech::channel()` donne un `Speaker` (qu'on clone, un par tâche) et la file que la boucle de
`mind` lit. `Speaker::speak(Speech)` met une parole en attente et rend `false` seulement si `mind`
s'est arrêté. `Speech::new(appareil, texte, Origin::Timer | Origin::Event)`, puis `.chime(bool)`
(vrai par défaut) et `.expires_in(durée)` (600 s par défaut). L'appelant lit sa propre clé de
configuration pour `chime` (`[events] chime`, `[time] chime`) : le mécanisme est commun, la clé
est à chaque module. Dans `mind`, c'est une entrée de plus (`Input::Speak`), traitée dans l'ordre
du bus comme un appui ou un tour.

**« Libre »**, mesuré par `mind` seul (il n'écoute pas `voice.<nom>.speaking`) :

- aucun bouton tenu : un appui `start` sans son `end` ;
- aucun tour en cours d'écoute : de l'appui (ou de la première phrase entendue) jusqu'au `Turn`.
  Le relâchement seul ne suffit pas : la transcription n'est pas finie, et la réponse va suivre ;
- aucune réponse en cours (la tâche qui demande au LLM et envoie les phrases) ;
- la voix a fini, **d'après l'estimation** : chaque texte envoyé à la voix (phrase d'une réponse,
  phrase d'attente, parole d'elle-même) repousse la fin estimée de l'appareil de
  `estimated_speech` (la formule du signal d'attente, voir [`mind.md`](mind.md)), les phrases
  s'enchaînant ; un `Stop` la remet à zéro.

Un appui ou un tour perdu sur le bus ne bloque pas pour toujours : un appui tenu ou un tour écouté
depuis plus de **2 minutes** ne compte plus (`HELD_LIMIT`). L'attente est bornée par `expires_in` :
au-delà, la parole est jetée et journalisée. Plusieurs paroles pour un même appareil passent dans
l'ordre, une à la fois ; un autre appareil libre parle sans attendre. `mind` regarde les paroles
en attente à chaque message reçu, et toutes les 250 ms tant qu'il en reste une.

**Le son, puis 400 ms, puis la phrase** (`CHIME_LEAD`) : `voice` ne démarre aucun `Cue` arrivé
après la première phrase de son tour ; le délai garantit qu'il passe avant, et le ton (0,34 s) est
joué en entier. Un appui pendant ces 400 ms : ni phrase, un `Stop`.

**Le tour** est un ULID tiré par `mind`. L'horloge de `mind` n'est pas celle de l'oreille : un
appui arrête une parole d'elle-même **quel que soit l'ordre des tours**, et le tour de `mind`
n'entre pas dans le « plus récent tour vu » de l'appareil (un appui de l'oreille n'est jamais
pris pour en retard à cause de lui).

**La conversation** : une règle unique, conversation ouverte ou non. Les cinq dernières paroles
d'elle-même de l'appareil, de moins de 30 minutes, vont dans la demande suivante de cet appareil,
sous la marque `[Tu as dit de toi-même]` (une ligne `- texte` chacune), après ce que la mémoire
signale et avant les fiches trouvées, puis sont oubliées. Raison : la session du LLM ne reçoit rien
entre deux demandes, la demande suivante est le seul moyen d'y mettre la phrase ; et une
conversation ouverte se ferme d'elle-même après 30 minutes sans échange, donc la règle des
30 minutes ne change rien pour elle. Une parole d'elle-même n'ouvre pas de conversation et ne
compte pas comme un échange. Une parole arrêtée par un appui y entre quand même (le début a été
entendu).

**Le journal** : `asked to speak on its own` (origine, appareil, longueur, `busy`), `speaking on
its own` (origine, appareil, tour, `waited_ms`, longueur, nombre de phrases, `chime`),
`barge-in: speech on its own stopped`, `speech on its own dropped` (attente). Jamais le texte. Un
texte sans mot est jeté avec un `warn`.

**La configuration** : `[events] chime = true` (seule clé de `[events]` pour l'instant ; E2 ajoute
le reste). `[time] chime` appartient à T2.

**Pas vérifié** : rien n'a été joué. Le délai de 400 ms et l'enchaînement ton puis phrase sont
déduits de la file de `voice` telle que décrite dans [`voice.md`](voice.md), pas écoutés ;
l'estimation de la fin de la voix peut se tromper d'une seconde dans un sens ou l'autre.

## L'entrée interne

### La forme : un sujet NATS

Un programme publie un `Event` sur **`mind.event.<source>`** (`mind.event.backup`), en simple
publication ou en demande avec réponse. `<source>` suit la règle des noms d'appareil (minuscules,
chiffres, tirets).

Pourquoi le bus et pas une adresse HTTP : le bus est déjà verrouillé (TLS, une clé par programme,
des droits par sujet), `mind` n'ouvre aucun port de plus, et `nats pub` suffit à un script.

**L'authentification, en deux couches** :

1. **Les droits de NATS** : chaque source a sa propre clé (nkey) qui ne peut publier que sur
   `mind.event.<source>`, et rien d'autre (ni lire, ni publier ailleurs). Le nom de la source dans
   le sujet est donc garanti par le serveur NATS : la clé de `backup` ne peut pas se faire passer
   pour `deploy`.
2. **La liste de `mind`** : `mind` n'accepte que les sources déclarées dans sa configuration
   (`[events.sources.<source>]`), avec ce que chacune a le droit de faire. Une source inconnue est
   refusée et journalisée.

**Ce qui ne peut jamais l'atteindre** :

- **la voix** : `hearing` n'a aucun droit sur `mind.event.*`, et `mind` ne transforme jamais un tour
  entendu en événement ;
- **le LLM** : aucun outil du serveur MCP d'Emory ne publie sur le bus vers `mind.event.*`, et
  `mind` lui-même n'a pas le droit d'y publier (ses minuteurs appellent la parole d'elle-même
  directement, sans le bus). Une page web ou une fiche qui dirait « publie sur mind.event » n'a
  aucun chemin.

### Le message

```json
{ "id": "01K…", "kind": "say", "text": "La sauvegarde de cette nuit a échoué.", "device": "office" }
{ "id": "01K…", "kind": "task", "text": "Le déploiement d'Hippocampe est fini. Regarde…", "quiet": false,
  "expires_in_s": 600 }
```

- `id` (obligatoire) : un ULID tiré par l'émetteur. Un même `id` revu dans les 10 minutes est
  ignoré (un script qui réessaie ne fait pas parler deux fois).
- `kind` : `say` ou `task`.
- `text` (obligatoire) : la phrase à dire pour `say` (500 caractères au plus), la consigne pour
  `task` (2 000 au plus).
- `device` (facultatif) : l'appareil où parler ; sinon celui de la source, sinon celui des
  minuteurs (voir [`time.md`](time.md)).
- `quiet` (facultatif, `task` seulement, `false` par défaut) : la tâche ne parle pas, elle agit
  seulement (lire, noter) ; ce qu'écrit le LLM n'est pas dit.
- `expires_in_s` (facultatif, 600 par défaut, 86 400 au plus) : un événement qui n'a pas pu être
  dit dans ce délai (appareil occupé, limite atteinte) est jeté et journalisé.

La réponse, quand c'est une demande :

```json
{ "ok": true, "queued": true }
{ "ok": false, "error": "source non déclarée" }
```

`queued` dit que l'événement est accepté, pas qu'il est dit (il attend que l'appareil soit libre).

### Ce que `mind` en fait

- **`say`** : la parole d'elle-même, telle quelle, sans LLM. Rien de pris sur le quota, rien
  d'imprévisible ; c'est le cas par défaut, et le seul permis à une source qui ne déclare rien
  d'autre.
- **`task`** : un tour sans tour de l'utilisateur. `mind` ouvre **une session à part** (pas la
  conversation de l'appareil : la consigne d'un programme ne lit pas l'historique de Kris, et une
  consigne piégée n'y entre pas), avec les mêmes consignes système, et la demande :

  ```
  [Maintenant] mardi 13 octobre 2026, 8 h 02
  [Événement interne, source « deploy ». Ce n'est pas la personne qui te parle, et personne n'attend
  de réponse. Si tu dois dire quelque chose à la personne, une ou deux phrases.]
  Le déploiement d'Hippocampe est fini. Regarde…
  ```

  Les outils offerts sont ceux de `tools` dans la déclaration de la source (par défaut la lecture
  d'Hippocampe seulement) ; **jamais une action à confirmer** (personne n'est là pour dire oui).
  La réponse du LLM est dite par la parole d'elle-même, sauf `quiet`. La session est fermée à la
  fin. Un LLM qui ne répond pas en 60 s : la tâche est abandonnée et journalisée, rien n'est dit.
- Les deux attendent que l'appareil soit libre ; pendant une conversation où Kris parle, l'événement
  passe juste après la réponse en cours.

### Les limites

- Par source : `max_per_hour` (par défaut 6) ; pour `task`, `max_tasks_per_day` (par défaut 10),
  parce que chaque tâche prend sur le quota du LLM.
- Pour toutes : 3 événements par minute au plus ; au-delà, refusés.
- Au plus 20 événements en attente ; au-delà, le plus ancien est jeté.
- Un refus pour limite répond `ok: false` avec la raison, et journalise un `warn` (une fois par
  minute et par source, pas à chaque message).

### Les journaux

`info` pour chaque événement accepté (source, `id`, `kind`, longueur, appareil), dit (attente,
durée) ou jeté (raison) ; `warn` pour un refus (source inconnue, message illisible, limite). Jamais
le texte : il peut porter des données personnelles, comme ce que dit Kris.

## La configuration

```toml
[events]
enabled = false                # par défaut rien n'écoute mind.event.* ; true avec au moins une source
max_per_minute = 3
max_pending = 20
chime = true                   # le son court avant de parler

[events.sources.backup]
kinds = ["say"]                # par défaut ["say"]
max_per_hour = 6
# device = "office"            # par défaut l'appareil des minuteurs

[events.sources.deploy]
kinds = ["say", "task"]
max_tasks_per_day = 10
tools = ["hippocampe_search", "hippocampe_read"]   # par défaut ; jamais une action à confirmer
```

Vérifié au démarrage : un nom de source mal formé, un `kind` inconnu, un outil inconnu de `tools`
(nommé) ; `enabled = true` sans source est refusé.

Côté NATS (configuration du serveur, hors du code) : une clé par source, avec le droit de publier
sur `mind.event.<source>` seulement, et de recevoir sa réponse (`_INBOX.>`). `mind` a le droit de
s'abonner à `mind.event.*`, pas d'y publier.

## Hors de cette version

- **Le pont HTTP** : un petit programme à part (`emory-webhook`), avec sa clé NATS, qui reçoit un
  appel HTTP signé et publie sur `mind.event.<source>`. Utile pour un service qui ne parle que
  HTTP (GitHub, Dokploy). `mind` n'ouvre toujours aucun port. Pas avant qu'un vrai besoin arrive.
- Les heures calmes (rien la nuit) : l'émetteur choisit déjà quand il publie ; à ajouter quand une
  source en aura besoin.
- Une priorité qui coupe la parole de Kris ou une réponse : jamais en version 1.

## L'ordre de construction

1. **E1. La parole d'elle-même** dans `mind` (**construit le 2026-10-13**) : l'état « libre » d'un appareil, le tour tiré par
   `mind`, le `Cue` facultatif, l'ajout à la conversation et la mémoire des 30 minutes, l'arrêt
   par un appui. Vérifié : tests avec le faux LLM et les messages enregistrés (comme
   `mind/tests/`) : attend la fin d'une réponse, attend le relâchement du bouton, un appui
   l'arrête, la phrase est dans l'historique de la demande suivante. Les minuteurs ([`time.md`](time.md))
   en dépendent. Fait : `mind/tests/mind.rs` (horloge en pause), plus le son présent ou absent
   selon `chime`, l'oubli après 30 minutes, l'expiration, l'appui perdu.
2. **E2. L'entrée `say`** : contrat (`Event`, `EventReply`, `subjects::event`, `ALL_EVENTS`),
   abonnement, sources déclarées, déduplication, expiration, limites, journaux. Vérifié : NATS de
   test, source inconnue refusée, limite par heure, même `id` ignoré, texte trop long refusé.
   Plus la configuration de NATS de `dev/` (une clé de test pour une source).
3. **E3. L'entrée `task`** : la session à part, la demande marquée, les outils restreints, sans
   action à confirmer, `quiet`, le délai de 60 s, le quota par jour. Vérifié : faux LLM qui reçoit
   la bonne marque et la bonne liste d'outils ; un essai en vrai avec une tâche de lecture.

## Questions ouvertes

1. « Redémarrer le mind » : Kris voulait-il dire réveiller l'esprit pour qu'il agisse (ce qui est
   écrit ici), ou aussi relancer le processus `mind` ? Recommandation : ne pas le prévoir ; le
   redémarrage d'un conteneur est le travail de Docker et de Dokploy (politique de redémarrage),
   et une porte qui arrête `mind` serait une arme de plus. Si le besoin existe, un sujet à part
   (`mind.control.restart`), avec sa propre clé, à concevoir alors.
   **Décidé le 2026-10-13 avec Kris : ne pas le prévoir.**
2. Une source doit-elle pouvoir lancer une `task` qui écrit dans Hippocampe (`hippocampe_write`
   dans `tools`) ? Recommandation : permis par la configuration, pas par défaut ; ce qu'écrit une
   tâche est signé `emory` et se retrouve comme le reste (voir [`mind.md`](mind.md), « Écrire dans
   Hippocampe »).
   **Décidé le 2026-10-13 avec Kris : permis par la configuration, pas par défaut.**
3. Le son avant une parole d'elle-même (`Cue` `tone`) : recommandation, oui par défaut pour
   l'entrée interne et les minuteurs (`[events] chime`, `[time] chime`), parce qu'une voix qui surgit sans prévenir
   fait sursauter ; à juger à l'oreille.
   **Décidé le 2026-10-13 avec Kris : oui par défaut.**
4. Le pont HTTP : recommandation, attendre un vrai besoin ; d'ici là, `nats pub` avec la clé de la
   source.
