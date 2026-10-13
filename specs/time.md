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

1. **T1. Le module `time` sans parole** : `[data]` et `[time]`, le fichier (écriture atomique,
   relecture, fichier cassé), les quatre outils, les durées et échéances en mots, le fuseau.
   Vérifié : tests unitaires avec une horloge simulée et un dossier temporaire ; relecture après un
   « redémarrage » simulé ; un rappel `at` à travers un changement d'heure.
2. **T2. La sonnerie** (après E1 de [`events.md`](events.md)) : la tâche qui attend l'échéance la
   plus proche, la parole d'elle-même, la répétition et son arrêt par un appui, le retard au
   démarrage, `mind.timer.changed` et `mind.timer.fired`. Vérifié : temps en pause de tokio, faux
   bus enregistré : la phrase part à l'échéance, attend la fin d'une réponse, se répète puis
   s'arrête ; un essai en vrai d'un minuteur d'une minute par Kris.

## Questions ouvertes

1. L'appareil qui sonne : recommandation, celui qui l'a posé (Kris pose un minuteur là où il est),
   avec `default_device` en repli. Quand il y aura plusieurs appareils dans la maison, peut-être
   « là où Kris a parlé en dernier » : à revoir alors.
2. Une sonnerie que personne n'entend (appareil éteint) : recommandation, rien en version 1 ; quand
   `emory-mouth` existera, il pourra dire qu'il a joué un tour (un accusé), et `mind` redira la
   phrase au prochain tour de Kris.
3. La répétition : une fois, une minute après, si personne n'a appuyé. Recommandation : garder ce
   défaut et l'ajuster à l'usage (un minuteur de cuisine qu'on n'entend pas est pire qu'une phrase
   de trop).
4. Un minuteur perdu au-delà de 12 heures de panne : recommandation, le jeter (et le journaliser)
   plutôt que de dire de vieilles choses.
