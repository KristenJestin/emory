# L'esprit d'Emory : version 1

Statut : brouillon du 2026-10-11, à valider par Kris.

L'esprit reçoit ce que les organes perçoivent, décide, lit et écrit dans Hippocampe, et répond.
C'est la partie la plus coûteuse et la plus ouverte.

## Ce qu'Emory sait faire en version 1

Décidé le 2026-10-11 : les quatre, la discussion libre comprise (sans elle, Emory ne serait qu'un
répondeur à Hippocampe).

1. **Répondre à partir d'Hippocampe** : « qu'est-ce qu'on avait décidé pour la musique ? »,
   « c'est quand l'échéance du contrat internet ? ».
2. **Noter dans Hippocampe** : « note que j'ai prêté la perceuse à Marc ».
3. **Discuter librement** : « explique-moi X », « aide-moi à réfléchir à Y ».
4. **Agir** : minuteurs et rappels en version 1. Les autres actions (musique, appareils, bureau
   de Kris) sont les mains, à spécifier à part.

## Deux parties

**Le relais**, notre programme `esprit`, en Rust, sans IA. Tout ce qui demande d'être prévisible :

- **la conversation en cours** : les échanges depuis le premier appui, gardés en mémoire ; la
  conversation se ferme après quelques minutes sans échange (réglable, 5 minutes pour commencer) ;
- **le routage** : la réponse part vers le poste qui a entendu Kris ;
- **le flux** : la réponse du LLM est découpée en phrases au fil de l'eau, chaque phrase part
  aussitôt vers la bouche (`bouche.dire`) ;
- **l'interruption** : un appui pendant qu'Emory parle arrête le LLM et envoie `bouche.stop` ;
- **les outils** : il exécute ce que le LLM demande (lire, chercher, écrire dans Hippocampe ;
  poser un minuteur, un rappel) et lui rend le résultat ;
- **les minuteurs et les rappels** : il les garde, et à l'échéance Emory le dit sur le poste ;
- **la tâche de fond** : après la fermeture d'une conversation, il demande au LLM ce qui mérite
  d'être gardé et l'écrit dans Hippocampe. Jamais pendant la réponse.

**Le LLM**, branché derrière par un adaptateur (voir « Les moteurs » dans le
[README](README.md)). Il reçoit la conversation, ses consignes et la liste des outils, et répond
en texte. Le changer ne touche pas au relais.

## Hippocampe

`esprit` est un client MCP d'Hippocampe, avec sa propre clé (`emory`). Ce qu'il écrit suit les
règles du [README](README.md) : ce que Kris dit est connu (`said_by`), ce qu'Emory suppose est
`inferred`, ce qu'Emory a fait ou vu lui-même est `seen_by`, un épisode par moment qui compte.
Une entrée écrite par Emory n'attend aucune validation ; Kris confirme quand il veut.

## Les consignes du LLM

- Il parle, il n'écrit pas : des phrases courtes, pas de listes ni de mise en forme, les nombres,
  les dates et les heures en toutes lettres.
- Pas de phrase d'attente (voir [`bouche.md`](bouche.md)).
- Il cherche dans Hippocampe avant d'affirmer quelque chose sur la vie de Kris, et dit quand il
  ne sait pas.

## La latence

Le but reste le premier son moins de 2 secondes après le second appui. Le premier mot du LLM
doit arriver en moins d'une seconde : c'est le critère principal du choix du modèle.

En version 1, le relais reste simple : quand le LLM appelle un outil, la réponse attend le
résultat. On mesure ; si l'attente se sent (une recherche dans Hippocampe avant de répondre), on
ajoute ensuite la recherche en parallèle et la reprise au bord d'une phrase prévues par
`bouche.md`.

## La confidentialité

C'est la seule exception au « tout en local » : le texte des conversations, et ce qu'Emory lit
d'Hippocampe pour répondre, partent chez le fournisseur du LLM. L'audio, lui, ne sort jamais.

## Questions ouvertes

1. Le modèle. Critères : premier mot en moins d'une seconde, bon français, bon usage des outils,
   coût. Dans le cloud pour commencer (l'API se paie à part de l'abonnement Claude) ; un modèle
   local quand il y aura une carte graphique, si un modèle de cette taille suffit.
2. La recherche sur le web pour la discussion libre : un outil de plus, et un coût de plus.
3. La durée d'une conversation avant sa fermeture : 5 minutes pour commencer, réglée à l'usage.
