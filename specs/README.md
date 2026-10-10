# Emory

Statut : vue d'ensemble décidée avec Kris le 2026-10-10.

Emory est l'assistant de Kris. Tout ce qui fait l'assistant est dans ce projet : les organes et
l'esprit. Sa mémoire à long terme n'y est pas : c'est Hippocampe, un projet à part.

## Ce qui est dans Emory

| Organe | Rôle | Spec |
|---|---|---|
| **Les oreilles** | transforment ce qui est dit en événements pour l'esprit | [`oreilles.md`](oreilles.md) (brouillon) |
| **La bouche** | fait parler Emory, et se tait quand Kris reprend la parole | à écrire |
| **Les yeux** | ce qu'Emory voit | à écrire |
| **Les mains** | ce qu'Emory fait dans le monde (appareils, services) | à écrire |
| **L'esprit** | reçoit les événements des organes, décide, lit et écrit dans Hippocampe, répond | à écrire ; le plus coûteux et le plus ouvert |

Un seul projet, un monorepo : un dossier par organe, chaque organe tourne comme un programme à part,
et ils ne se parlent que par un **contrat** commun (le format des événements entre organes), dans
un dossier à lui. Un organe se change sans toucher aux autres.

```
emory/
  specs/        cette vue d'ensemble, puis une spec par organe
  contract/     le format des événements entre organes
  ears/         capture/ (l'oreille, un adaptateur par micro) et center/ (le centre auditif)
  mouth/  eyes/  hands/  mind/   plus tard
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
