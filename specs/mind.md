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
- Pas de phrase d'attente (voir [`voice.md`](voice.md)) : celle du signal d'attente (plus bas) est
  dite par le code, pas par le LLM.
- Changé le 2026-10-13 (essai en direct) : avant un outil, le LLM écrit une phrase de transition
  qui dit ce qu'il va faire et sur quoi (« Je lis ta fiche sur le déménagement. »), jamais une
  phrase vide (« je regarde », « un instant »). Elle part à la voix pendant que l'outil tourne.
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


## Le locuteur en appel direct à l'API : mesures (2026-10-13)

Une unité de mesure, sans changer `mind` : `mind/examples/talker-bench.rs` envoie à l'API Anthropic,
avec la crate `genai` 0.7.0-rc.4 (la 0.7 n'existe qu'en rc), la même requête que `mind` : le prompt
système rendu par `prompt::render`, le texte de `request::build` (fiches cherchées pendant que la
personne parle, point du jour à la première requête), et les outils d'Emory exécutés pour de bon
dans le processus (Hippocampe en lecture seule, le dictionnaire avec une écoute simulée). Les
questions sont celles du test en vrai du 2026-10-12 (`dev/talker-bench.toml`, 10 tours en 5
conversations). 77 requêtes à l'API, 0,41 USD ; 3 requêtes sur l'abonnement pour le chemin actuel.
Mesures, réponses et rapport complet dans `.agent-relay/runs/talker-bench/1/`.

Prix de liste lus le 2026-10-13 sur la page de tarifs d'Anthropic, par million de tokens (entrée /
écriture de cache 5 min / lecture de cache / sortie) : Haiku 5.5 0,10 / 0,125 / 0,01 / 0,50 USD ;
Sonnet 5.5 2 / 2,5 / 0,10 / 10 USD. Le cache exige au moins 512 tokens ; le préfixe d'Emory (prompt
et définitions des outils) en fait environ 5 200, donc il est mis en cache.

