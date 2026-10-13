# L'esprit d'Emory (`mind`) : version 1

Statut : brouillon du 2026-10-11, à valider par Kris. Noms passés en anglais le 2026-10-11. Première version construite le 2026-10-12 (voir « La version 1 construite », à la fin).

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
- **le dictionnaire** : ajouter un mot au vocabulaire de la transcription, après le oui de
  l'utilisateur (construit le 2026-10-13, voir « Le dictionnaire » à la fin) ;
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
3. Haiku 5.5 réfléchit toujours (voir plus bas) : la réflexion coûte environ une seconde sur les
   tours où il lit des fiches. Garder Haiku, ou essayer Sonnet, ou l'API, à décider à l'usage.
4. Ce que la mémoire signale du jour : Emory n'en parle aujourd'hui que si Kris le demande ou si
   cela touche sa demande. Faut-il qu'il le dise de lui-même à l'ouverture d'une conversation ?

## La version 1 construite

Décidé et mesuré le 2026-10-12, en construisant `mind` (dossier `mind/`, binaire `mind`). Hors de
cette version : les minuteurs et les rappels, le web, la musique et les mains, la tâche de fond
qui écrit après une conversation, le conteneur moteur et le déploiement.

**Le bus.** `mind` écoute `hearing.*` sur un seul abonnement (les phrases d'un tour arrivent donc
avant le tour) et `ear.*.button`. Il publie `voice.say` et `voice.stop`, vers l'appareil qui a
entendu Kris. Ses journaux donnent les temps de chaque tour et chaque appel d'outil, jamais ce
qui a été dit ; les journaux des dépendances qui portent les messages (le client ACP, MCP, la
pile HTTP) restent bloqués à `warn` quoi que dise `RUST_LOG` (2026-10-12, revue).

**L'ordre entre les deux abonnements** (2026-10-12, revue). Rien ne garantit l'ordre entre un
appui et ce que dit `hearing`. Les tours sont des ULID, qui se rangent dans le temps : une phrase
ou un tour plus ancien que le dernier tour vu sur l'appareil est ignoré (un `hearing.turn` en
retard n'est pas répondu après un nouvel appui) ; un appui garde les recherches déjà lancées par
les phrases de son propre tour et n'arrête jamais la réponse à son propre tour ; un tour dont
l'appui s'est perdu arrête la réponse plus ancienne comme un appui.

**La conversation.** Une par appareil, ouverte au premier appui : la session ACP s'ouvre et le
`briefing` d'Hippocampe se lit pendant que Kris parle encore. Elle se ferme après 30 minutes
sans échange (`conversation.idle_minutes`), et la session avec (`session/close`).

**Le LLM.** `mind` est un client ACP (crate `agent-client-protocol` 3.3) qui lance
`claude-agent-acp` (0.89.1) et lui parle par son entrée et sa sortie standard. Les réglages de
`session/new` sont ceux mesurés le 2026-10-11 (plus haut), avec le modèle et l'effort pris dans la
configuration (`claude-haiku-5-5`, `low`). L'adaptateur hérite de l'environnement : la connexion
Claude Code locale sur le PC, `CLAUDE_CODE_OAUTH_TOKEN` quand il est défini. **Haiku 5.5 ne peut
pas ne pas réfléchir** : Claude Code refuse de couper la réflexion pour ce modèle (« Thinking
can't be turned off »), `thinking: disabled` est ignoré et il réfléchit en mode adaptatif, sans
montrer sa réflexion. Il ne réfléchit pas pour une histoire ou un « d'accord », il réfléchit dès
qu'il a des fiches à lire. `effort: "low"` reste le seul levier (sans lui, premier son à 2,3 s
au lieu de 2,0 s sur la même question).

**Les consignes** sont dans `mind/prompts/system.fr.md`. Chaque demande commence par le jour et
l'heure (`[Maintenant]`) ; la première d'une conversation porte aussi ce que la mémoire signale du
jour (le `briefing`, sans ce qui attend une confirmation de Kris) : la session le garde ensuite.
Emory n'en parle que si Kris le demande ou si cela touche sa demande. Quand le LLM ne répond pas,
Emory dit « Je n'arrive pas à réfléchir pour le moment. ». Une session qui n'a pas pu s'ouvrir
est rouverte au tour suivant.

