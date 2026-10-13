# Le temps d'Emory : minuteurs et rappels

Statut : brouillon du 2026-10-13, à relire avec Kris. Décidé avec lui le même jour : `mind` garde
les minuteurs et les rappels, ils survivent à un redémarrage, et à l'échéance Emory le dit sur
l'appareil. Le reste est une proposition.

Un fichier à part de [`mind.md`](mind.md) : un module du serveur MCP d'Emory, un fichier de
données, deux sujets sur le bus, une unité de construction qui se lit seule. La parole à
l'échéance passe par la parole d'elle-même de [`events.md`](events.md).

## Ce qu'Emory sait faire

- « Mets un minuteur de dix minutes pour les pâtes. »
- « Rappelle-moi d'appeler le garage demain à neuf heures. »
- « Rappelle-moi dans une heure de sortir le linge. »
- « Il reste combien sur le minuteur ? »
- « Annule le minuteur des pâtes. » / « Quels rappels j'ai ? »

Un **minuteur** est une durée : à la fin, Emory dit qu'il est fini (« Le minuteur des pâtes est
fini. »). Un **rappel** est un texte à une heure : à l'heure, Emory le dit (« Rappel : appeler le
garage. »).

## La conception

### Ce que voit le LLM

| Outil | Arguments | Description |
|---|---|---|
| `time_timer` | `seconds` (1 à 86 400), `label` (facultatif, 40 caractères) | « Lance un minuteur. » |
| `time_reminder` | `text` (200 caractères), et `at` (heure locale `AAAA-MM-JJTHH:MM`) ou `in_seconds` | « Un rappel à une heure, ou dans un délai. Le texte, dit à la personne, à la deuxième personne. » |
| `time_list` | aucun | « Les minuteurs et rappels en cours, avec le temps qui reste. » |
| `time_cancel` | `id` | « Annule un minuteur ou un rappel de time_list. » |

- `in_seconds` évite au LLM un calcul de date (« dans une heure ») ; `at` se calcule depuis
  `[Maintenant]`, déjà dans chaque demande.
- `time_list` rend, pour chacun : `id` (un petit nombre, 1, 2, 3, jamais lu), `kind`, `label` ou
  `text`, `due` en heure locale, `remaining` en mots (« huit minutes », « demain à neuf heures »),
  et l'appareil.
- Un rappel dans le passé, ou à plus d'un an, est refusé avec la raison ; au-delà de `max_active`,
  aussi.
