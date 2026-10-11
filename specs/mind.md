# L'esprit d'Emory (`mind`) : version 1

Statut : brouillon du 2026-10-11, à valider par Kris. Noms passés en anglais le 2026-10-11.

L'esprit reçoit ce que les organes perçoivent, décide, lit et écrit dans Hippocampe, et répond.
C'est la partie la plus coûteuse et la plus ouverte.

## Ce qu'Emory sait faire en version 1

Décidé le 2026-10-11 : les quatre, la discussion libre comprise (sans elle, Emory ne serait qu'un
répondeur à Hippocampe).

1. **Répondre à partir d'Hippocampe** : « qu'est-ce qu'on avait décidé pour la musique ? »,
   « c'est quand l'échéance du contrat internet ? ».
2. **Noter dans Hippocampe** : « note que j'ai prêté la perceuse à Marc ».
3. **Discuter librement** : « explique-moi X », « aide-moi à réfléchir à Y ».
4. **Agir** : minuteurs et rappels, et la musique du PC (voir [`hands.md`](hands.md)).

## Deux parties

**Le relais**, notre programme `mind`, en Rust, sans IA. Tout ce qui demande d'être prévisible :

- **la conversation en cours** : les échanges depuis le premier appui, gardés en mémoire ; la
  conversation se ferme après un temps sans échange, réglable, large pour commencer (30 minutes) ;
- **le routage** : la réponse part vers l'appareil qui a entendu Kris ;
- **le flux** : la réponse du LLM est découpée en phrases au fil de l'eau, chaque phrase part
  aussitôt vers `voice` (`voice.say`), la dernière marquée `last` ;
- **l'interruption** : un appui pendant qu'Emory parle arrête le LLM et envoie `voice.stop` ;
- **les outils** : il sert le serveur MCP d'Emory (plus bas) et exécute ce que le LLM demande ;
- **les minuteurs et les rappels** : il les garde, et à l'échéance Emory le dit sur l'appareil ;
- **la tâche de fond** : après la fermeture d'une conversation, il demande au LLM ce qui mérite
  d'être gardé et l'écrit dans Hippocampe. Jamais pendant la réponse.

**Le LLM**, branché derrière par un adaptateur (voir « Les moteurs » dans le
[README](README.md)). Il reçoit la conversation, ses consignes et la liste des outils, et répond
en texte, en appelant les outils du serveur MCP d'Emory (plus bas). Le changer ne touche pas au
relais.

**Pour commencer : Claude (Haiku ou Sonnet) par ACP** (décidé le 2026-10-11). `mind` est un
client ACP (Agent Client Protocol, la crate officielle `agent-client-protocol`) et pilote Claude
Code par l'adaptateur `claude-agent-acp`, dans un conteneur moteur à lui :