**Une demande à la fois par session** (2026-10-12, revue). Le texte d'une demande encore en
cours serait pris pour la réponse de la suivante : le client ACP refuse une demande sur une
session dont la précédente n'a pas reçu sa fin. Une demande annule la précédente et attend sa
fin ; si elle ne vient pas en 10 s, la session est tenue pour cassée : elle est fermée (ce qui
termine l'ancienne demande) et une autre s'ouvre, qui reçoit à nouveau ce que la mémoire signale.
Un LLM muet pendant 60 s : Emory dit l'échec et la demande est annulée, mais la session attend
quand même sa fin.

**Les permissions.** L'adaptateur demande la permission avant chaque outil : `mind` ne l'accorde
qu'aux outils du serveur MCP d'Emory (`mcp__emory__*`), une seule fois (`allow_once`, choisi
nommément) ; tout autre outil est refusé et journalisé.

**Le flux.** Le texte est coupé en phrases à `.`, `!`, `?`, `…` suivis d'un espace, ou à un retour
à la ligne ; une phrase part dès que la suivante commence, ce qui garde toujours la dernière pour
la fin du flux : elle part alors marquée `last`. Une phrase sans fin de plus de 220 caractères est
coupée à sa dernière virgule. Les marques de mise en forme sont retirées. `voice` reçoit toujours
un `last`, quelle que soit la fin (erreur après quelques phrases, flux coupé, LLM muet, reste
vide après nettoyage) : la phrase d'échec si rien n'a été dit, sinon une dernière phrase vide,
que `voice` termine par un morceau vide qui porte la fin (2026-10-12, revue).

**L'interruption.** Un appui sur un appareil arrête la dernière réponse de cet appareil : la
demande en cours est annulée (`session/cancel`) et `voice.stop` part. L'interruption prend
d'abord un verrou et marque la réponse arrêtée, puis annule, puis arrête la voix. La demande part
sous ce même verrou, sans rien attendre : soit elle ne part jamais, soit elle part avant son
annulation, qui la suit vers l'agent dans l'ordre (2026-10-12, revue : avant, l'annulation pouvait
précéder la demande, qui était alors générée en silence). Les phrases sont mises en file sous ce
verrou : aucune phrase du tour ne suit son `voice.stop`. La file part vers le bus par une tâche à
elle, dans l'ordre : aucun aller-retour réseau sous le verrou, et l'envoi de chaque message
attend le serveur au plus 1 s. Le `voice.stop` part aussi quand la réponse est déjà écrite,
puisque `voice` peut encore la dire.

**La recherche en avance.** À chaque phrase, `mind` cherche dans Hippocampe la phrase telle
quelle (5 fiches, sans voisins, sans seuil), sur une connexion MCP gardée ouverte, et y renonce
au-delà de 300 ms. À la fin du tour, les fiches de toutes les phrases, chacune une fois et dix au
plus, entrent dans la demande : titre, type, slug, date, résumé, extrait.

**Le serveur MCP d'Emory** est servi par `mind` lui-même, en HTTP sur 127.0.0.1 (port libre),
avec une clé tirée au démarrage que seul l'adaptateur reçoit. Plus simple qu'un serveur lancé par
l'adaptateur : les outils partagent l'état de `mind` (plus tard les minuteurs, les mains sur le
bus) sans autre canal. C'est le seul serveur MCP du LLM. Chaque groupe d'outils est un module ;
ses outils s'appellent `<module>_<outil>`. Le premier module relaie Hippocampe : les outils de la
liste `hippocampe.tools` (par défaut `search`, `read`, `write`, `link`, `types`) qu'Hippocampe
offre à la clé utilisée. Un refus d'Hippocampe revient au LLM comme une erreur d'outil, qu'il
peut dire, et un appel sans réponse en 10 s aussi (`hippocampe.tool_timeout_ms`). La clé du
serveur est comparée en temps constant. Elle passe aujourd'hui sur la ligne de commande de
`claude` (`--mcp-config`, visible dans `ps`) : à corriger (2026-10-12, revue). Deux retouches au relais : une recherche du LLM se fait sans voisins s'il n'en demande
pas (avec les trois voisins par défaut, une recherche de dix fiches pesait environ 17 000 jetons,
une seconde de plus avant le premier mot) ; tout résultat porte `resultType`, que la révision MCP
parlée par le serveur exige et que l'adaptateur refuse sans lui.

**La clé d'Hippocampe** est lue dans son fichier à chaque ouverture de connexion, jamais
journalisée. Sur le PC, c'est la clé en lecture seule : Hippocampe n'offre alors que la lecture
(`search`, `read`, `types`), et Emory dit qu'il ne peut pas noter.

**Mesuré le 2026-10-12** sur le PC (vrai Hippocampe en lecture, Pocket, `hearing-replay` avec des
questions synthétisées par Pocket, 18 demandes au LLM) : la recherche en avance prend 67 à
159 ms. Du second appui au premier son :

| Tour | Premier mot du LLM | Premier son |
|---|---|---|
| Question tirée des fiches, premier tour d'une conversation (4 essais) | 1,6 à 1,9 s | 1,86 à 2,29 s |
| Discussion libre, « d'accord » (8 essais) | 0,6 à 0,95 s | 0,77 à 1,24 s |
| Longue phrase de Kris, 10 fiches (1 essai) | 2,0 s | 2,25 s |
| Le LLM cherche lui-même (2 essais) | 4,5 à 5,7 s | 4,8 à 5,9 s |

Le premier son suit le premier `voice.say` de 0,1 à 0,2 s. Les réponses sont justes et parlées.
Quand le mot clé est mal transcrit (« les mains des Maury » pour « d'Emory »), le LLM cherche
lui-même : c'est le cas le plus lent.

**Réponses courtes, et le tour vide** (2026-10-12, essai en direct de Kris). Deux constats.

*Trop long.* « Tu peux me refaire un topo sur ce qu'on a bossé aujourd'hui ? » a donné neuf phrases,
puis onze pour la suite : justes et tirées de la mémoire, mais trop pour être écoutées. Décidé avec
Kris : deux ou trois phrases courtes par défaut, quelle que soit la question, plus longues
seulement quand il demande des détails ; Emory peut proposer d'en dire plus (« Je t'en dis
plus ? »). Toujours aucune phrase d'attente, aucune liste lue à voix haute. La règle est dans
`mind/prompts/system.fr.md`, écrite avec un plafond net (trois phrases, un résumé de ce qui
reste) et les mots qui lèvent le plafond. Mesuré le jour même sur quatre vraies demandes à Haiku 5.5
(`low`), avec des fiches inventées pour la question du topo : 3, 2, 3 puis 2 phrases (47, 43, 41 et
15 mots), dont un « Je t'en dis plus ? » à la fin du topo. Une demande de détails (« détaille-moi
tout… ») est restée à trois phrases, mais les fiches de l'essai ne contenaient presque rien de plus
à dire : à revoir à l'usage.

