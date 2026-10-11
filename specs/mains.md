# Les mains d'Emory : version 1

Statut : brouillon du 2026-10-11, à valider par Kris.

Les mains sont ce qu'Emory fait dans le monde. L'esprit décide ; une main exécute, sans réfléchir.

## Deux sortes d'actions

- **Un service réseau** (chercher dans la bibliothèque musicale de Navidrome, plus tard d'autres
  API) : c'est un module du serveur MCP d'`esprit`, sur le serveur. Rien sur l'appareil.
- **Une action sur un appareil** (piloter le lecteur qui joue sur le PC, régler son volume) : c'est
  la **main** de cet appareil.

## La main d'un appareil

Un seul programme, `main`, le même sur chaque appareil, à côté du poste (voir le
[README](README.md)). Il est fait de modules ; chaque appareil n'active que les siens dans sa
configuration :

```toml
# PC de Kris
nom = "bureau"
modules = ["mpris"]
```

- Au démarrage, la main annonce sur le bus les modules actifs et leurs actions ; l'esprit en fait
  des outils pour le LLM, et seulement ceux-là. Demander de la musique là où il n'y a pas de
  lecteur donne une réponse simple, pas une erreur.
- Une commande arrive par le bus (demande avec réponse NATS, sujets `main.<appareil>.<action>`),
  la main l'exécute et répond avec le résultat. Un appareil éteint répond par un délai dépassé ;
  Emory le dit simplement.
- **Un module parle un standard, pas un logiciel.** Il marche avec tout ce qui respecte ce
  standard ; on n'écrit pas un module par application.

## Le premier module : MPRIS

MPRIS est le standard des lecteurs multimédia sous Linux (par D-Bus) : `poste music`, Spotify,
un navigateur le parlent. Avec lui, Emory sait sur le PC :

- lecture, pause, suivant, précédent, arrêt, volume ;
- dire ce qui joue : « c'est quoi ce morceau ? » ;
- **ouvrir un morceau, un album, une playlist** (`OpenUri`), après que l'esprit l'a trouvé dans
  Navidrome par l'API Subsonic : « mets du Daft Punk ».
- **baisser la musique quand Emory parle**, et la remonter après : la bouche prévient la main de
  l'appareil où elle parle.

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
- Les minuteurs et rappels d'Emory vivent dans `esprit`, pas sur l'appareil.

## Questions ouvertes

1. Le format des adresses passées à `OpenUri` pour un morceau, un album ou une playlist
   Navidrome, à fixer avec le dépôt `poste`.