Temps en ms depuis le tour (après la recherche des fiches), médiane / pire. « Premier mot » est le
premier texte, « première phrase » ce que `voice` pourrait dire (même découpe que `mind`), « premier
mot utile » le premier texte de la réponse après les outils (le premier mot peut être une phrase
d'attente).

| modèle | tours | premier mot | première phrase | premier mot utile | fin | USD par réponse | USD par mois (50 par jour) |
|---|---|---|---|---|---|---|---|
| Haiku 5.5 sans réflexion | 20 | 733 / 1851 | 1017 / 2795 | 1323 / 4304 | 1649 / 5245 | 0,0011 | 1,7 |
| Sonnet 5.5 sans réflexion | 17 | 1523 / 3133 | 2552 / 8571 | 2534 / 8563 | 2690 / 8573 | 0,0150 | 22,5 |
| Sonnet 5.5, effort bas | 7 | 1090 / 5934 | 1600 / 5949 | 1090 / 5934 | 2602 / 5953 | 0,0140 | 21,1 |

Séparés selon les outils, Haiku donne la première phrase à 926 ms (médiane, sans outil) et 2032 ms
(avec outil) ; Sonnet à 2042 ms et 6118 ms. Le chemin actuel (ACP) : 1,85 à 5,2 s pour la première
phrase au test du 2026-10-12 (Sonnet 5.5 moyen) et 2,0 / 3,8 / 3,3 s pour Haiku 5.5 effort bas
rejoué le 2026-10-13 (sans recherche préalable des fiches).

Ce que cela veut dire :

- **Haiku en appel direct vaut la peine** : la première phrase vient en 1 s quand il répond sans
  outil, en 2 à 3 s avec outil, soit environ moitié moins que par ACP sans outil et un tiers de moins avec, pour 2 USD par mois.
- **Sonnet ne se justifie pas comme locuteur** : trois fois plus lent après un outil (6 s, et le
  texte d'après un outil arrive d'un seul bloc, pas au fil de l'eau), et treize fois plus cher.
- **Le cache ne change pas les temps, il divise le coût par deux environ** (le préfixe ne passe
  qu'une fois à plein tarif toutes les 5 minutes). Chaud ou non, le premier mot ne bouge pas à la
  précision de la mesure.
- **Les phrases d'attente** : malgré le prompt (« jamais de phrase d'attente »), les deux modèles
  écrivent « Je regarde… » avant un outil (Haiku 8 fois sur 10 tours avec outil, Sonnet sans
  réflexion 5 sur 7, Sonnet effort bas 0 sur 3). Le prompt est à durcir, ou le texte écrit avant un
  appel d'outil à jeter.
- **Les outils** : les deux lisent Hippocampe quand les fiches trouvées ne suffisent pas et
  proposent un mot avant de l'ajouter ; la règle de `mind` (pas d'ajout avant le oui) a servi une
  fois à Sonnet effort bas (il a appelé `dictionary_add` d'emblée, refusé, puis proposé).
- Limite de `genai` : il envoie `thinking: disabled`, que Sonnet 5.5 refuse (il veut `between_tools`,
  mis à la main dans le corps). Haiku 5.5 l'accepte.

## Le signal d'attente (2026-10-13)

Kris l'a demandé pour combler le silence entre la fin de son tour et le premier mot (1 à 2 s sans
outil, 3 à 5 s quand le LLM lit la mémoire) : les assistants du commerce le comblent par un petit
son. Deux sortes de signal sont codées, et on choisit dans la configuration, une fois installé,
pour les essayer. Le prompt du LLM continue d'interdire ses propres phrases d'attente : ici, c'est
le code qui parle, choisi et borné.

**Où ça vit.** `mind` possède le moment, parce que seul lui sait quand un tour se ferme
(`hearing.turn`), quand la première phrase de la réponse part, et quand le LLM appelle un outil
(l'appel arrive sur son propre serveur MCP, avec ses arguments). `mind` ne décide que *quoi* :
un `Cue` sur `voice.cue` (voir [`contract.md`](contract.md)), que `voice` joue (voir
[`voice.md`](voice.md), « Le signal d'attente »). Le code est dans `mind/src/waiting.rs`.
Choix de conception, le plus simple et le plus réversible : un sujet de plus plutôt qu'un champ
sur `Say`, parce qu'un son n'est pas une phrase (autre numérotation, jamais après la réponse) ; et
la synthèse du ton dans `voice`, pour que `emory-mouth` reste inchangé.

**La configuration**, section `[waiting]` de `mind` (par défaut `off`, le comportement d'avant) :

```toml
[waiting]
mode = "tone"          # "off", "tone" ou "speech"
pulse_delay_ms = 1500  # tone : sans réponse au bout de ce temps, la pulsation commence
speech_delay_ms = 800  # speech : sans réponse au bout de ce temps, une phrase pré-écrite
source = "both"        # speech : "prewritten", "contextual" ou "both"
max_phrases = 2        # speech : au plus ce nombre de phrases par tour
phrases = "mind/prompts/waiting.fr.toml"   # speech : le fichier de phrases (celui livré sans cette clé)
```

Le volume, la forme du ton et le temps entre deux pulsations sont dans la configuration de `voice`
(`[cue]`).

**`tone`.** Dès que le tour est fermé et qu'il contient des mots (un tour vide dit « Je n'ai rien
entendu. » et n'a pas de ton), `mind` envoie un `Cue` `tone`. Si aucune réponse n'a commencé au
bout de `pulse_delay_ms`, il envoie un `Cue` `pulse`, que `voice` répète jusqu'au premier mot.

**`speech`.** Une phrase courte, dite par la voix d'Emory :

- **pré-écrites** : une quarantaine de phrases françaises très courtes (« Je regarde. », « Je
  cherche. », « Voyons voir. »), tirées au hasard sans retomber sur les huit dernières. Le fichier
  `mind/prompts/waiting.fr.toml` est le modèle générique livré avec le dépôt, aussi intégré au
  programme ; le propriétaire écrit les siennes dans un autre fichier et met son chemin dans
  `phrases`. Aucune ne promet (« une seconde ») ni ne s'adresse à la personne.
- **contextuelles** : quand le LLM appelle un outil, `mind` le sait par son serveur MCP, avec les
  arguments. Le fichier de phrases a, par outil, un `argument` dont la valeur remplace `{}`
  (`hippocampe_search`, `query` = « Ali » donne « Je regarde ce que j'ai sur Ali. »), des phrases
  `plain` sans `{}`, et `silent = true` pour les outils qui répondent tout de suite. **Une valeur
  n'est dite que si elle est dicible** : cinq mots au plus, quarante caractères, sans chiffre
  collé à des lettres, ni `_ / \ @ # = < >`, ni mot unique à tiret (`mon-contrat-internet`). Un
  identifiant, un chemin, une adresse ne sont jamais lus. Sinon : la phrase `plain` de l'outil,
  puis une pré-écrite. `hippocampe_read` (dont l'argument est un identifiant) n'a que des `plain`.
- **quand** : `source = "prewritten"`, une pré-écrite après `speech_delay_ms` sans réponse ;
  `"contextual"`, une phrase à chaque appel d'outil seulement ; `"both"`, au premier appel d'outil
  la contextuelle, et la pré-écrite au délai sinon. Au plus `max_phrases` par tour, **deux
  secondes et demie d'écart au moins** (sinon deux phrases collées sonnent comme un bégaiement :
  « Je regarde. Je regarde ce que j'ai sur Ali. »), et **aucune une fois la réponse commencée**.
- **la réponse arrive pendant une phrase** : elle la suit, sans couper un mot (voir
  [`voice.md`](voice.md) pour le pourquoi).

**Fin du signal.** La première phrase de la réponse l'arrête (`mind` le dit avant de mettre la
phrase en file, sous le même verrou : aucun signal ne part après elle), un appui (barge-in) ou un
nouveau tour aussi, et `voice` ignore de toute façon un signal arrivé après la réponse.

**Les outils observés.** `Tools::observed_by` donne à un observateur chaque appel (conversation,
nom, arguments) avant l'exécution ; `Waiter` en est un. La conversation vient de l'en-tête
`emory-conversation` déjà posé pour le dictionnaire. Le signal ne dépend donc d'aucun LLM branché :
tout LLM passe par ce serveur MCP.

**Ce qui est testé** (avec des faux : `mind/src/waiting.rs`, `mind/tests/waiting.rs`, la file de
`voice`) : le ton part au tour fermé ; la pulsation après le délai et pas si la réponse est venue à
temps ; rien après la première phrase ; un appui ou un nouveau tour la coupe ; pas de répétition
des dernières phrases ; la phrase contextuelle depuis un appel avec une requête ; le repli sans
requête, avec un identifiant, un chemin ou une phrase trop longue ; l'écart et le plafond de
phrases ; les trois sources ; `off` n'envoie rien. De bout en bout sur un NATS à part (voice et
Pocket réels, `voice/examples/cue-check.rs`) : voir `voice.md`.

**Pas vérifié.** Rien de tout cela n'a été écouté par la personne qui l'a codé : le volume, le
caractère des quatre tons, la gêne ou non de la pulsation, et surtout le naturel de la phrase
d'attente avec la voix d'Emory (Pocket, puis Kyutai) restent à juger à l'oreille. Aucune requête
au vrai LLM : la phrase contextuelle depuis un vrai appel d'outil du LLM (par le serveur MCP) n'a
été testée qu'avec un faux appel. Les échantillons à écouter sont décrits dans le `LISEZMOI.txt`
de `.agent-relay/scratch/attente/`.

## Le choix du moteur : ACP ou appel direct, par la configuration (2026-10-13)

Kris veut essayer, une fois installé, quel chemin parle le plus vite, sans toucher au code. `mind`
a donc deux implémentations du `Brain` (le trait qui existait déjà) et `[llm] mode` choisit :

- `mode = "acp"` (par défaut, les fichiers d'avant marchent tels quels) : l'adaptateur ACP comme
  avant (`command`, `args`), le serveur MCP d'Emory sur la boucle locale ;
- `mode = "api"` : `mind` appelle lui-même l'API du fournisseur avec la crate `genai`
  (0.7.0-rc.4, désormais une vraie dépendance de `mind`) : réponse en flux, outils d'Emory donnés
  comme définitions de fonctions et exécutés dans le processus par les mêmes modules que le
  serveur MCP, avec la clé de la conversation (la règle du dictionnaire tient de la même façon),
  historique de la conversation gardé par `mind`, interruption qui coupe la connexion aussitôt.
  Pas de serveur MCP dans ce mode. Les appels passent par `Tools::call_in`, comme ceux du serveur : l'observateur
  des outils (le signal d'attente et sa phrase contextuelle) les voit dans les deux modes.

La configuration :

```toml
[llm]
mode = "api"
provider = "anthropic"          # voir la liste plus bas
model = "claude-haiku-5-5"
key_file = "~/.cles/anthropic.key"   # relue à chaque requête, jamais journalisée
# base_url = "https://api.cerebras.ai/v1/"   # une autre adresse ; obligatoire pour openai_compatible
thinking = "off"                # "off", "adaptive" ou un budget de tokens (2048)
# effort = "low"                # Anthropic : output_config.effort ; autres : profondeur de réflexion
# max_tokens = 2048
# cache = true                  # cache des prompts (Anthropic)
# fast = false
system_prompt = "mind/prompts/system.fr.md"
```

```toml
[llm]
mode = "acp"
command = "npx"
args = ["-y", "@agentclientprotocol/claude-agent-acp@0.89.1"]
model = "claude-haiku-5-5"
effort = "low"
thinking = "off"
system_prompt = "mind/prompts/system.fr.md"
```

Les choix :

- **Vérifié au démarrage**, avec le nom de l'option fautive : fournisseur inconnu, clé absente,
  option sans sens dans le mode (`command` en `api`, `key_file` en `acp`…), mode rapide sur un
  modèle qui ne l'a pas, budget de réflexion sous 1024 pour Anthropic, `base_url` sans `/` final.
- **La réflexion** : en `acp`, elle passe dans les options du SDK (`disabled`, `adaptive`,
  `enabled` avec `budgetTokens`). En `api` avec Anthropic, `off` envoie `thinking: disabled`
  (Haiku l'accepte) sauf pour Sonnet 5.5, qui le refuse (mesuré le 2026-10-13), et par prudence
  Sonnet 5 : ils reçoivent
  `between_tools` ; Opus en `off` n'a pas été essayé. Gemini reçoit son budget de 0 ; les autres
  fournisseurs ne reçoivent rien (un modèle sans raisonnement refuserait le paramètre). `adaptive`
  passe par l'effort (`effort` obligatoire hors Anthropic).
- **Le cache** (Anthropic) : un point sur les consignes (qui couvre les outils) et un point qui
  suit le dernier message. Essayé en vrai : chaque tour lit du cache tout l'historique précédent
  (7 071 tokens au 2e tour, 17 966 au 6e) et n'écrit que le nouveau (1 500 à 2 200 tokens) ; au
  second appel d'un tour à outil, tout ce que le premier avait envoyé est lu du cache (15 744
  tokens), seul le résultat de l'outil est nouveau. Gardé : sans lui,
  tout l'historique serait facturé en entrée pleine à chaque tour.
- **L'interruption** : la requête s'arrête et la connexion est lâchée ; le tour reste dans
  l'historique avec ce qui en a été dit (les appels d'outils sans résultat en sont retirés, sinon
  l'API refuserait la suite). Une requête en échec est retirée de l'historique.
- **Les fournisseurs** (lus dans les sources de `genai` 0.7.0-rc.4, `adapter_kind.rs` et
  `all_adapters.rs`) : natifs `anthropic`, `openai`, `openai_resp`, `gemini`, `ollama` (local,
  sans clé) ; compatibles OpenAI avec leur adresse connue de `genai` : `groq`, `mistral`,
  `deepseek`, `xai`, `together`, `fireworks`, `open_router` ; `cerebras`, que `genai` ne connaît
  pas, par le protocole OpenAI à `https://api.cerebras.ai/v1/` ; et `openai_compatible` pour tout
  autre serveur, avec `base_url`. `genai` en connaît d'autres (cohere, vertex, bedrock, zai,
  kimi…) : ajoutés à la liste quand il y aura une raison. Seul Anthropic a été appelé en vrai ;
  les autres ne sont vérifiés que par la configuration.

**Le mode rapide** (`fast = true`), documenté le 2026-10-13, jamais envoyé pour de vrai :

- Ce qu'il est : le même modèle Opus servi plus vite, jusqu'à 2,5 fois plus de tokens de sortie
  par seconde, **sans gain sur le délai du premier token**. Seulement Opus 5.5, Opus 5 et
  Opus 4.8 ; ni Haiku ni Sonnet. Pour un locuteur, qui attend surtout le premier mot, le gain
  serait faible.
- Par l'API : `speed: "fast"` dans le corps et l'en-tête `anthropic-beta: fast-mode-2026-02-01`.
  Aperçu de recherche : l'organisation doit avoir l'accès ouvert (sinon 429). Prix : Opus 5.5
  8 / 40 USD par million de tokens en entrée / sortie, contre 4 / 20 au tarif normal (le double),
  avec les multiplicateurs du cache par-dessus. Changer de vitesse invalide le cache.
- Par ACP (`claude-agent-acp` 0.89.1) : l'option de session `fast`, offerte seulement pour un
  modèle qui l'a, mise à `on` par `session/set_config_option` à l'ouverture ; `mind` refuse
  d'ouvrir la session si l'agent ne l'offre pas. Sur l'abonnement, **le mode rapide ne compte pas
  dans le forfait : il est payé sur les crédits d'usage (usage en plus du forfait), au même prix
  que l'API**, et seulement si ces crédits sont activés sur le compte.

**Essayé en vrai** le 2026-10-13, sur un bus à part, Haiku 5.5 sans réflexion, une conversation de
six tours (8 requêtes à l'API, environ 0,004 USD ; temps depuis la fin du tour, recherche des
fiches pendant la parole comprise) :

| tour | première phrase | fin |
|---|---|---|
| « T'as accès à Internet là ? » (ouverture, cache froid) | 1 266 | 1 579 |
| question sans outil | 950 | 1 349 |
| question interrompue après sa première phrase | 865 | `voice.stop` aussitôt l'appui |
| « topo de ma journée » (réponse d'après les fiches trouvées) | 943 | 1 526 |
| fiche lue avec `hippocampe_read` | 2 164 | 2 969 |
| « la première chose que je t'ai demandée ? » (historique) | 1 090 | 1 438 |

Même ordre que le banc (1 s sans outil, 2 s avec). Par ACP, deux tours rejoués juste après (2
requêtes d'abonnement) : 1 337 ms (ouverture de session comprise) et 868 ms. Sur ces deux tours
sans outil, ACP n'est donc pas plus lent ; l'écart mesuré par le banc (2 s contre 1 s) ne s'est
pas reproduit sur si peu de mesures. C'est justement ce que la configuration permet à Kris de
mesurer chez lui, en changeant `mode`.

Le banc (`examples/talker-bench.rs`) reste tel quel : il mesure les tokens et le coût de chaque
appel, ce que le `Brain` ne remonte pas. La sonde `examples/talker-probe.rs` (ex `acp-probe`) joue
des tours sur un bus à part quel que soit le mode, appui et phrase compris, et une interruption.

## Remplir le silence après la phrase d'attente (2026-10-13)

Mesuré en vrai avec `dev/mind.toml` (`speech`, `both`, phrase à 100 ms, acquittements neutres) :
requête partie à 0 s, phrase d'attente entendue de +0,36 s à +1,5 s, `hippocampe_search` appelé à
+2,5 s sans aucune phrase (l'écart de 2,5 s, une constante, partait du début de la première phrase
et n'était pas écoulé), première phrase de la réponse à +4,2 s : 2,7 s de silence entre
l'acquittement et la réponse. Kris : « qu'elle comble un peu les blancs, pour pas que ce soit trop
bizarre », et « faut que tout ça soit de la config ». Les valeurs par défaut gardent le
comportement d'avant ; tout est dans `[waiting]` (`mind/examples/mind.toml` documente chaque clé).

**Les clés ajoutées** (mode `speech`, sauf `pulse_delay_ms` qui existait) :

```toml
[waiting]
phrase_spacing_ms = 2500        # 0 à 60000 : au moins ce temps entre deux phrases d'un tour
phrase_spacing_from = "start"   # "start" : compté depuis l'envoi de la phrase précédente ;
                                # "end" : depuis sa fin estimée
pulse_after_speech = false      # true : la pulsation comble le silence après une phrase
pulse_delay_ms = 1500           # (déjà là) avec pulse_after_speech : silence après la fin de la phrase
prewritten_repeat_ms = 0        # 0 : aucune ; sinon une autre pré-écrite après ce silence de plus
```

**L'écart, depuis la fin.** `mind` connaît le texte d'une phrase, pas la durée de son audio : il
l'estime (`estimated_speech`) à 350 ms avant le premier son, plus 500 ms et 70 ms par caractère.
C'est calé sur la mesure d'aujourd'hui (neuf caractères : entendu dès +0,34 s, 1,16 s de long ; la
formule donne 1,48 s). L'estimation est grossière (la voix peut être plus lente ou plus rapide) mais
elle suffit : un écart « depuis la fin » est ce qu'on entend, un silence entre deux phrases, là où
« depuis le début » mélange la durée de la phrase et le silence. Le défaut reste `start` avec 2500
ms (le comportement d'avant) ; `end` est un choix de configuration, pas un changement silencieux.
Les valeurs ne se comparent pas : 800 ms depuis la fin vaut à peu près 2,3 s depuis le début pour
une phrase de neuf caractères. Les constantes de l'estimation sont dans le code, pas dans la
configuration : ce que le propriétaire règle est l'écart, pas la vitesse de la voix.

**La pulsation après une phrase.** Avec `pulse_after_speech = true`, quand une phrase est finie
(fin estimée) et qu'aucune réponse n'a commencé au bout de `pulse_delay_ms`, `mind` envoie un `Cue`
`pulse` (celui du mode `tone`, que `voice` répète à son intervalle jusqu'au premier mot et arrête
tout de suite, comme avant). `mind` ne l'envoie qu'une fois. Une phrase envoyée plus tard (la
contextuelle à un appel d'outil) **n'interrompt pas la pulsation et ne l'attend pas** : la file de
`voice` dit la phrase dès qu'elle arrive, la pulsation saute les échéances pendant que la phrase est
en synthèse (un seul écrivain des numéros de chunk à la fois), puis reprend après elle. Si la
pulsation n'est pas encore partie quand la phrase est envoyée, `mind` la repousse à la fin de cette
phrase plus `pulse_delay_ms` : une pulsation ne sonne jamais juste avant une phrase. Raison du choix
(plutôt qu'arrêter la pulsation à chaque phrase) : rien à changer dans `voice`, et la file le fait
déjà proprement ; un test de la file le fixe (`a_pulse_after_a_phrase_numbers_on_from_it...`). Sans
phrase dite, pas de pulsation : en `source = "contextual"` sans appel d'outil, c'est le silence
d'avant.

**Plus de phrases.** Avant, la pré-écrite ne partait qu'une fois, au délai (`speech_delay_ms`).
`prewritten_repeat_ms` ajoute, avec `source = "prewritten"` ou `"both"`, une autre pré-écrite après
ce silence de plus depuis la dernière phrase (compté comme l'écart : depuis le début ou la fin selon
`phrase_spacing_from`), jamais plus près que `phrase_spacing_ms`, jamais au-delà de `max_phrases`.
Une phrase envoyée (même contextuelle) repart pour un tour de ce délai ; sans cette clé, le délai
initial reste comme avant (une phrase au délai, puis rien). Jamais une phrase après la réponse :
`answer_started` et `cancel` retirent tout ce qui est prévu pour le tour, sous le verrou du
`Waiter`, avant que la phrase de la réponse soit mise en file.

**Pas fait, volontairement.** Le ton au début du tour avant la phrase en mode `speech` (un court
ton puis la phrase) : Kris ne l'a pas demandé, ce serait un son de plus sur chaque tour.

**Validation.** `max_phrases` de 1 à 5 ; `phrase_spacing_ms` de 0 à 60000 ; `prewritten_repeat_ms` 0
ou de 100 à 60000 ; `phrase_spacing_from` `"start"` ou `"end"`. Refusés avec un message qui nomme la
clé : `pulse_after_speech = true` ou `prewritten_repeat_ms` hors du mode `speech` (la clé ne ferait
rien), `prewritten_repeat_ms` avec `source = "contextual"` (aucune pré-écrite à répéter).

**Les valeurs de `dev/mind.toml`** : phrase à 100 ms, `phrase_spacing_ms = 800` depuis `"end"`,
`pulse_after_speech = true`, `pulse_delay_ms = 600`. L'écart n'est pas 1500 ms : la première phrase
finit vers +1,6 s, donc 1500 ms depuis sa fin ferait +3,1 s et l'appel d'outil de +2,5 s resterait
muet comme avant ; 800 ms laisse parler dès +2,4 s. Ce que ça donne sur le cas mesuré :

| temps | ce qu'on entend |
|---|---|
| 0 à +0,45 s | rien (la synthèse démarre) |
| +0,45 à +1,5 s | « D'accord. » (phrase envoyée à +0,1 s, finie estimée à +1,58 s) |
| +1,5 à +2,2 s | rien |
| +2,2 s | la pulsation (1,58 + 0,6 s) |
| +2,5 s | appel d'outil : « Je regarde ce que j'ai sur Ali. » envoyée (écart ok depuis +2,38 s) ; la pulsation saute ses échéances pendant la synthèse |
| +2,85 à ≈ +5 s | la phrase contextuelle |
| ≈ +5 s | la réponse, qui était prête à +4,2 s : **elle suit la phrase** (voir « la réponse arrive pendant une phrase »), donc ≈ 0,8 s de retard sur le premier mot |

Le silence le plus long passe de 2,7 s à 0,7 s, mais la phrase contextuelle retarde la réponse quand
celle-ci arrive pendant qu'on la dit. À juger à l'oreille : si ce retard gêne, `source =
"prewritten"` avec `pulse_after_speech` donne l'acquittement puis la pulsation sans phrase longue
au milieu.

**Ce qui est testé** (faux, temps en pause) : le cas ci-dessus, à +2,5 s la phrase contextuelle est
maintenant dite avec `end` et ne l'est pas avec `start` (et jamais avant la fin de la première phrase
pour `end`) ; la pulsation arrive après la phrase, une seule fois, et pas si la réponse est venue ;
une phrase à l'appel d'outil repousse la pulsation à sa fin, une phrase après le départ de la
pulsation ne la redemande pas ; rien après le premier mot ni après un barge-in ; la répétition des
pré-écrites (écart, plafond de `max_phrases`, jamais plus près que l'écart) ; les valeurs par défaut
inchangées ; la config (clés, bornes, refus). À travers tout `mind` (faux LLM, messages enregistrés) :
la phrase, la pulsation, le premier mot. La file de `voice` : une pulsation après une phrase prend
le numéro suivant, une phrase entre deux pulsations est dite, la pulsation s'arrête au premier mot et
à un `Stop`. `voice` n'a pas changé (un test de plus seulement).

**Pas vérifié.** Jamais écouté. Pas de bout en bout avec `voice` et un haut-parleur, ni avec un vrai
appel d'outil du LLM. L'estimation de la fin des phrases vient d'une seule mesure (neuf caractères).
