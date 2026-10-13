# La bouche et la voix d'Emory : version 1

Statut : brouillon du 2026-10-11, à valider par Kris. Noms passés en anglais le 2026-10-11.

La bouche fait parler Emory. Elle a deux parties (voir le [README](README.md)) : `voice`, sur le
serveur, reçoit le texte de `mind`, le transforme en voix et envoie le flux audio à la bouche de
l'appareil qui a entendu Kris ; `emory-mouth`, sur l'appareil, joue ce flux.

## Principes

1. **La voix se fabrique sur le serveur, en local.** `emory-mouth` ne fait que jouer le flux qui
   revient. Aucun service de synthèse dans le cloud en version 1.
2. **En flux, phrase par phrase.** La bouche commence à parler dès que l'esprit a écrit sa
   première phrase. Le but : le premier son moins de 0,2 s après la réception d'une phrase.
3. **Pas de phrase d'attente du LLM.** Le LLM ne dit jamais « je regarde » pour faire patienter
   (le signal d'attente du 2026-10-13, plus bas, est autre chose : c'est le code qui le joue, choisi
   et borné, et il se coupe dès que la réponse commence). Le LLM répond
   avec ce qu'il sait ; quand un travail lancé en parallèle aboutit, l'esprit jette les phrases
   pas encore dites et la bouche enchaîne au bord de la phrase en cours (« … ah, je l'ai : »),
   jamais au milieu d'un mot. Ce qui est dit en attendant doit être utile, pas du remplissage.
4. **Le moteur est un adaptateur.** Changer de moteur (passer sur la carte graphique, ou au cloud
   un jour) ne touche ni à `mind` ni à `emory-mouth`.
5. **Emory ne gêne pas le reste du serveur**, avec les mêmes limites que `hearing`.

## Le signal d'attente (2026-10-13)

Construit le 2026-10-13 : un son, ou une phrase courte, pendant que `mind` prépare sa réponse (1 à
2 s sans outil, 3 à 5 s quand le LLM lit la mémoire). `mind` décide quand (voir
[`mind.md`](mind.md), « Le signal d'attente ») et envoie un `Cue` sur `voice.cue` ; `voice` fabrique
et joue le son, par le chemin de la voix, donc `emory-mouth` n'a rien à savoir de nouveau.

**Les sons sont dessinés par le code** (`voice/src/cue.rs`), sans fichier audio : des sinus sous
une enveloppe (attaque de 6 ms, chute exponentielle, un faible second harmonique), à 24 kHz,
comme les moteurs.

- **Le ton** : deux notes montantes (660 puis 880 Hz, 130 et 190 ms, 25 ms entre elles, 0,34 s
  en tout). Quatre formes au choix dans `[cue] tone` : `rising` (défaut), `bright` (plus aiguë et
  plus courte), `warm` (plus grave, plus ronde), `single` (une seule note).
- **La pulsation** : une note douce de 523 Hz, 160 ms, en cloche, à 0,45 fois le volume du ton.
  `voice` la renvoie toutes les `pulse_interval_ms` (1,6 s) tant que la réponse n'a pas commencé,
  et s'arrête seule au bout de 30 s si `mind` disparaît.
- **La phrase** passe par le moteur comme une phrase de la réponse, avec le numéro de phrase
  `u32::MAX`.
- Le volume (`[cue] volume`, de 0 à 1, 0,3 par défaut) est le pic du ton, en fraction du maximum.

**La file** (`voice/src/queue.rs`) tient le signal : il n'a qu'un écrivain à la fois sur la
numérotation des morceaux (`Emory-Seq`), donc un son n'est jamais émis pendant qu'une phrase est
synthétisée. Règles :

- un `Cue` ne démarre rien pour un tour déjà arrêté ou remplacé, ni **après la première phrase de
  la réponse** (alors la pulsation s'arrête, les phrases d'attente pas encore commencées sont
  jetées) ;
- une pulsation n'est pas un flux en temps réel : un son d'un coup, répété par l'échéance de la
  file. Le plus tard qu'elle puisse gêner la première parole est donc la fin de la pulsation déjà
  envoyée (160 ms), et rien n'est à couper ;
- **la réponse qui arrive pendant une phrase d'attente la suit** (décidé le 2026-10-13). Si le son
  de la phrase est déjà parti, elle va jusqu'au bout et la réponse enchaîne derrière (le tampon
  de `emory-mouth` joue dans l'ordre) : couper au milieu d'un mot sonne cassé, et ce que ça coûte
  est au plus une seconde de parole. Si aucun son n'en est parti (elle attend le moteur), elle est
  coupée : personne n'a rien entendu, la réponse passe tout de suite ;
- deux phrases d'attente d'un même tour passent l'une après l'autre ; un `Stop` coupe tout.

**Mesuré le 2026-10-13** sur un bus à part (voice et Pocket réels, `voice/examples/cue-check.rs`,
rien de joué) : le ton part 6 ms après le `Cue` ; une pulsation toutes les 1,6 s ; la réponse
envoyée à 4,5 s fait son premier son à 4,8 s et aucune pulsation ne la suit (la dernière était à
3,7 s) ; avec une phrase d'attente (premier son de la phrase à 0,3 s) et la réponse envoyée à
0,9 s, la phrase est entière puis la réponse, numérotation sans trou ; après un `Stop`, plus aucune
pulsation. Jamais écouté.

**Une limite** : après une phrase d'attente, `Speaking` `end` part au bout du délai de grâce
(700 ms) si la réponse n'est pas encore là, puis `start` à nouveau : la musique peut remonter
un instant entre les deux. Un ton ou une pulsation ne baisse pas la musique.

## Le volume de la voix (2026-10-13)

Kris a essayé la chaîne en direct : la voix (Pocket, estelle) est bien plus faible que le ton
d'attente, et trop faible en général. Le moteur envoie son audio tel quel (pics de Pocket
mesurés entre 0,3 et 0,5 du maximum) ; le ton, lui, est normalisé à son pic (`[cue] volume`).

**Construit** (`voice/src/gain.rs`) : une clé de configuration, `gain` (à la racine, à côté de
`voice` : un tableau `[voice]` entrerait en conflit avec la clé `voice`), de 0,1 à 8, 1 par
défaut. C'est un facteur sur l'amplitude de l'audio de **tout** moteur (Pocket et Kyutai),
appliqué morceau par morceau juste avant l'envoi : pas d'attente de la phrase entière, pas de
latence en plus. Le signal d'attente garde son propre `[cue] volume`. Une valeur hors de la
plage arrête le démarrage avec un message qui nomme la clé.

**Pourquoi un gain simple plutôt qu'un niveau cible** : un niveau cible demande de connaître le
pic ou le niveau moyen de la phrase, donc de l'attendre (ou de deviner sur le début) ; un gain
est sans état, réglable à l'oreille et n'ajoute rien. Les moteurs ont des niveaux différents, mais
`gain` est dans le fichier de chaque `voice` : on règle donc par moteur.

**Contre l'écrêtage** : un échantillon coupé net au maximum crépite. Jusqu'à 0,8 du maximum, un
échantillon est seulement multiplié ; au-delà, la courbe s'infléchit doucement vers le maximum
(une `tanh` qui part avec la même pente, sans angle) et ne le dépasse jamais : une syllabe forte
est arrondie, jamais coupée. La courbe n'a pas de mémoire, donc rien ne claque à la jointure de
deux morceaux. Avec un gain d'exactement 1, les octets passent tels quels (comportement
d'avant).

**Mesuré le 2026-10-13** sur un bus à part, avec `voice` et Pocket réels (`voice-try`, cinq
phrases de 1,6 à 5 s, une fois chacune par gain, rien de joué) : les pics sont de 0,29 à 0,37 à
`gain = 1` et de 0,58 à 0,93 à `gain = 2` (aucun échantillon au-dessus de 0,976). Pocket n'est
pas déterministe : sur cinq autres phrases passées d'un trait, le pic allait de 0,49 (`gain = 1`)
à 0,72 (`gain = 2`) et 0,96 (`gain = 3`). **Réglé à 2,0** dans `dev/voice.toml` ; `dev/voice-kyutai.toml`
a aussi 2,0, sans mesure (le moteur Kyutai ne tournait pas), le limiteur gardant le son sous le
maximum. **Jamais écouté** : le niveau perçu par rapport au ton à 0,1 reste à juger à l'oreille.

## L'interruption

En version 1, un appui sur le bouton pendant qu'Emory parle le fait taire : `emory-mouth` jette le
son du tour dès l'appui, `voice` coupe le flux et jette ce qui restait à dire (`voice.stop`), et
`emory-ear` se met à écouter. Avec l'oreille de pièce, plus tard, ce sera la reprise de parole de
Kris (voir [`hearing.md`](hearing.md)).

## Le moteur

Recherche du 2026-10-11 (chiffres des éditeurs, rien de mesuré chez Kris) :

- **Sur processeur, pour commencer : Kyutai Pocket TTS** (100M, CC-BY-4.0). Français depuis
  mai 2026, en flux, environ 200 ms jusqu'au premier son sur 2 cœurs, clonage à partir de 5 s
  d'audio. Le seul trouvé qui fait les trois sur processeur.
- **En secours : Piper.** Très léger et solide, mais la voix sonne robotique.
- **Avec la carte graphique : Kyutai TTS 1.6B en_fr** (CC-BY-4.0). Fait pour le français et
  l'anglais, lit le texte pendant que l'esprit l'écrit, environ 220 ms ; 4 à 6 Go de mémoire
  vidéo, qui tiennent juste sur une RTX 3060 avec la transcription et un petit modèle de langage.
- **Écartés** : Kokoro (une seule voix française, médiocre), Voxtral TTS (environ 16 Go de
  mémoire vidéo), Chatterbox (pas de vrai flux), XTTS v2 (projet arrêté, licence non commerciale).

Le moteur est un adaptateur (voir « Les moteurs » dans le [README](README.md)) : Pocket TTS est
en Python, il tourne donc dans un conteneur à lui, que `voice` (en Rust) appelle
(`pocket-tts serve` fournit déjà un serveur HTTP) ; des portages en Rust existent mais sont trop
jeunes (0 à 5 étoiles en octobre 2026). Le serveur de Kyutai pour le 1.6B est en Rust.

**Choisi au banc du 2026-10-11** (serveur, cœurs E) : **Pocket TTS, modèle `french` (6 couches)**.
Premier son en 0,19 s (au pire 0,24 s), facteur temps réel 0,53 sur un seul fil, 1,6 cœur et
1 Go de mémoire. Le modèle `french_24l` (24 couches) est trop lent sur les cœurs E (facteur
1,86) : il attendra une carte graphique. Piper calcule plus vite mais ne parle qu'une fois la
phrase entière calculée (premier son 0,7 à 1,1 s), et sa voix est plus robotique : il reste le
secours. Le clonage de voix de Pocket n'a pas été essayé (modèle soumis à un jeton Hugging Face).

## La voix

Écoutées le 2026-10-11 : les voix précalculées de Pocket (estelle, jean, marius, fantine,
azelma, et estelle en 24 couches) et trois voix de Piper. Pour Kris, toutes sont médiocres ; la
seule à peu près correcte est **estelle** (Pocket, 6 couches), la seule française d'origine, les
autres étant des voix anglaises réencodées. Estelle pour commencer ; une meilleure voix viendra
du clonage ou d'un autre moteur. **VoiceStudio** (application locale qui embarque plusieurs moteurs, dont
Pocket TTS) sert à cette audition ; il ne sert pas de bouche, trop lourd et fait pour produire
des fichiers, pas pour parler en flux.

