# Les oreilles d'Emory : version 1

Statut : brouillon du 2026-10-08, en relecture avec Kris le 2026-10-10.

Emory est l'assistant de Kris. Hippocampe est sa mémoire à long terme. Les
oreilles sont le premier organe : elles transforment ce qui est dit autour de Kris en événements
que l'esprit d'Emory reçoit. L'esprit décide ensuite : chercher dans Hippocampe, y écrire, ou
répondre.

L'esprit n'est pas encore choisi. Les oreilles n'en dépendent pas : elles produisent un événement
au format fixé plus bas, et n'importe quel esprit le lit.

## Principes

1. **L'audio ne quitte pas le lieu où il est capté.** Il est traité sur les machines de Kris,
   dans le même logement. Seul du texte en sort, vers l'esprit. En version 1, l'audio passe de
   l'oreille au centre en clair sur le réseau local.
2. **Rien n'est conservé par défaut.** Ce qui n'est adressé à personne est oublié. Les deux
   exceptions sont la mémoire tampon et les conversations en attente d'identification, toutes
   deux bornées dans le temps (voir plus bas).
3. **Les oreilles sont indépendantes de l'esprit.** On peut changer l'un sans toucher à l'autre.
4. **Ce qui est déduit est écrit comme supposé.** Ce que Kris affirme est pris tel quel, connu,
   avec Kris comme source. Ce que le système déduit seul (« cette voix, c'est sans doute
   Marie ») est écrit, mais marqué supposé (provenance `inferred` d'Hippocampe). Rien n'attend
   une validation : Kris confirme quand il veut, et une supposition confirmée devient connue.
5. **Une voix n'est jamais un mot de passe.** Emory reconnaît Kris, mais une action sensible
   demande toujours autre chose que sa voix.
6. **Emory ne gêne pas le reste du serveur.** Le serveur fait aussi du multimédia et d'autres
   services. Sur le processeur, les oreilles tournent avec des limites strictes : quelques cœurs
   réservés (par exemple les cœurs E de l'i5-14400), une priorité basse, et un plafond de mémoire
   par cgroup. Le banc d'essai mesure l'effet sur les autres services. Le processeur n'est
   qu'un début : la charge passe sur une carte graphique dès que possible.

## Matériel

| Rôle | Machine | Ce qu'elle fait |
|---|---|---|
| **L'oreille** | le poste (voir le [README](README.md)) : d'abord le PC de Kris avec son casque ; plus tard le Raspberry Pi 4 inutilisé ou une autre machine, avec un micro de pièce | Capte le son, rien d'autre, avec un adaptateur par type de micro. Envoie l'audio au serveur, sur le réseau local, avec le nom du poste. |
| **Le cerveau auditif** | le serveur (i5-14400, 32 Go, pas de carte graphique aujourd'hui) | Un seul centre pour toutes les oreilles : il reçoit leurs flux, transcrit et envoie les événements à l'esprit ; pour les oreilles de pièce, il fait en plus le mot de réveil, la fin de tour, la reconnaissance des voix, la mémoire tampon et le choix entre deux oreilles qui entendent la même phrase. |

L'oreille et le serveur sont dans le même logement, et suivent Kris s'il déménage. Le serveur est
relié en Wi-Fi : un flux de voix fait environ 256 kbit/s, ce qui ne pose aucun problème.

**N'importe quel micro.** Une oreille est une source audio, pas un appareil : le micro du casque
de Kris sur son PC, un micro USB sur le Raspberry Pi, une matrice ou un micro de conférence dans
une pièce. Chaque type de micro n'est qu'un adaptateur de capture. Chaque oreille a un nom.

**Deux sortes d'oreilles** (décidé le 2026-10-10) :

| | Oreille à bouton | Oreille de pièce |
|---|---|---|
| Exemple | le casque de Kris sur son PC | un micro posé dans une pièce |
| Quand elle écoute | entre deux appuis sur un bouton | toujours |
| Qui parle | Kris, forcément | à reconnaître |
| Réveil | le bouton | le mot « Emory » |
| Fin du tour | le second appui | un modèle de fin de tour |
| Mémoire tampon | non : rien n'est écouté hors des appuis | oui |
| Version | **1** | plus tard |

**La version 1 est l'oreille à bouton du casque de Kris**, avant tout achat. Le micro de pièce
se choisit ensuite, quand la chaîne a fait ses preuves avec le casque.

## La chaîne d'écoute

**Des oreilles bêtes, un seul centre** (décidé le 2026-10-10). Une oreille capte ; tout le reste
se fait au centre, pour toutes les oreilles à la fois. Au centre, il n'y a qu'une carte graphique
et qu'un jeu de modèles, les empreintes des voix sont au même endroit pour toutes les pièces, et
deux oreilles qui entendent la même phrase sont départagées. Ajouter un micro ne demande qu'un
adaptateur de capture. Une oreille hors du logement (un téléphone en déplacement) demandera un
transport chiffré : hors version 1, le centre ne change pas.

### L'oreille à bouton (version 1)

```
oreille : appui → micro → second appui   │   centre : transcription → événement → esprit
```

1. **Le bouton.** Un appui ouvre l'écoute, un second la ferme. L'audio entre les deux part au
   centre. Pas de détection de voix, de mot de réveil ni de fin de tour : c'est Kris qui les fait.
2. **Transcription**, au centre. Le modèle sera choisi par le banc d'essai plus bas. Les modèles
   écorchent les noms propres : on leur donne une liste de mots à privilégier (prompt initial ou
   hotwords), tirée des titres et des alias d'Hippocampe (Emory, Hippocampe, les projets et les
   proches de Kris, Neo4j…). Les silences sont coupés avant la transcription (Silero VAD, intégré
   à faster-whisper comme à sherpa-onnx), ce qui évite les phrases inventées.
3. **Qui parle** : Kris, puisque c'est son casque. L'événement porte `locuteur` à `kris`, d'origine
   `connue`, et `reveil` à `bouton`.

### L'oreille de pièce (plus tard)

```
  oreille (×n)          │  centre auditif
micro → détection de voix → mot de réveil → transcription → fin de tour → qui parle → événement → esprit
                        │                ↘ mémoire tampon (toujours, en continu)
```

1. **Détection de voix**, sur l'oreille, avec Silero VAD (un modèle de 2 Mo qui dit si un morceau
   d'audio contient de la voix). Le silence, la musique et le bruit ne vont pas plus loin. C'est
   aussi ce qui empêche les hallucinations de Whisper sur le silence (« Sous-titres réalisés par
   la communauté d'Amara.org »).
2. **Mot de réveil « Emory »**, au centre, avec openWakeWord entraîné sur ce prénom. Il est
   **obligatoire** : comprendre sans lui qu'une phrase est adressée à Emory reviendra à l'esprit.
   Après une réponse d'Emory, une **fenêtre de suite** d'environ 10 secondes permet de continuer
   sans le répéter. La mémoire tampon transcrit de toute façon toute la parole : un mot de réveil
   sur l'oreille n'économiserait rien.
3. **Transcription**, comme pour l'oreille à bouton.
4. **Fin de tour**. Un modèle de fin de tour (comme Smart Turn de Pipecat) écoute l'intonation et
   lit la phrase. Une pause au milieu d'une phrase ne la coupe pas.
5. **Interruption.** Si Kris reprend la parole pendant qu'Emory parle, Emory se tait et écoute.
   Ça concerne aussi la bouche, et sera repris avec elle.
6. **Qui parle**. Voir la section « Les voix ».
7. **Plusieurs oreilles.** Quand deux oreilles entendent la même phrase, on garde celle qui
   l'entend le plus fort.

## La mémoire tampon

Elle ne concerne que les oreilles de pièce.

Les **10 dernières minutes** de ce qui a été dit sont transcrites en continu, avec qui parlait,
puis effacées au fil de l'eau. Elles restent sur le serveur et ne sont envoyées à l'esprit que
quand il est réveillé et qu'il en a besoin.

Elles permettent :
- « Emory, note ce que je viens de dire », même pour une phrase dite avant le réveil ;
- « Emory, tu as entendu ce qu'il a dit ? ».

## Les voix

Elles ne concernent que les oreilles de pièce : l'oreille à bouton n'entend que Kris.

**La voix de Kris est enregistrée volontairement.** Une minute de parole, dans plusieurs
situations (près ou loin du micro, calme ou fatigué). C'est la seule voix enregistrée exprès.
Pour suivre ses changements lents, les phrases reconnues avec une très forte confiance sont
ajoutées à sa référence.

**Les autres voix s'apprennent seules.**
- Chaque voix inconnue reçoit une étiquette anonyme et stable : « voix 7 ».
- Son empreinte reste dans le service des oreilles. Hippocampe ne stocke jamais d'empreinte.

**Les conversations en attente.**
- Ce qu'une voix inconnue a dit est gardé en texte sur le serveur, avec son étiquette, pendant
  une durée bornée, **réglable**. Elle commence haute (**7 jours**) et sera réglée après un essai
  en conditions réelles (décidé le 2026-10-10).
- Pendant ce temps, l'esprit peut demander à Kris, au bon moment : « quelqu'un était là cet
  après-midi, tu sais qui c'était ? ».

**Quand la voix reçoit un nom :**
- **Kris le dit** (« c'était Marie Martin ») : la voix est associée à cette personne
  d'Hippocampe. L'esprit crée la personne si elle n'existe pas, puis écrit après coup ce qui
  mérite d'être gardé de la conversation, rattaché à elle.
- **Le système le déduit seul** (quelqu'un a dit « salut Marie » juste avant) : l'association
  est proposée, pas appliquée. Ce que l'esprit écrit à partir d'elle est marqué supposé ; Kris
  le confirme quand il veut.
- **Personne ne sait** : au bout du délai, l'étiquette et ce qu'elle a dit sont effacés.

**Les voix non humaines.** La télé, la radio et les vidéos sont des voix aussi. Elles sont
reconnues comme sources de média : elles ne sont jamais associées à une personne, ne réveillent
jamais Emory et ne sont jamais notées.

**Oublier une voix.** « Emory, oublie la voix de Marie » efface l'empreinte. La personne et ce
qu'on sait d'elle restent dans Hippocampe.

Kris a décidé d'avancer sans avis de juriste (2026-10-10). Pour mémoire : le Code pénal (article 226-1) punit le fait
d'enregistrer sans leur accord les paroles privées d'autrui. Le RGPD, lui, exempte en principe
l'usage purement domestique. Prévenir les personnes qui viennent souvent lève l'ambiguïté.

## L'événement envoyé à l'esprit

Un événement par tour de parole adressé à Emory :

```json
{
  "id": "evt_…",
  "oreille": "bureau",
  "debut": "2026-10-08T21:14:03.120+02:00",
  "fin": "2026-10-08T21:14:06.870+02:00",
  "texte": "Emory, rappelle-moi ce qu'on avait décidé pour la musique",
  "locuteur": { "voix": "kris", "personne": "kris", "origine": "connue", "confiance": 0.94 },
  "confiance_transcription": 0.91,
  "reveil": "mot",
  "contexte": [
    { "debut": "…", "voix": "voix-7", "personne": null, "texte": "…" }
  ]
}
```

- `reveil` vaut `bouton` (oreille à bouton), `mot` (le mot de réveil) ou `suite` (dans la
  fenêtre de suite).
- `contexte` est vide par défaut. Il porte les phrases de la mémoire tampon quand l'esprit les
  demande.
- `voix` est l'étiquette stable de l'empreinte (`kris`, `voix-7`) ; `personne` est le slug de la
  personne dans Hippocampe quand la voix a un nom, `null` sinon. Les séparer permet de rattacher
  après coup à une personne tout ce qu'une voix a dit.
- `origine` vaut `connue` (Kris a donné le nom) ou `supposee` (le système l'a déduit) : ce que
  l'esprit écrit à partir d'une voix supposée est marqué `inferred` dans Hippocampe.

L'événement est publié sur le bus NATS, sujet `oreilles.tour` (voir le [README](README.md)) ; son
format exact est dans `contrat/`.

## La latence

Le but : qu'Emory commence à répondre **moins de 2 secondes** après la fin de la phrase.

| Étape | Ordre de grandeur visé |
|---|---|
| Fin de tour | 0,1 à 0,3 s |
| Transcription de la dernière phrase | 0,2 à 0,5 s |
| Premier mot de l'esprit | 0,5 à 1 s |
| Premier son de la bouche | 0,2 s |

Ce qui rend ça tenable, à reprendre avec l'esprit :
- **Tout en flux.** On transcrit pendant que Kris parle, et la bouche parle pendant que l'esprit
  écrit.
- **Pas de phrase d'attente** (Kris n'aime pas les assistants qui disent « je regarde » et font
  attendre). Emory répond tout de suite avec ce qu'il sait ; le travail long (fouiller
  Hippocampe, relire la mémoire tampon) se fait en parallèle, et quand il aboutit, Emory jette
  ce qu'il n'a pas encore dit et enchaîne au bord d'une phrase (« … ah, je l'ai : »). Ce qu'il
  dit en attendant doit être utile, pas du remplissage. À concevoir avec l'esprit et la bouche.
- **La réflexion sur ce qui s'est passé n'a jamais lieu pendant la réponse.** Ranger, relier,
  identifier les voix : tout ça se fait plus tard, en tâche de fond, comme la consolidation de
  l'hippocampe pendant le sommeil.

## Le banc d'essai de la transcription

Il doit être fait **avant** de choisir un modèle, sur le serveur.

**Les enregistrements**, faits au casque de Kris, comme l'oreille à bouton :
- 2 minutes de Kris, en français, avec ses mots : Emory, Hippocampe, Grenier, les noms de ses
  projets, des mots techniques anglais (Neo4j, MCP, Rust) ;
- 30 secondes de silence et de bruit de fond (un appui sans parler), pour voir si le modèle
  invente des phrases.

La conversation à deux voix et le mot de réveil se mesureront avec l'oreille de pièce : combien
de fois il rate un « Emory » de Kris, combien de fois il se déclenche seul (pendant une vidéo).

**Les modèles :**
- Parakeet TDT 0.6B v3 (NVIDIA, multilingue, rapide sur processeur) ;
- faster-whisper `small` et `large-v3-turbo` en int8 ;
- whisper.cpp ;
- Voxtral Mini, si sa taille le permet.

Chaque modèle est un moteur, branché à `oreilles` (en Rust) dans le binaire ou hors processus (voir
« Les moteurs » dans le [README](README.md)) : son langage n'est pas un critère. Le banc note
quand même comment chacun se branche, puisqu'à qualité égale un moteur dans le binaire l'emporte.

**Sur quoi :**
- le processeur i5-14400 ;
- la puce graphique intégrée UHD 730, avec OpenVINO, en tenant compte du transcodage vidéo qui
  l'utilise déjà ;

**Ce qu'on mesure :**
- la vitesse, rapportée à la durée de l'audio (il faut être nettement plus rapide que le temps
  réel) ;
- les erreurs sur les phrases et sur les noms propres, avec et sans vocabulaire donné ;
- les phrases inventées sur le silence ;
- la charge du processeur pendant la transcription, à côté des autres services du serveur ;
- l'effet sur eux : un transcodage vidéo et une lecture multimédia lancés pendant que les
  oreilles transcrivent ne doivent ni ralentir ni saccader.

Le processeur sert à démarrer, pas à durer : la charge passe sur une carte graphique dès que
possible, pour ne pas gêner le reste du serveur. Le banc dit ce qui est tenable en attendant et
quelle carte suffit. Elle doit entrer dans le Jonsbo N6 et être alimentée par la Corsair CX550.

Recherche du 2026-10-08 (prix et cotes à revérifier avant d'acheter) :

- **Les contraintes.**
  - Le Jonsbo N6 accepte une carte de 275 mm au plus avec ses ventilateurs avant. Son épaisseur
    maximale n'est pas confirmée.
  - La CX550 a deux connecteurs PCIe 6+2 broches, ce qui suffit pour une carte de 170 W.
  - Une carte de 2 slots peut recouvrir le slot M.2 situé au-dessus du slot x16. Il faut vérifier
    où est branché l'adaptateur SATA ASM1166.
- **Les prix neufs ont grimpé en 2026**, à cause de la pénurie de mémoire. Presque rien de
  récent ne passe sous 300 €.
- **Recommandé : une RTX 3060 12 Go d'occasion**, autour de 220 à 300 €.
  - Elle a CUDA, la référence pour faster-whisper, pyannote, Parakeet et llama.cpp.
  - Ses 12 Go permettent de garder la transcription, la voix et un petit modèle de 7 à 8 milliards
    de paramètres chargés ensemble.
  - Elle fait environ 240 mm, sur 2 slots, et consomme 170 W au plus et une dizaine de watts au repos.
- **Moins cher, neuf : une RTX 3050 6 Go**, environ 230 €. Elle suffit pour la transcription et la
  voix, mais pas pour un modèle de langage local.
- **À éviter.**
  - Intel Arc : pas de CUDA, donc pas de faster-whisper accéléré, et une consommation au repos
    élevée.
  - La GTX 950 : il faudrait recompiler CTranslate2, et ses 2 Go sont trop justes.

## Hors version 1

- L'oreille de pièce : détection de voix, mot de réveil, fin de tour, mémoire tampon, voix
  (conçue, pas réalisée).
- Plusieurs oreilles (conçu, pas réalisé).
- Comprendre qu'une phrase est adressée à Emory sans le mot de réveil : c'est l'esprit qui le
  fera.
- La bouche, sauf l'interruption, qui la concerne aussi.
- Les yeux et les mains.

## Questions ouvertes

1. Le micro de pièce à acheter, après l'essai avec le casque.
2. Le modèle de transcription : choisi par le banc d'essai.
