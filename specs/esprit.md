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
  conversation se ferme après un temps sans échange, réglable, large pour commencer (30 minutes) ;
- **le routage** : la réponse part vers le poste qui a entendu Kris ;
- **le flux** : la réponse du LLM est découpée en phrases au fil de l'eau, chaque phrase part
  aussitôt vers la bouche (`bouche.dire`) ;
- **l'interruption** : un appui pendant qu'Emory parle arrête le LLM et envoie `bouche.stop` ;
- **les outils** : il sert le serveur MCP d'Emory (plus bas) et exécute ce que le LLM demande ;
- **les minuteurs et les rappels** : il les garde, et à l'échéance Emory le dit sur le poste ;
- **la tâche de fond** : après la fermeture d'une conversation, il demande au LLM ce qui mérite
  d'être gardé et l'écrit dans Hippocampe. Jamais pendant la réponse.

**Le LLM**, branché derrière par un adaptateur (voir « Les moteurs » dans le
[README](README.md)). Il reçoit la conversation, ses consignes et la liste des outils, et répond
en texte, en appelant les outils du serveur MCP d'Emory (plus bas). Le changer ne touche pas au
relais.

**Pour commencer : Claude (Haiku ou Sonnet) par ACP** (décidé le 2026-10-11). `esprit` est un
client ACP (Agent Client Protocol, la crate officielle `agent-client-protocol`) et pilote Claude
Code par l'adaptateur `claude-agent-acp`, dans un conteneur moteur à lui :

- c'est l'abonnement Claude de Kris qui paie, pas l'API ; chaque échange prend sur son quota ;
- **Claude Code n'a aucun de ses propres outils** (ni shell, ni fichiers, ni web) : tout ce
  qu'Emory peut faire passe par **un seul serveur MCP, celui d'Emory**, servi par `esprit`
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

## Le serveur MCP d'Emory

Le LLM ne voit qu'un seul jeu d'outils : celui que sert `esprit`. Il les exécute, les journalise
et en garde le contrôle ; changer de LLM ne change pas les outils. Chaque groupe d'outils est un
module, et ce qui s'ajoutera (les mains, les yeux) devient un module de plus :

- **Hippocampe** : `esprit` relaie les outils d'Hippocampe (lire, chercher, écrire…), avec la clé
  `emory` ;
- **le temps** : minuteurs et rappels ;
- **le web** : chercher et lire une page. La recherche passe par une instance SearXNG sur le
  serveur, en local, sans clé ni abonnement ;
- **plus tard** : les mains (musique, appareils, bureau de Kris), les yeux.

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

**Le relais cherche lui-même, avant le LLM** (décidé le 2026-10-11). Un LLM qui doit appeler un
outil avant de parler fait deux allers-retours et perd 2 à 3 secondes, quel que soit le LLM. Donc
dès que la transcription arrive, `esprit` interroge Hippocampe avec la phrase de Kris, sans IA,
et glisse ce qu'il trouve dans la demande au LLM, qui répond en un seul aller-retour. Le LLM
garde ses outils pour ce que ce premier contexte ne couvre pas, et pour agir (noter, minuteur).

Si l'attente se sent encore, on ajoute ensuite la recherche en parallèle et la reprise au bord
d'une phrase prévues par `bouche.md`, jamais une phrase d'attente.

## La confidentialité

C'est la seule exception au « tout en local » : le texte des conversations, et ce qu'Emory lit
d'Hippocampe pour répondre, partent chez le fournisseur du LLM. L'audio, lui, ne sort jamais.

## Questions ouvertes

1. Haiku ou Sonnet : aussi rapides l'un que l'autre en premier mot ; à trancher sur la qualité
   des réponses et le quota.
2. La durée d'une conversation avant sa fermeture : 30 minutes pour commencer, réglée à l'usage.