*Le silence quand rien n'est entendu.* Plusieurs tours sont arrivés à `mind` avec un texte vide (le
casque avait envoyé du presque-silence) et Emory ne disait rien, ce que Kris a trouvé « un peu con ».
Désormais un tour dont le texte est vide, ou ne contient que des blancs et de la ponctuation (aucune
lettre ni chiffre), ne part pas au LLM : aucune demande, donc rien de pris sur le quota. Emory dit
une phrase fixe, « Je n'ai rien entendu. » (réglable : `conversation.nothing_heard`), par
`voice.say` avec `last`, vers l'appareil du tour. Le tour passe par les mêmes règles que les autres :
un tour plus ancien que le dernier vu sur l'appareil n'est pas dit ; il arrête la réponse plus
ancienne comme un appui ; et la phrase est une réponse comme une autre, donc un nouvel appui pendant
qu'elle se dit envoie `voice.stop` (sans rien annuler, il n'y a pas de demande). Le tour vide
n'ouvre aucune session : celle qui existe a été ouverte par l'appui, pendant que Kris parlait,
comme pour tout tour (ouvrir une session ne prend rien sur le quota ; retarder l'ouverture après le
tour aurait coûté de la latence à tous les autres tours). Un tour vide arrivé sans son appui n'en
ouvre aucune. Une phrase transcrite vide n'ouvre plus de conversation non plus.