- Le résultat de `time_timer` et `time_reminder` dit l'échéance en mots, que le LLM répète
  (« C'est parti pour dix minutes. »).

### Où ils sont gardés : un fichier

**Un fichier JSON dans le dossier de données de `mind`** (`timers.json`), réécrit en entier à
chaque changement : écrit à côté puis renommé (après `fsync`), donc jamais à moitié écrit.

Pourquoi le plus simple suffit :

- quelques dizaines d'éléments au plus, aucune requête : une base (SQLite) n'apporterait qu'une
  dépendance ;
- lisible et réparable à la main ;
- **pas Hippocampe** : un rappel doit sonner même quand Hippocampe ne répond pas, et c'est de la
  mémoire de travail, qui reste dans Emory (voir le [README](README.md)) ;
- **pas JetStream** : une dépendance au serveur NATS pour rien.

```json
{ "version": 1, "next_id": 4,
  "items": [ { "id": 3, "kind": "timer", "label": "pâtes", "seconds": 600,
               "created": "2026-10-13T19:02:11+02:00", "due": "2026-10-13T19:12:11+02:00",
               "device": "office" } ] }
```

L'échéance est une date absolue, avec son décalage : un redémarrage ne fausse rien. Un rappel
donné en heure locale est converti à sa création, dans le fuseau de `[time] timezone`.

**Au démarrage**, `mind` relit le fichier. Un fichier illisible est renommé
(`timers.json.broken-<date>`), `mind` démarre sans minuteur et le journal le dit en `error` :
jamais un refus de démarrer pour ça.

**Ce qui a sonné pendant que `mind` était arrêté** est dit au démarrage, précédé de « Avec du
retard : » quand le retard dépasse une minute, jusqu'à `late_limit_hours`. Plus vieux, c'est jeté
et journalisé (un minuteur de pâtes de la veille ne sert plus à rien).

### À l'échéance

- **L'appareil** : celui de la conversation qui l'a posé (`mind` connaît l'appareil de chaque
  conversation). Sans lui (un minuteur relu après un redémarrage garde son appareil, donc ce cas
  ne vient que d'un fichier écrit à la main), `[time] default_device`, sinon l'appareil du dernier
  tour entendu.
- **Ce qui est dit** : une phrase fixe du code, sans LLM, donc immédiate, gratuite, et qui marche
  quand le LLM ne répond pas. Les gabarits sont dans la configuration : `timer_done` (« Le
  minuteur {label} est fini. », ou `timer_done_unlabeled`, « Le minuteur de {duration} est
  fini. ») et `reminder` (« Rappel : {text}. »). `{duration}` est en mots.
- **Comment** : la parole d'elle-même ([`events.md`](events.md)) : un `Cue` `tone` si `chime`,
  puis la phrase, quand l'appareil est libre (après la réponse en cours, jamais pendant que Kris
  parle). La phrase entre dans la conversation, pour que « relance-le cinq minutes » marche.
- **La répétition** : si aucun appui n'arrive sur l'appareil dans les `repeat_after_s` qui suivent,
  la phrase est redite, `repeat` fois au plus. Un appui (même pour dire autre chose) arrête les
  répétitions.
- **Pas d'accusé de réception** de l'appareil : `mind` ne sait pas si `emory-mouth` a joué le son
  (un appareil éteint n'entend rien). Voir les questions ouvertes.

### Sur le bus

`mind` publie l'état, pour qu'un écran ou une barre (l'île du bureau de Kris, plus tard) puisse
afficher les minuteurs sans rien demander à `mind` :

- `mind.timer.changed` : la liste entière, à chaque création, annulation, échéance ;
- `mind.timer.fired` : un minuteur ou un rappel qui sonne.

Personne ne les écoute aujourd'hui ; ils coûtent deux lignes et évitent de rouvrir le contrat plus
tard. Le format est dans [`contract.md`](contract.md). Seul `mind` publie sur `mind.timer.*`.

## La configuration

```toml
[data]
dir = "~/.local/state/emory/mind"   # le dossier de données de mind ; dans le conteneur, un volume

[time]
enabled = true
# store = "<data.dir>/timers.json"   # par défaut dans le dossier de données
timezone = ""                  # vide : celui du système (TZ) ; sinon "Europe/Paris"
# default_device = "office"    # par défaut l'appareil du dernier tour entendu
max_active = 50
chime = true
repeat = 1                     # 0 à 5
repeat_after_s = 60            # 10 à 600
late_limit_hours = 12
timer_done = "Le minuteur {label} est fini."
timer_done_unlabeled = "Le minuteur de {duration} est fini."
reminder = "Rappel : {text}."
```

`[data]` est une section nouvelle, commune : tout ce que `mind` devra garder entre deux démarrages
ira dans ce dossier. Vérifié au démarrage : un fuseau inconnu, un gabarit sans son `{…}`, des
bornes ; le dossier est créé s'il manque.

## La sécurité

- Les minuteurs ne passent ni par l'entrée interne ni par le bus pour sonner : un appel direct dans
  `mind`. Personne d'autre que le LLM, par ses outils, ne pose ou n'annule un minuteur.
- Le fichier est dans le dossier de `mind`, lisible par lui seul (droits `0600`) : un rappel peut
  être personnel. Les journaux disent l'`id`, le type, l'échéance et l'appareil, jamais le texte
  ni le libellé.

## Hors de cette version

- Les rappels récurrents (« tous les lundis ») : plus tard, peut-être par les échéances
  d'Hippocampe plutôt que par `mind`.
- Le report (« encore cinq minutes ») : le LLM pose un nouveau minuteur, c'est assez.
- Un réveil en musique : quand les mains existeront, une action à l'échéance.

## L'ordre de construction

1. **T1. Le module `time` sans parole** (**construit le 2026-10-13**, voir « Ce qui a été décidé en construisant T1 ») : `[data]` et `[time]`, le fichier (écriture atomique,
   relecture, fichier cassé), les quatre outils, les durées et échéances en mots, le fuseau.
   Vérifié : tests unitaires avec une horloge simulée et un dossier temporaire ; relecture après un
   « redémarrage » simulé ; un rappel `at` à travers un changement d'heure.
2. **T2. La sonnerie** (**construit le 2026-10-13**, voir « Ce qui a été décidé en construisant T2 ») : la tâche qui attend l'échéance la
   plus proche, la parole d'elle-même, la répétition et son arrêt par un appui, le retard au
   démarrage, `mind.timer.changed` et `mind.timer.fired`. Vérifié : temps en pause de tokio, faux
   bus enregistré : la phrase part à l'échéance, attend la fin d'une réponse, se répète puis
   s'arrête ; un essai en vrai d'un minuteur d'une minute par Kris.

## Ce qui a été décidé en construisant T1 (2026-10-13)

Le code est dans `mind/src/time/` (`mod.rs` les outils, `store.rs` le fichier, `words.rs` les mots,
`zone.rs` le fuseau, `config.rs` `[data]` et `[time]`). Les choix que la spec laissait ouverts,
pris au plus simple et réversible :

- **Fuseau** : une seule dépendance nouvelle, `jiff` (déjà dans `Cargo.lock`), pour nommer un
  fuseau (`Europe/Paris`) et connaître son décalage à un instant donné ; `chrono` seul ne sait pas.
  `timezone` vide : le fuseau du système (`TZ`, sinon `/etc/localtime`). Un fuseau inconnu est
  refusé au démarrage, avec le nom de la clé. (`tempfile` en plus, en dépendance de test
  seulement : déjà compilé pour `ear`.)
- **Changement d'heure** : un rappel `at` est une heure du mur dans le fuseau. Une heure qui
  n'existe pas (le saut de 02:00 à 03:00) est décalée d'autant (02:30 devient 03:30) et la réponse
  le dit au LLM ; une heure qui existe deux fois (le retour de 03:00 à 02:00) est la première. Un
  minuteur dure ce qui a été demandé, changement d'heure ou non (c'est un écart en secondes). Le
  fichier garde le décalage de l'échéance elle-même (`+01:00` ou `+02:00`), pas celui du jour où
  le rappel a été posé.
- **`[data] dir`** : par défaut `$XDG_STATE_HOME/emory/mind`, sinon `~/.local/state/emory/mind`
  (l'exemple de la spec). `[time] store` peut désigner un autre fichier. `dev/mind.toml` prend
  `~/.local/state/emory/mind-dev` : hors du dépôt, et distinct du dossier d'un `mind` de production
  qui tournerait sur le même PC.
- **Les clés de T2** (`default_device`, `chime`, `repeat`, `repeat_after_s`, `timer_done`,
  `timer_done_unlabeled`, `reminder`) sont déjà lues et vérifiées (bornes, gabarits : le `{…}`
  voulu, et lui seul) ; rien ne les utilise avant T2. `late_limit_hours` est borné de 1 à 168.
- **Les outils** : `seconds` va de 1 à 86 400 ; `in_seconds` de 1 à 31 536 000 (un an) ; `at`
  accepte `AAAA-MM-JJTHH:MM`, avec des secondes, ou un instant avec son décalage (`…+02:00`, `…Z`).
  Un `label` de plus de 40 caractères ou un `text` de plus de 200 est **refusé** avec la raison,
  pas coupé (couper un rappel en change le sens). Ni `at` ni `in_seconds`, ou les deux : refusé.
  Un nombre sans fraction écrit en décimal (`600.0`) est accepté. Les erreurs sont des phrases
  françaises, que le LLM lit.
- **Ce que disent les résultats** : « Minuteur 1 (pâtes) : dix minutes, échéance à dix-neuf
  heures dix. », « Rappel 2 : échéance demain à neuf heures. ». Ils ne promettent pas de
  sonnerie : avant T2, rien ne sonne, et les descriptions des outils n'en parlent pas.
- **`time_list`** rend un objet JSON `{"items": [...]}`, du plus proche au plus lointain, avec
  `id`, `kind`, `label` ou `text`, `due` (heure locale `AAAA-MM-JJTHH:MM:SS`), `remaining` et
  `device` quand il est connu ; sans rien : « Aucun minuteur ni rappel en cours. ». Ce qui est
  échu n'y figure plus (avant T2 rien ne le fait sonner : T2 le prend au fichier). `remaining` est
  une durée en mots jusqu'à six heures (« huit minutes et douze secondes » ; arrondie à la minute
  à partir de dix minutes, à l'heure à partir d'un jour), puis le moment (« demain à neuf
  heures », « lundi à midi », « le vingt octobre à dix heures »). Les heures sont dites sur 24 h
  (« dix-neuf heures douze ») : une voix lit mal « 19:12 ».
- **`max_active`** compte ce qui n'est pas encore échu.
- **Le fichier** : `timers.json` comme dans la spec ; `next_id` ne redescend jamais (un numéro
  n'est pas réutilisé, même après un redémarrage). Écrit à côté (`timers.json.tmp`, droits `0600`),
  `fsync`, renommé, puis `fsync` du dossier. Une écriture qui échoue ne change rien en mémoire et
  le LLM l'apprend. Un fichier est « cassé », et renommé `timers.json.broken-AAAAMMJJTHHMMSSZ`
  (un `-2` si le nom existe), non seulement s'il n'est pas du JSON, mais s'il est d'une autre
  version, si deux éléments ont le même numéro, un numéro au-delà de `next_id`, ou un rappel sans
  texte : le contenu reste intact pour le réparer à la main.
- **Le retard au démarrage** : T1 jette déjà, en le journalisant (`warn`, sans le texte), ce qui
  est échu depuis plus de `late_limit_hours` (`Store::discard_late`). Ce qui l'est depuis moins
  reste dans le fichier : c'est ce que T2 dira « Avec du retard : », avec `created`, `due`,
  `seconds` et `device` pour décider.
- **L'appareil** : `Conversations::open_on(device)` et `Conversations::device(clé)` sont là, et le
  module range l'appareil de la conversation dans l'élément (sinon `default_device`, sinon rien).
  **Mais `mind.rs` appelle encore `conversations.open()`** (ligne 421, dans `Mind::open`) : T1 n'y
  a pas touché, pour rester hors de la boucle de tours que construit E1. Tant que cette ligne n'est
  pas devenue `conversations.open_on(device.clone())`, les éléments n'ont que `default_device`.
  À faire dans T2.
- **Les phrases d'attente** : `time_timer`, `time_reminder`, `time_list` et `time_cancel` sont
  `silent` dans `mind/prompts/waiting.fr.toml` et `dev/waiting.fr.toml` : ils répondent aussitôt,
  une phrase d'attente ne ferait que gêner la réponse.
- **Les journaux** disent l'`id`, le type, l'échéance et l'appareil, jamais le texte ni le libellé
  (un test le vérifie).

## Ce qui a été décidé en construisant T2 (2026-10-13)

Le code est dans `mind/src/time/ring.rs` (la tâche qui sonne), avec un crochet dans
`mind/src/speech.rs` et `mind/src/mind.rs`. Les choix, pris au plus simple et réversible :

- **La tâche** : `Time::ringer(...)` la donne, `mind` la lance à côté du module (`lib.rs`). Elle
  regarde l'échéance la plus proche, dort jusqu'à elle, mais **jamais plus de 30 s** (`RECHECK`) :
  l'horloge du mur est relue, pour qu'une mise en veille du PC ou une heure remise à l'heure ne
  laisse pas une échéance attendre sur un sommeil faux. Un outil qui pose ou annule la réveille
  aussitôt.
- **La phrase** : les gabarits de `[time]`, sans LLM. Le texte d'un rappel perd son point final
  (« Rappel : appeler le garage. », jamais « .. »). En retard de plus d'une minute, quel que soit
  le moment (au démarrage, ou après une mise en veille), elle commence par « Avec du retard : »
  et sa première lettre passe en minuscule (« Avec du retard : le minuteur pâtes est fini. »).
  Au-delà de `late_limit_hours`, jetée et journalisée, au démarrage (T1) comme en marche.
- **L'appareil** : celui de l'élément ; sinon `default_device` ; sinon l'appareil du dernier tour
  entendu (`Conversations::last_device`, noté à chaque tour). Sans aucun, l'élément est jeté avec
  un `warn`. **`mind.rs` ouvre désormais la conversation avec son appareil** (`open_on`) : un
  minuteur posé en parlant au bureau sonne au bureau.
- **Le crochet « un appui après la sonnerie »** : une parole d'elle-même peut être suivie
  (`Speech::followed()`, qui rend un canal). `mind` y dit `Said` quand il la dit (le ton, puis la
  phrase), puis `Pressed` au premier appui sur cet appareil (n'importe quel appui, même d'un tour
  en retard), et ferme le canal ; ou `Dropped` si elle n'a jamais pu être dite (appareil jamais
  libre avant son expiration, 600 s). Un appui **avant** que la phrase soit dite (Kris parlait)
  ne compte pas : la fenêtre de la répétition part du moment où elle est dite, pas de l'échéance.
  Raison : une phrase qui attend la fin d'une longue réponse ne doit pas être redite aussitôt
  après.
- **La répétition** : `repeat_after_s` après qu'elle a été dite, sans appui sur l'appareil, elle
  est redite (avec le ton si `chime`), `repeat` fois au plus. Un appui sur un autre appareil ne
  l'arrête pas.
- **Le fichier** : l'élément en sort quand la phrase est dite la première fois (ou jetée). Si
  `mind` s'arrête entre l'échéance et ce moment (appareil occupé), il est dit en retard au
  démarrage suivant. `time_list` ne montrait déjà plus ce qui est échu.
- **Une annulation pendant que la phrase attend l'appareil** ne la retire pas de la file de
  `mind` : elle est dite quand même. Cas rare (il faut annuler pendant une réponse, à la seconde
  de l'échéance), laissé ainsi.
- **Sur le bus** (`contract`) : `subjects::TIMER_CHANGED` et `TIMER_FIRED`, les types `Timers`,
  `TimerItem`, `TimerKind`, `TimerFired` (le format de [`contract.md`](contract.md), `late_s` en
  secondes). `mind.timer.changed` part au démarrage de la tâche, à chaque pose, annulation, et
  quand un élément sort du fichier (dit ou jeté) ; la liste entière, du plus proche au plus
  lointain, échus compris tant qu'ils n'ont pas été dits. `mind.timer.fired` part à l'échéance,
  une fois (pas à chaque répétition). Ils portent le libellé et le texte, comme le contrat le
  prévoit ; les journaux, jamais.
- **Les journaux** : `timer or reminder rings` (id, type, appareil, `late_s`), `nobody pressed:
  said again`, `timer or reminder heard: a press`, `... not said: the device was never free`,
  `... too late to be said: dropped`, `... with no device to ring on: dropped`.
- **Vérifié** (`mind/tests/mind.rs`, module `ring`, temps de tokio en pause, l'horloge du module
  la suit, faux bus enregistré) : la phrase part à l'échéance, avec son ton, sur l'appareil de la
  conversation ; elle attend la fin d'une réponse ; elle est redite une fois après une minute puis
  plus ; les répétitions suivent la configuration ; un appui sur l'appareil arrête la répétition,
  un appui ailleurs non ; un minuteur échu pendant un arrêt de trois heures est dit « Avec du
  retard » au démarrage, une minute et demie ne l'est pas, treize heures est jeté sans rien dire ;
  sans appareil, `default_device` puis le dernier tour entendu ; un rappel et un minuteur sans
  libellé ; un minuteur annulé ne sonne pas ; les messages du bus. Plus le crochet seul (dite,
  appui, jetée).
- **Pas vérifié** : rien n'a été joué ni lancé sur le vrai bus. L'essai d'un minuteur d'une minute
  par Kris reste à faire.

## Questions ouvertes

1. L'appareil qui sonne : recommandation, celui qui l'a posé (Kris pose un minuteur là où il est),
   avec `default_device` en repli. Quand il y aura plusieurs appareils dans la maison, peut-être
   « là où Kris a parlé en dernier » : à revoir alors.
   **Décidé le 2026-10-13 avec Kris : celui qui l'a posé, `default_device` en repli.**
2. Une sonnerie que personne n'entend (appareil éteint) : recommandation, rien en version 1 ; quand
   `emory-mouth` existera, il pourra dire qu'il a joué un tour (un accusé), et `mind` redira la
   phrase au prochain tour de Kris.
3. La répétition : une fois, une minute après, si personne n'a appuyé. Recommandation : garder ce
   défaut et l'ajuster à l'usage (un minuteur de cuisine qu'on n'entend pas est pire qu'une phrase
   de trop).
   **Décidé le 2026-10-13 avec Kris : oui, une répétition après une minute.**
4. Un minuteur perdu au-delà de 12 heures de panne : recommandation, le jeter (et le journaliser)
   plutôt que de dire de vieilles choses.
   **Décidé le 2026-10-13 avec Kris : oui, jeté et journalisé.**
5. Le gabarit sans libellé dit « Le minuteur de une minute est fini. » : « de une » se dit mal
   (« d'une »). Recommandation : écrire « d'une » quand la durée commence par « une », dans le
   code, ou changer le gabarit par défaut en « Le minuteur est fini : {duration}. ». À juger à
   l'oreille.