**Écoutées le 2026-10-12**, sur de vraies réponses d'Emory de 9 à 11 phrases : sur la durée,
estelle « bave » en fin de mots. Les nouveaux poids français de Pocket (révision 3e82814a, PR
#344), le modèle `french_24l` et des morceaux de texte plus courts n'y changent rien d'audible :
le défaut vient du moteur. **Kyutai TTS 1.6B est nettement meilleur.** Choisie par Kris : la voix
`cml-tts/fr/5476_3103_000072-0001` (voix féminine plutôt grave, environ 185 Hz), avec
`padding_bonus = -3` (`max_padding = 8`, `temp = 0.6`, `cfg_coef = 2.0`), environ un quart plus
rapide que le réglage par défaut, que Kris trouvait lent. Sur la carte graphique du PC (RTX 3070
Ti), facteur temps réel d'environ 0,45 par phrase ; premier son en flux pas encore mesuré.

**Décision (Kris, 2026-10-12)** : Kyutai 1.6B, cette voix, est la voix visée pour le serveur, qui
recevra une carte graphique (12 Go au moins, pour la voix et la transcription ensemble). Rien
d'Emory ne prend la carte graphique du PC de Kris, déjà utilisée par sa dictée. D'ici là, Pocket
estelle reste la voix de développement, sur processeur.

## Le moteur Kyutai, tel que construit (2026-10-12)

Kyutai TTS 1.6B est un moteur de `voice` à côté de Pocket, choisi dans la configuration
(`engine = "kyutai"`, `[kyutai]` pour les réglages). Pocket reste le moteur par défaut et la voix
de développement.

- **Le moteur** (`engines/kyutai/`) est un petit serveur HTTP en Python sur le paquet `moshi`
  de Kyutai, sur la carte graphique : `POST /tts` reçoit la phrase en JSON et renvoie un WAV
  24 kHz en flux, trame par trame (80 ms), comme Pocket ; fermer la connexion arrête le calcul au
  pas suivant. Une phrase à la fois.
- **Pourquoi pas le serveur de Kyutai** (`moshi-server`, en Rust) : pour la synthèse, il fait
  tourner ce même code Python (embarqué par pyo3), seul Mimi est en Rust. Il expose tous les
  réglages de la voix choisie et prend le texte mot par mot, mais il faut le compiler avec CUDA
  et lui fournir en plus l'environnement Python. Pour une phrase à la fois, notre serveur donne
  le même flux avec une seule dépendance. On lui reprend une chose : un générateur construit une
  fois et remis à zéro avant chaque phrase (le reconstruire coûtait environ 0,5 s par phrase).
- **La voix et ses réglages sont dans la configuration de `voice`** (`voice`, `[kyutai]` :
  `padding_bonus`, `max_padding`, `temp`, `cfg_coef`), envoyés avec chaque phrase ; par défaut,
  ceux choisis par Kris. Ce qui change le modèle chargé (`n_q = 32`, la carte) est dans
  l'environnement du moteur.
- **Pour le serveur**, une image Docker est prête (`engines/kyutai/Dockerfile`, sur une image
  Python, la roue CUDA de torch apporte ses bibliothèques) ; elle demande le runtime NVIDIA de
  Docker, absent pour l'instant. Sur le PC, le moteur tourne sans Docker
  (`engines/kyutai/run-native.sh`).

**Mesuré sur le PC** (RTX 3070 Ti, sans la dictée de Kris, avec Parakeet d'un autre travail sur
la même carte, processeur chargé par d'autres travaux), de `voice.say` au premier morceau sur
`mouth.<nom>.audio`, première phrase d'un tour, 14 mesures par moteur, par le même chemin :

| | Premier son (médiane, min à max) | Facteur temps réel | Mémoire |
|---|---|---|---|
| Kyutai 1.6B, carte graphique | 0,29 s (0,25 à 0,47) | 0,31 à 0,36 | 4,5 Go de mémoire vidéo |
| Pocket estelle, processeur | 0,16 s (0,10 à 0,35) | 0,26 à 0,59 | 1 Go |

Le premier son de Kyutai est fixé par le modèle : le son a 1,28 s de retard sur le texte, soit
16 pas de calcul sans son avant le premier, puis 4 pas ; 20 pas à environ 12,5 ms. Une carte plus
rapide le raccourcit, rien d'autre. Les phrases suivantes ne s'attendent pas : elles sont
calculées trois fois plus vite qu'elles ne se disent. `voice.stop` arrête le calcul en cours
en environ 30 ms (mesuré une fois).

**Le texte au fil de l'eau** : le modèle sait lire le texte pendant que l'esprit l'écrit (le
serveur de Kyutai le fait). Le premier son partirait alors environ 0,25 s après les premiers
mots, au lieu de 0,25 s après la première phrase entière : on gagnerait le temps que met le LLM
à écrire sa première phrase. Le contrat (`Say`, phrase par phrase) n'a pas changé.

## Le lexique de prononciation (2026-10-13)

Entendu en direct le 2026-10-13 : la voix française (Pocket, estelle) lit « GitHub » « jithub » et
« SSH » comme un mot. Le modèle écrit souvent des mots techniques, et la mémoire de Kris en est
pleine : le lexique prévu depuis le 2026-10-11 est construit.

**Une étape de texte avant la synthèse**, dans `voice/src/text.rs`, là où le texte est nettoyé
(`text::clean`, appelée par `device.rs` pour chaque phrase). Elle s'applique à tous les moteurs,
après le nettoyage des espaces, et fait deux choses, dans cet ordre :

1. **Le lexique** : des mots (ou n'importe quel texte) remplacés par leur écriture à lire,
   `GitHub` devient « guite-heub ». Mot entier seulement (« Git » ne touche pas « GitHub »), le plus
   long d'abord, les majuscules respectées sauf si l'entrée demande de les ignorer, et la
   ponctuation autour du mot reste. Une entrée l'emporte sur la règle 2. Ce qu'une règle écrit
   n'est jamais relu par l'autre.
2. **Les sigles épelés** : un mot de 2 à 6 caractères, tous des majuscules A à Z ou des chiffres,
   avec au moins une lettre, qu'aucune entrée ne couvre et qui n'est pas dans la liste des sigles
   dits comme des mots (`not_spelled`), est épelé avec le nom des lettres et des chiffres, reliés
   par des traits d'union : « SSH » devient « èsse-èsse-ache », « MP3 » « èm-pé-trois », « 4G »
   « quatre-jé ». Un « s » minuscule final (« PCs », « APIs ») tombe : le pluriel d'un sigle ne se
   prononce pas. **Ce qui n'est pas épelé** : une seule lettre (« A », « I »), plus de 6 (« WYSIWYG »),
   un mot qui mêle des minuscules (« iPhone », « Ssh »), un nombre seul (« 2024 »), un mot qui
   commence par deux chiffres ou plus (« 10H », « 20KG » : une quantité), un mot qui a une lettre
   accentuée (« ÉTÉ »), un mot qui contient un `_` (« MAX_SIZE »).

**Les écritures des lettres** (celles de `LETTERS` dans `text.rs`) sont les noms français usuels
(« bé », « cé », « ache », « ji », « ka », « èm », « èsse », « double vé », « i-grec »,
« zède »…), avec deux choix faits à la mesure :

- **G se lit « jé »**, pas « gé » (qui se lirait « gué »).
- **Les lettres sont reliées par des traits d'union**, pas par des espaces ni des virgules. Mesuré
  avec Pocket en refaisant transcrire la synthèse par Parakeet (douze sigles, trois séparateurs) :
  avec des traits d'union, neuf sont reconnus comme le sigle (SSH, API, USB, DNS, URL, QR, CPU,
  TCP…) et la durée est celle de quelqu'un qui épelle ; avec des espaces, deux lettres égales se
  fondent (« SSH » devient « SH ») et les noms sont entendus comme des mots (« a pé i » : « à
  payer ») ; avec des virgules, chaque lettre a sa pause et un sigle de trois lettres dure presque le
  double. Pour la même raison, Y est « i-grec » (« ix-i grec-zède » : « YZ », « ix-i-grec-zède » :
  « XYZ »). W reste « double vé » avec une espace.
- Un moteur ne lit pas comme un autre : la table se change par lettre dans le fichier
  (`[letters]`), sans toucher au code.

**Le fichier** est du TOML (`voice/data/lexicon.fr.toml`, le lexique livré, en explique le format
en français en tête). Une table `[replace]` pour les mots remplacés en respectant les majuscules,
`[replace_any_case]` pour ceux qui les ignorent, `not_spelled` pour les sigles dits comme des
mots, `[letters]` pour le nom d'une lettre ; des commentaires partout. **Pourquoi du TOML et pas
le TSV de la table de corrections de `hearing`** : ce fichier a quatre sortes de contenu (deux
tables de mots, une liste, une table de lettres), la configuration de `voice` est déjà du TOML, et
`mind` garde ses phrases d'attente dans un TOML livré de la même façon. Le TSV de `hearing` est une
table de règles à expression régulière qu'on lit de haut en bas, ce qui n'est pas le besoin ici :
ici la clé est un mot, le plus long gagne, l'ordre ne compte pas.

**La configuration** (à la racine du fichier de `voice`, pas dans un tableau : `voice` y est une
chaîne) :

- `lexicon_enabled` (`true` par défaut) : `false` éteint toute l'étape, la phrase part comme
  l'esprit l'a écrite.
- `lexicon` : le chemin d'un fichier de lexique. **Un fichier donné remplace le lexique livré, il ne
  s'y ajoute pas** : on le copie et on le complète. Sans cette clé, le lexique livré est dans le
  binaire. `~/` désigne le dossier personnel, et un chemin relatif part du dossier du fichier de
  configuration, comme dans `hearing` (`voice` n'avait aucune clé de chemin relatif avant celle-ci).
- Le lexique est lu une fois au démarrage : un fichier illisible, une entrée mal formée (une
  valeur qui n'est pas du texte, une écriture vide, une clé de `[letters]` qui n'est pas une
  majuscule ou un chiffre, un mot de `not_spelled` qui en contient deux) arrêtent le démarrage avec
  un message qui nomme le fichier, la table et l'entrée. Pour recharger, on redémarre `voice`.

**Par défaut, l'étape est allumée, avec le lexique livré.** Elle ne change que des mots que le
moteur lit de travers et des sigles de majuscules qu'aucun moteur français ne lit bien, et le
lexique livré est court et mesuré (voir plus bas). Le risque est un mot français écrit en
majuscules (« NON », « OUI », « STOP ») : les plus courants sont dans `not_spelled`, et l'éteindre
tient en une ligne.

**Le lexique livré est générique** : il corrige les mots que Pocket lisait de travers à la mesure
(GitHub, GitLab, Git, Linux, Android, Redis, PostgreSQL, Neo4j, Rust, JSON, Gmail, STOP, OVNI) et
met en `not_spelled` des sigles qui se disent comme des mots (NASA, OVNI, UNESCO, OTAN, ONU, RAM,
LED, LASER, RADAR, SIDA, SMIC, OUI, NON, OK, YAML). **Docker, Bluetooth, YouTube, Google, iPhone,
Wi-Fi n'y sont pas** : Pocket les lit déjà bien (mesure ci-dessous), y toucher risquait de les
abîmer. Les mots de Kris (les noms de ses outils) sont dans `dev/lexicon.fr.toml`, qui recopie le
lexique livré et y ajoute les siens (`dev/voice.toml` et `dev/voice-kyutai.toml` le lisent).

**Mesuré le 2026-10-13, avec Pocket (estelle), sans rien jouer** : chaque forme candidate est
synthétisée puis transcrite par Parakeet de `hearing` (processeur, 2 fils), dans une phrase
« J'utilise X tous les jours. ». « GitHub » est transcrit « JTUP » tel quel et « GitHub » avec
« guite-heub » ; « Linux » « Linus » et « Linux » avec « linuxe » ; « JSON » « Y el son » et
« Jason » avec « djé-son » ; « Android » « en droit » et « Android » avec « androïde ». Sur dix
phrases passées par le vrai `voice` sur un bus à part, avant et après, GitHub, JSON, Neo4j, « 4G »
et OVNI passent de faux à reconnus (échantillons dans `.agent-relay/scratch/lexique/`). **Les
limites de la mesure** : un transcripteur est indulgent (SSH est transcrit « SSH » avec ou sans
l'épellation) et corrige d'après le sens (« reuste » est entendu « russe »), donc il écarte les
formes franchement fausses et ne départage pas les bonnes. **Jamais écouté** : les écritures
choisies sont à juger à l'oreille.

**Limites connues**

- Une élision devant un sigle épelé (« l'API » devient « l'a-pé-i ») est le point faible mesuré :
  la transcription en est mauvaise (« La pays »). À écouter.
- Les chiffres romains (« XIV », « CD ») sont épelés comme des sigles ; une entrée les corrige
  (`XIV = "quatorze"`).
- Un mot français écrit tout en majuscules par emphase est épelé, sauf ceux de `not_spelled`.
- Le lexique livré est réglé sur Pocket. **Kyutai TTS 1.6B (`en_fr`) n'a pas été essayé** (la carte
  graphique est prise) : il lit peut-être déjà bien les mots anglais, et un lexique fait pour
  Pocket pourrait alors les abîmer. À mesurer quand il tournera ; `lexicon_enabled = false`, ou un
  fichier à lui, règle le cas.
- Le lexique ne vient pas encore d'Hippocampe : un mot s'y ajoute à la main. Le tirer des titres et
  des alias, comme le vocabulaire de `hearing`, reste possible (le format est fait pour être écrit
  par un programme).

## Ce qui est lu

- **Les mots anglais, les noms propres et les sigles** : un moteur français lit « Neo4j » ou
  « MCP » à la française. **Construit le 2026-10-13** : un lexique de prononciation les remplace
  avant la synthèse (« Neo4j » → « néo-fore-djé »), et les sigles de majuscules sont épelés
  (« MCP » → « èm-cé-pé ») ; voir « Le lexique de prononciation ». Le tirer d'Hippocampe, comme le
  vocabulaire de `hearing`, reste à faire.
- **Les nombres, les dates et les heures** sont écrits en toutes lettres avant la synthèse ; le
  plus simple est de le demander à l'esprit.

## Questions ouvertes

1. Une meilleure voix qu'estelle : Kyutai 1.6B, choisi le 2026-10-12 et construit (voir « Le
   moteur Kyutai »), attend la carte graphique du serveur.
2. La lecture des nombres, des sigles et des noms propres par Pocket : le lexique et l'épellation
   des sigles sont construits (2026-10-13, voir « Le lexique de prononciation »), mais **jamais
   écoutés** : les écritures choisies sont à juger à l'oreille, l'élision devant un sigle épelé en
   premier. Restent la lecture des nombres, des dates et des heures, et le comportement de
   Kyutai avec ce lexique.
3. Le format du flux entre l'esprit et la bouche (texte phrase par phrase, annulation des phrases
   pas encore dites) : sujets `voice.say` et `voice.stop` du bus, format fixé dans `contract/` (`Say`, avec `last` sur la dernière phrase ; voir [`contract.md`](contract.md)).
