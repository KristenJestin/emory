# Les mains d'Emory : version 1

Statut : brouillon du 2026-10-11, à valider par Kris. Noms passés en anglais le 2026-10-11.

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
modules = ["mpris"]
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

## Plus tard

- **Les appareils de la maison** : Kris n'en a aucun aujourd'hui. Pas de Home Assistant (décidé
  le 2026-10-11) : on ne veut pas installer un autre assistant pour avoir accès aux appareils.
  On s'inspire de ce qu'il a fait, sans en dépendre. Le jour venu, on choisit des appareils
  **Matter**, le standard commun de la domotique : un seul module Matter les pilote tous, quelle
  que soit la marque.
- D'autres modules standard selon les besoins : volume système, notifications.
- Les minuteurs et rappels d'Emory vivent dans `mind`, pas sur l'appareil.

## Questions ouvertes

1. Le format des adresses passées à `OpenUri` pour un morceau, un album ou une playlist
   Navidrome, à fixer avec le dépôt `poste`.