- c'est l'abonnement Claude de Kris qui paie, pas l'API ; chaque échange prend sur son quota ;
- **Claude Code n'a aucun de ses propres outils** (ni shell, ni fichiers, ni web) : tout ce
  qu'Emory peut faire passe par **un seul serveur MCP, celui d'Emory**, servi par `mind`
  (décidé le 2026-10-11, et maintenu : vrai pour tout LLM branché, quoi qu'il arrive) ;
- les réglages qui le permettent, à passer dans `_meta` de `session/new` (mesuré le 2026-10-11
  avec `claude-agent-acp` 0.89.1) : `systemPrompt` en chaîne (remplace tout le prompt de Claude
  Code), `claudeCode.options.tools = []`, `settingSources: []` (sinon les réglages, hooks et
  skills de Kris entrent dans le contexte), `skills: []`, `strictMcpConfig: true`. Il reste un
  socle d'environ 700 jetons qu'on ne peut pas retirer ;
- la session ACP reste ouverte le temps d'une conversation, pour ne pas repayer le démarrage.

Mesuré le 2026-10-11 (une centaine de requêtes, une à la fois) : premier mot en 1,25 s (Haiku)
et 0,98 s (Sonnet) en médiane quand il répond directement, mais 3,4 à 4,3 s quand il appelle un
outil avant de répondre, parce qu'il fait alors deux allers-retours. Haiku n'est pas plus rapide
que Sonnet. Une session s'ouvre en 0,5 s, une fois par conversation.

Seconde mesure le même jour, avec les fiches d'Hippocampe déjà jointes à la question (112 tours) :
premier mot en **0,68 s** en médiane pour Haiku avec `effort: "low"` (pire 1,5 s), 0,84 s pour
Sonnet ; aucun appel d'outil, réponses justes, deux ou trois phrases. Haiku écrit encore des
chiffres (« le 2 septembre ») dans un tiers des réponses : `voice` normalise les nombres de
toute façon.

## Le serveur MCP d'Emory

Le LLM ne voit qu'un seul jeu d'outils : celui que sert `mind`. Il les exécute, les journalise
et en garde le contrôle ; changer de LLM ne change pas les outils. Chaque groupe d'outils est un
module, et ce qui s'ajoutera (les mains, les yeux) devient un module de plus :

- **Hippocampe** : `mind` relaie les outils d'Hippocampe (lire, chercher, écrire…), avec la clé
  `emory` ;
- **le temps** : minuteurs et rappels ;
- **le web** : chercher et lire une page. La recherche passe par une instance SearXNG sur le
  serveur, en local, sans clé ni abonnement ;
- **la musique** : chercher dans Navidrome par l'API Subsonic, un module de l'`emory-hands` du
  serveur (appareil `server`) ;
- **les mains** : les actions annoncées par l'`emory-hands` de chaque appareil (MPRIS sur le PC) ;
- **plus tard** : les yeux.

## Hippocampe

`mind` est un client MCP d'Hippocampe, avec sa propre clé (`emory`). Ce qu'il écrit suit les
règles du [README](README.md) : ce que Kris dit est connu (`said_by`), ce qu'Emory suppose est
`inferred`, ce qu'Emory a fait ou vu lui-même est `seen_by`, un épisode par moment qui compte.
Une entrée écrite par Emory n'attend aucune validation ; Kris confirme quand il veut.

## Les consignes du LLM

- Il parle, il n'écrit pas : des phrases courtes, pas de listes ni de mise en forme, les nombres,
  les dates et les heures en toutes lettres.
- Pas de phrase d'attente (voir [`voice.md`](voice.md)).
- Il cherche dans Hippocampe avant d'affirmer quelque chose sur la vie de Kris, et dit quand il
  ne sait pas.

## La latence

Le but reste le premier son moins de 2 secondes après le second appui. Le premier mot du LLM
doit arriver en moins d'une seconde : c'est le critère principal du choix du modèle.

**Le relais cherche lui-même, avant le LLM** (décidé le 2026-10-11). Un LLM qui doit appeler un
outil avant de parler fait deux allers-retours et perd 2 à 3 secondes, quel que soit le LLM. Donc
pendant que Kris parle, à chaque phrase transcrite (`hearing.sentence`), `mind` interroge
Hippocampe avec cette phrase, sans IA ; au second appui, le contexte est déjà prêt, et il le
glisse dans la demande au LLM, qui répond en un seul aller-retour. Ce travail en avance ne fait
que lire : rien d'irréversible avant la fin du tour (`hearing.turn`). Le LLM
garde ses outils pour ce que ce premier contexte ne couvre pas, et pour agir (noter, minuteur).

Mesuré le 2026-10-11 sur le vrai Hippocampe : une recherche prend environ 25 ms depuis le serveur
(deux appels sur trente-deux à 2 s, sans doute un délai de connexion). `mind` garde donc une
connexion ouverte et renonce à la mémoire pour ce tour au-delà de 300 ms. Il ne colle dans la
demande que le titre, le résumé et l'extrait de chaque fiche (une réponse brute fait environ
1 100 jetons pour 5 fiches, surtout des identifiants et des dates).

**`mind` passe la phrase de Kris telle quelle** : trouver ce qui est pertinent dans une phrase
entière est le travail d'Hippocampe, pas d'Emory (décidé le 2026-10-11). Aujourd'hui la recherche
d'Hippocampe exige tous les mots et ne trouve presque rien avec une phrase parlée ; c'est une
demande générale faite à Hippocampe (recherche en langage naturel classée par pertinence).

À l'ouverture d'une conversation, `mind` lit aussi ce qu'Hippocampe signale du moment (les
échéances du jour, `heads_up`) et le garde pour toute la conversation.

Si l'attente se sent encore, on ajoute ensuite la recherche en parallèle et la reprise au bord
d'une phrase prévues par [`voice.md`](voice.md), jamais une phrase d'attente.

## La confidentialité

C'est la seule exception au « tout en local » : le texte des conversations, et ce qu'Emory lit
d'Hippocampe pour répondre, partent chez le fournisseur du LLM. L'audio, lui, ne sort jamais.

## Questions ouvertes

1. Haiku 5.5 en `effort: "low"` par défaut (le plus rapide et le moins cher en quota), Sonnet si
   les réponses manquent de soin : à confirmer à l'usage.
2. La durée d'une conversation avant sa fermeture : 30 minutes pour commencer, réglée à l'usage.