**Emory ne connaît personne par son nom, et les consignes changent** (2026-10-13, essai en direct du
2026-10-12). Emory doit pouvoir être reprise par quelqu'un d'autre : plus aucun nom propre dans le
code, les consignes ni le texte des demandes. Le propriétaire vient de la configuration, section
`[owner]` de `mind` : `name` (son nom) et `address` (texte libre, en français, sur la façon de lui
parler, par exemple « un homme ; tu le tutoies »). Les deux sont facultatifs. Le fichier de consignes
(`prompts/system.fr.md`) est un gabarit : `{owner}` devient `name`, ou « la personne qui te parle »
sans nom ; `{address}` devient `address`, ou « tu tutoies cette personne » sans adresse. Une section
absente donne donc des consignes qui ne nomment personne, et non un refus de démarrer (un nom ou une
adresse vides sont refusés). Les marqueurs envoyés au LLM ne nomment personne non plus
(`[On te dit]`, `[Fiches trouvées pendant qu'on te parlait]`) : le nom dans chaque demande coûterait
des jetons sans rien apporter. Trois retouches aux consignes, tirées de l'essai : Emory parle au
propriétaire à la deuxième personne et reformule les fiches (« tu as… », pas « Kris a… »), sans dire
son nom sauf rarement, quand c'est naturel (elle l'avait dit une dizaine de fois en une séance) ;
deux ou trois phrases par défaut mais plus de plafond dur, qui coupait des réponses : elle finit
toujours sa pensée ; « Je t'en dis plus ? » seulement quand il reste quelque chose d'important, et
rarement (elle terminait presque chaque réponse ainsi). Le reste est inchangé (aucune phrase
d'attente, aucune liste lue, nombres en lettres, aucun identifiant lu, français). Les exemples et
les tests du dépôt ne contiennent plus de données personnelles ; `dev/mind.toml` porte les valeurs
du développeur, `[owner]` compris.

## Le dictionnaire (2026-10-13)

Kris l'a demandé le 2026-10-12 (« ça tu peux le dev déjà ») : Parakeet écorche les noms propres
de l'utilisateur, et Emory doit pouvoir en ajouter un au vocabulaire de `hearing` elle-même, en
conversation, **mais seulement une fois que l'utilisateur a confirmé** : Emory propose (« Tu veux
que j'ajoute Hemera au dictionnaire ? »), l'utilisateur dit oui, le mot est ajouté.

**La confirmation est tenue par `mind`, pas seulement demandée au LLM.** Un module `dictionary`
sur le serveur MCP d'Emory, deux outils :

- `dictionary_propose(word, heard_as?)` note la proposition dans la conversation et répond au LLM
  de demander à l'utilisateur, puis d'attendre son oui ;
- `dictionary_add(word)` n'est accepté que pour un mot proposé **à un tour précédent** de la même
  conversation : l'utilisateur doit avoir parlé depuis. Sinon il est refusé, avec un message qui
  dit de demander d'abord (mot jamais proposé, ou proposé à ce tour-ci). Accepté, il envoie le mot
  à `hearing` (`hearing.vocabulary.add`, voir [`contract.md`](contract.md)) et rend sa réponse
  au LLM : ajouté, déjà là, ou refusé et pourquoi.
- Les propositions meurent avec la conversation ; un mot ajouté consomme sa proposition.
- Les descriptions des outils sont courtes (elles partent avec chaque requête) et portent la
  consigne ; le prompt système n'a pas changé.

**Comment un outil sait de quelle conversation il est appelé.** Les appels du LLM arrivent au
serveur MCP sans le dire. Chaque conversation tire donc une clé au hasard, que sa session donne au
serveur dans l'en-tête `emory-conversation` (dans `session/new`, à côté de la clé du serveur) ; le
serveur la lit dans la requête HTTP de l'appel. `mind` tient le registre des conversations
ouvertes et du dernier tour de l'utilisateur dans chacune (`src/conversations.rs`) : le tour
avance quand un tour entendu arrive à `mind`, avant la requête, et la conversation sort du
registre quand elle se ferme. Une clé inconnue, ou un appel sans clé, est refusé. Le registre sert
à tout outil qui doit attendre l'utilisateur.

**La configuration** : `[dictionary]` `enabled` (par défaut oui ; non retire les deux outils) et
`timeout_ms` (2 s par défaut), l'attente de la réponse de `hearing`.

**Essayé en vrai** le 2026-10-13, Sonnet 5.5, sur un bus à part (3 requêtes) : à « quand je dis
Hemera, la transcription écrit « et mera », tu peux arranger ça ? », le LLM a appelé
`dictionary_propose` et demandé « Je l'ajoute ? » ; au « Oui, vas-y. » suivant, `dictionary_add`
est passé et `hearing` a écrit le mot, avec « et mera » en commentaire. À « Ajoute aussi Jellyfin
au dictionnaire, direct. », le LLM a appelé `dictionary_add` d'emblée : refusé (jamais proposé), il
a proposé puis demandé. La règle tient donc même quand l'utilisateur donne l'ordre directement,
au prix d'une question de plus ; à revoir avec Kris si cela agace. Sa formulation (« Le système
veut que je te demande d'abord confirmation ») est maladroite mais juste.

