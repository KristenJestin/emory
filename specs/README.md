# Emory

Statut : vue d'ensemble décidée avec Kris le 2026-10-10, le poste ajouté le 2026-10-11.

Emory est l'assistant de Kris. Tout ce qui fait l'assistant est dans ce projet : les organes et
l'esprit. Sa mémoire à long terme n'y est pas : c'est Hippocampe, un projet à part.

## Ce qui est dans Emory

| Organe | Rôle | Spec |
|---|---|---|
| **Les oreilles** | transforment ce qui est dit en événements pour l'esprit | [`oreilles.md`](oreilles.md) (brouillon) |
| **La bouche** | fait parler Emory, et se tait quand Kris reprend la parole | [`bouche.md`](bouche.md) (brouillon) |
| **Les yeux** | ce qu'Emory voit | à écrire |
| **Les mains** | ce qu'Emory fait dans le monde (appareils, services) | à écrire |
| **L'esprit** | reçoit les événements des organes, décide, lit et écrit dans Hippocampe, répond | à écrire ; le plus coûteux et le plus ouvert |

## Le poste

**Toute l'intelligence vit sur le serveur ; un appareil n'est qu'un poste de réception et
d'envoi** (décidé le 2026-10-11). Le poste est un petit programme, le même sur chaque appareil (le
PC de Kris d'abord, plus tard un Raspberry avec un micro et une enceinte) :

- il capte le micro (en version 1 : entre deux appuis sur un bouton, un raccourci Hyprland sur le
  PC) et envoie ce flux audio au serveur ;
- il joue le flux audio qui revient du serveur ;
- il n'a ni modèle, ni état, ni décision. Un adaptateur par type de micro et de haut-parleur.

Il garde une seule connexion avec le serveur (une WebSocket), avec son nom. Le serveur sait quel
poste a entendu Kris, et c'est par lui qu'Emory répond. Le poste est la prise des organes dans une
pièce : les oreilles et la bouche restent des organes à part, sur le serveur. Un site web pour le
téléphone pourra plus tard se brancher à la même connexion ; il ne remplace pas le poste sur le
PC, parce qu'un onglet ne capte pas un raccourci quand il n'a pas le focus.

## Le dépôt

Un seul projet, un monorepo : un dossier par organe, chaque organe tourne comme un programme à part,
et ils ne se parlent que par un **contrat** commun (le format des événements entre organes), dans
un dossier à lui. Un organe se change sans toucher aux autres.

```
emory/
  specs/        cette vue d'ensemble, puis une spec par organe
  contract/     le format des événements entre organes et des flux du poste
  station/      le poste, un adaptateur par micro et par haut-parleur
  ears/         le centre auditif
  mouth/        la synthèse de la voix
  eyes/  hands/  mind/   plus tard
```

**Tout tourne en local, sauf l'esprit** pour lequel il n'y aura sans doute pas le choix (décidé le
2026-10-11).

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
