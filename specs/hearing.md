# Les oreilles d'Emory : version 1

Statut : brouillon du 2026-10-08, en relecture avec Kris le 2026-10-10. Noms passés en anglais le 2026-10-11.

Emory est l'assistant de Kris. Hippocampe est sa mémoire à long terme. Les oreilles sont le
premier organe : elles transforment ce qui est dit autour de Kris en événements que `mind` (l'esprit
d'Emory) reçoit. `mind` décide ensuite : chercher dans Hippocampe, y écrire, ou répondre.

Les oreilles ont deux parties (voir le [README](README.md)) : `emory-ear`, sur l'appareil, capte le
son ; `hearing`, sur le serveur, le transcrit.

L'esprit n'est pas encore choisi. Les oreilles n'en dépendent pas : elles produisent un événement
au format fixé plus bas, et n'importe quel esprit le lit.

## Principes

1. **L'audio ne quitte pas le lieu où il est capté.** Il est traité sur les machines de Kris,
   dans le même logement. Seul du texte en sort, vers `mind`. En version 1, l'audio passe de
   `emory-ear` à `hearing` en clair sur le réseau local.
2. **Rien n'est conservé par défaut.** Ce qui n'est adressé à personne est oublié. Les deux
   exceptions sont la mémoire tampon et les conversations en attente d'identification, toutes
   deux bornées dans le temps (voir plus bas).
3. **Les oreilles sont indépendantes de `mind`.** On peut changer l'un sans toucher à l'autre.
4. **Ce qui est déduit est écrit comme supposé.** Ce que Kris affirme est pris tel quel, connu,
   avec Kris comme source. Ce que le système déduit seul (« cette voix, c'est sans doute
   Marie ») est écrit, mais marqué supposé (provenance `inferred` d'Hippocampe). Rien n'attend
   une validation : Kris confirme quand il veut, et une supposition confirmée devient connue.
5. **Une voix n'est jamais un mot de passe.** Emory reconnaît Kris, mais une action sensible
   demande toujours autre chose que sa voix.
6. **Emory ne gêne pas le reste du serveur.** Le serveur fait aussi du multimédia et d'autres
   services. **Décidé le 2026-10-13 : sur le serveur, tout le calcul d'IA tourne sur la carte
   graphique** (voir « Tout le calcul sur la carte du serveur ») ; le processeur est pour les
   films, la musique et le reste, et ne sert à Emory qu'en repli. Dans ce repli, `hearing` garde
   des limites strictes : quelques cœurs réservés (par exemple les cœurs E de l'i5-14400), une
   priorité basse, et un plafond de mémoire par cgroup. Le banc d'essai du 2026-10-11 a mesuré
   l'effet sur les autres services.

## Matériel

| Rôle | Machine | Ce qu'elle fait |
|---|---|---|
| **L'oreille** (`emory-ear`) | l'appareil (voir le [README](README.md)) : d'abord le PC de Kris avec son casque ; plus tard le Raspberry Pi 4 inutilisé ou une autre machine, avec un micro de pièce | Capte le son, rien d'autre, avec un module par déclencheur (`shortcut`, `button`, plus tard `continuous`). Envoie l'audio au serveur, sur le réseau local, avec le nom de l'appareil. |
| **Le cerveau auditif** | le serveur (i5-14400, 32 Go, et une carte graphique NVIDIA de 12 Go à acheter, décidée le 2026-10-13 ; pas de carte graphique aujourd'hui) | Un seul centre pour toutes les oreilles : il reçoit leurs flux, transcrit et envoie les événements à l'esprit ; pour les oreilles de pièce, il fait en plus le mot de réveil, la fin de tour, la reconnaissance des voix, la mémoire tampon et le choix entre deux oreilles qui entendent la même phrase. |

L'oreille et le serveur sont dans le même logement, et suivent Kris s'il déménage. Le serveur est
relié en Wi-Fi : un flux de voix fait environ 256 kbit/s, ce qui ne pose aucun problème.

**N'importe quel micro.** Une oreille est une source audio, pas un appareil : le micro du casque
de Kris sur son PC, un micro USB sur le Raspberry Pi, une matrice ou un micro de conférence dans
une pièce. Chaque type de micro n'est qu'un adaptateur de capture. Chaque oreille (un `emory-ear`
par microphone) a un nom.

**Deux sortes d'oreilles** (décidé le 2026-10-10) :

| | Oreille à bouton | Oreille de pièce |
|---|---|---|
| Exemple | le casque de Kris sur son PC | un micro posé dans une pièce |
| Quand elle écoute | entre deux appuis sur un bouton | toujours (module `continuous`) |
| Qui parle | Kris, forcément | à reconnaître |
| Réveil | le déclencheur (`shortcut` ou `button`) | le mot « Emory » |
| Fin du tour | le second appui | un modèle de fin de tour |
| Mémoire tampon | non : rien n'est écouté hors des appuis | oui |
| Version | **1** | plus tard |

**La version 1 est l'oreille à bouton du casque de Kris**, avant tout achat. Le micro de pièce
se choisit ensuite, quand la chaîne a fait ses preuves avec le casque.

## La chaîne d'écoute

**Des oreilles bêtes, un seul centre** (décidé le 2026-10-10). Une oreille capte ; tout le reste
se fait au centre (`hearing`), pour toutes les oreilles à la fois. Au centre, il n'y a qu'une carte graphique
et qu'un jeu de modèles, les empreintes des voix sont au même endroit pour toutes les pièces, et
deux oreilles qui entendent la même phrase sont départagées. Ajouter un micro ne demande qu'un
adaptateur de capture. Une oreille hors du logement (un téléphone en déplacement) demandera un
transport chiffré : hors version 1, le centre ne change pas.

### L'oreille à bouton (version 1)

```
emory-ear : appui → micro → second appui   │   hearing : transcription → événement → mind
```

1. **Le déclencheur** (un module de `emory-ear`). Sur le PC de Kris, `shortcut` utilise le standard
   GlobalShortcuts du xdg-desktop-portal, en appui court pour basculer ou en appui maintenu pour
   parler, comme sa touche de dictée ; sur un Raspberry, `button` est un vrai bouton. Un appui
   ouvre l'écoute, un second la ferme. L'audio entre les deux part au
   centre, en flux. Pas de mot de réveil ni de fin de tour : c'est Kris qui les fait. Un tour est
borné à 120 s (`max_turn`, réglable, décidé le 2026-10-11) : au-delà, `hearing` le ferme comme avec
`end`, avec un avertissement dans son journal, même si l'oreille continue d'envoyer du son.
2. **Découpage en phrases**, au centre, pendant que Kris parle (décidé le 2026-10-11). `emory-ear`
   n'envoie que l'audio brut ; c'est `hearing`, sur le serveur, qui y fait passer Silero VAD :
   une phrase s'ouvre quand Kris parle et se ferme après un court silence. Chaque phrase fermée
   est transcrite aussitôt et publiée sur `hearing.sentence`. Au second appui, il ne reste que la
   dernière à transcrire, puis le tour entier part sur `hearing.turn`. Le VAD coupe aussi les
   silences, ce qui évite les phrases inventées.
   - Sur le processeur, une phrase est transcrite une seule fois, quand elle se ferme. Le texte
     provisoire de la phrase en cours, retranscrite toutes les 0,4 s comme le fait un clavier de
     dictée, attendra la carte graphique : c'est trop coûteux sur le processeur.
   - **À ne pas faire** (essayé pour la dictée du PC de Kris, le 2026-10-11) : couper l'audio en
     morceaux de durée fixe (des mots doublés aux coupures), ou retranscrire en boucle les
     dernières secondes en fenêtre glissante (la qualité s'effondre, le texte est réécrit sans
     cesse).
3. **Transcription**, au centre, par Parakeet v3 (choisi au banc d'essai plus bas). Les modèles
   écorchent les noms propres, quel qu'il soit (« Hippocamp », « Chris », « au marché » pour
   Omarchy). Deux remèdes, tirés tous deux des titres et des alias d'Hippocampe (Emory,
   Hippocampe, les projets et les proches de Kris, Neo4j…) :
   - une liste de mots à privilégier, donnée au modèle (prompt initial ou hotwords) ;
   - **une table de corrections appliquée après la transcription** (« Hippocamp » →
     « Hippocampe », « Chris » → « Kris ») : la liste seule ne suffit pas. Une ligne dont le
     motif commence par `=` est une chaîne littérale (pour les entrées générées depuis les titres
     d'Hippocampe), les autres restent des expressions régulières ; texte et table sont comparés
     en NFC (2026-10-11).
4. **Qui parle** : Kris, puisque c'est son casque. L'événement porte `speaker` à `kris`, d'origine
   `known`, et `wake` à `button`.

### L'oreille de pièce (plus tard)

```
  emory-ear (×n)        │  hearing
micro → détection de voix → mot de réveil → transcription → fin de tour → qui parle → événement → mind
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
   Ça concerne aussi la bouche (voir [`voice.md`](voice.md)), et sera repris avec elle.
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
- Son empreinte reste dans `hearing`. Hippocampe ne stocke jamais d'empreinte.

**Les conversations en attente.**
- Ce qu'une voix inconnue a dit est gardé en texte sur le serveur, avec son étiquette, pendant
  une durée bornée, **réglable**. Elle commence haute (**7 jours**) et sera réglée après un essai
  en conditions réelles (décidé le 2026-10-10).
- Pendant ce temps, `mind` peut demander à Kris, au bon moment : « quelqu'un était là cet
  après-midi, tu sais qui c'était ? ».

**Quand la voix reçoit un nom :**
- **Kris le dit** (« c'était Marie Martin ») : la voix est associée à cette personne
  d'Hippocampe. `mind` crée la personne si elle n'existe pas, puis écrit après coup ce qui
  mérite d'être gardé de la conversation, rattaché à elle.
- **Le système le déduit seul** (quelqu'un a dit « salut Marie » juste avant) : l'association
  est proposée, pas appliquée. Ce que `mind` écrit à partir d'elle est marqué supposé ; Kris
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

## L'événement envoyé à `mind`

Un événement par tour de parole adressé à Emory :

```json
{
  "turn": "01K…",
  "device": "office",
  "start": "2026-10-08T21:14:03.120+02:00",
  "end": "2026-10-08T21:14:06.870+02:00",
  "text": "Emory, rappelle-moi ce qu'on avait décidé pour la musique",
  "speaker": { "voice": "kris", "person": "kris", "origin": "known", "confidence": 0.94 },
  "transcription_confidence": 0.91,
  "wake": "word",
  "context": [
    { "start": "…", "voice": "voix-7", "person": null, "text": "…" }
  ]
}
```

- `wake` vaut `button` (oreille à bouton), `word` (le mot de réveil) ou `follow_up` (dans la
  fenêtre de suite).
- `context` est vide par défaut. Il porte les phrases de la mémoire tampon quand `mind` les
  demande.
- `voice` est l'étiquette stable de l'empreinte (`kris`, `voix-7`) ; `person` est le slug de la
  personne dans Hippocampe quand la voix a un nom, `null` sinon. Les séparer permet de rattacher
  après coup à une personne tout ce qu'une voix a dit.
- `origin` vaut `known` (Kris a donné le nom) ou `inferred` (le système l'a déduit) : ce que
  `mind` écrit à partir d'une voix supposée est marqué `inferred` dans Hippocampe.

L'événement est publié sur le bus NATS, sujet `hearing.turn` (voir le [README](README.md)) ; son
format exact est dans `contract/` ([`contract.md`](contract.md)).

## La latence

Le but : qu'Emory commence à répondre **moins de 2 secondes** après la fin de la phrase.

| Étape | Ordre de grandeur visé |
|---|---|
| Fin de tour | 0,1 à 0,3 s |
| Transcription de la dernière phrase | 0,2 à 0,5 s |
| Premier mot de l'esprit | 0,5 à 1 s |
| Premier son de la bouche | 0,2 s |

Ce qui rend ça tenable, à reprendre avec `mind` :
- **Tout en flux.** On transcrit pendant que Kris parle, et la bouche parle pendant que `mind`
  écrit.
- **Pas de phrase d'attente** (Kris n'aime pas les assistants qui disent « je regarde » et font
  attendre). Emory répond tout de suite avec ce qu'il sait ; le travail long (fouiller
  Hippocampe, relire la mémoire tampon) se fait en parallèle, et quand il aboutit, Emory jette
  ce qu'il n'a pas encore dit et enchaîne au bord d'une phrase (« … ah, je l'ai : »). Ce qu'il
  dit en attendant doit être utile, pas du remplissage. À concevoir avec `mind` et la bouche.
- **La réflexion sur ce qui s'est passé n'a jamais lieu pendant la réponse.** Ranger, relier,
  identifier les voix : tout ça se fait plus tard, en tâche de fond, comme la consolidation de
  l'hippocampe pendant le sommeil.

## Le banc d'essai de la transcription

Fait le 2026-10-11 sur le serveur, sur les 4 cœurs E (`cpuset` 12-15, priorité basse), avec deux
jeux : 2 minutes de Kris au casque (parole spontanée, beaucoup de noms propres et d'anglais) et
FLEURS français (33 voix publiques lues, 17 hommes et 16 femmes). Rapport complet hors dépôt.

**Choisi : Parakeet TDT 0.6B v3 int8, découpé en phrases par Silero VAD, avec une table de
corrections, dans le binaire `hearing` par sherpa-onnx.**

| Moteur (cœurs E) | Erreurs sur Kris, sans aide / avec la table | FLEURS | Temps | Silence |
|---|---|---|---|---|
| **Parakeet v3 + VAD** | **15,3 / 7,7 %** | 6,0 % | **1,4 s par phrase** | rien inventé |
| faster-whisper `large-v3-turbo` | 12,7 / 4,3 % | 4,5 % | 10 s pour 9 s de parole | invente sans VAD |
| faster-whisper `small` | 18,0 / 10,3 % | 9,5 % | 2,6 s pour 9 s | invente avec mots-clés |

- **Le découpage en phrases est indispensable** : sur un fichier entier de 2 minutes, Parakeet
  saute des paragraphes entiers.
- **La table de corrections fait le travail, pas les mots-clés** : avec la table, Parakeet ne rate
  plus que 2 noms propres sur 28 ; les mots-clés ne l'aident pas (18 ratés avec ou sans). La table
  du banc était faite sur ce même enregistrement : celle tirée d'Hippocampe rattrapera moins. Elle
  ne doit corriger que des formes qui ne sont pas des mots français (« trafic », « Chris » se
  font réécrire sinon).
  (Ce constat vaut pour Whisper : avec Parakeet et sherpa-onnx, les mots-clés aident, voir
  « Le niveau du micro et le vocabulaire » plus bas, 2026-10-12.)
- **« Emory » est le nom le plus mal entendu** (« Memory », « et Maury », « et Boris ») : à
  retenir pour le mot de réveil.
- Whisper turbo transcrit un peu mieux mais est 7 fois trop lent sur les cœurs E ; il redevient
  un candidat avec une carte graphique.
- Erreurs communes à tous : « pattes » pour « pâtes », « trafique les expos » pour « Traefik les
  expose ».
- Non mesurés : whisper.cpp compilé nativement et Voxtral Mini (compilation refusée pendant le
  banc), l'iGPU UHD 730, l'effet sur un transcodage Jellyfin.

**La latence.** 1,4 s pour la dernière phrase sur les cœurs E, plus le premier mot du LLM
(0,7 s) et le premier son (0,2 s) : environ 2,3 s, au-delà du but de 2 s. Sur tous les cœurs, la
transcription tombe à 0,7 s. À essayer sur la vraie chaîne : laisser Parakeet déborder sur tous
les cœurs, en priorité basse, pendant les rafales de transcription.

**Une carte graphique pour le serveur** (le 2026-10-11, `hearing` et `voice` tenaient sur les
cœurs E sans gêner le reste du serveur, mais trop lentement ; Kris a décidé le 2026-10-12 puis le
2026-10-13 que le serveur reçoit une carte de 12 Go et que tout le calcul d'IA y passe). Elle sert
à Parakeet, à Kyutai TTS 1.6B, et peut-être plus tard à Whisper turbo ou à un LLM local.
Recherche du 2026-10-08 sur la carte (prix et cotes à revérifier avant d'acheter) :

- **Les contraintes.**
  - Le Jonsbo N6 accepte une carte de 275 mm au plus avec ses ventilateurs avant. Son épaisseur
    maximale n'est pas confirmée.
  - La CX550 a deux connecteurs PCIe 6+2 broches, ce qui suffit pour une carte de 170 W.
  - Une carte de 2 slots peut recouvrir le slot M.2 situé au-dessus du slot x16. Il faut vérifier
    où est branché l'adaptateur SATA ASM1166.
- **Recommandé : une RTX 3060 12 Go d'occasion**, autour de 220 à 300 €, CUDA, 12 Go pour la
  transcription, la voix et un petit modèle de langage ensemble.
- **À éviter** : Intel Arc (pas de CUDA, consommation au repos élevée), la GTX 950 (2 Go).

## Le niveau du micro et le vocabulaire (2026-10-12)

Essai en vrai de la chaîne par Kris avec son casque sans fil (Logitech PRO X 2) : 4 tours sur 9
sont sortis avec `sentences=0`, Emory n'a rien dit, et rien dans les journaux ne permettait de
dire pourquoi. Silero VAD n'avait vu aucune voix dans ces tours, alors que Parakeet seul
transcrit le même enregistrement atténué à un pic de -44 dBFS. Le plus probable : le casque a
envoyé du quasi-silence (bouton muet, liaison sans fil). Rien ne mesurait le niveau, donc rien ne
pouvait le prouver.

**Le niveau, en `debug`** (Kris a demandé `debug`, pas `info`) :
- `hearing` mesure, par tour, le pic et le niveau efficace (RMS) de l'audio reçu en dBFS (0 est le
  plus fort possible, plancher à -120), la durée de parole vue par le VAD et le nombre de phrases.
  Ligne `turn level`, avec le tour. Elle ne contient jamais le texte.
- Un tour de plus de 0,5 s qui finit sans phrase ajoute une ligne `no sentence in the turn` : soit
  `no voice found` (le VAD n'a rien vu : les niveaux disent si le micro envoyait quelque chose),
  soit `voice found, but nothing came out of it` (le VAD a vu de la parole mais Parakeet n'a rendu
  aucun texte).
- `emory-ear` journalise la même chose, en `debug`, pour ce que son micro a capté : si les deux
  niveaux diffèrent, le son s'est perdu en route.
- Mesures sur les fichiers d'essai : de la parole normale pique entre -3,7 et -6,3 dBFS pour un
  RMS autour de -24 dBFS. Le même fichier atténué à un pic de -54 dBFS donne `no voice found` ;
  à -44 dBFS le VAD y voit encore de la parole (2,4 s) mais Parakeet ne rend aucun texte
  (`voice found, but nothing came out of it`). Les deux pannes se lisent donc dans le journal.
- **Pas de repli « transcrire tout le tour quand le VAD n'a rien trouvé »** : Kris l'a refusé
  (2026-10-12).

**Le vocabulaire (hotwords) aide avec Parakeet.** Le constat de Whisper plus haut (« les
mots-clés ne l'aident pas ») ne vaut pas ici. sherpa-onnx 1.13.8 les prend avec Parakeet TDT
quand on règle `decoding_method = modified_beam_search`, un fichier de mots (`hotwords_file`) et
un score (`hotwords_score`), avec `modeling_unit = bpe` et un `bpe_vocab` que le modèle ne livre
pas : `scripts/download-models.sh` l'écrit depuis `tokens.txt` (chaque jeton avec le score -id).
Sans lui, sherpa-onnx répond « Cannot find ID for token Em ». Un fichier vide ou absent garde la
recherche gloutonne.
- Configuration : `[transcription]` `decoding_method`, `hotwords`, `hotwords_score` (2.0 par
  défaut), `bpe_vocab` (par défaut dans le dossier de Parakeet). Des mots sans
  `decoding_method` passent en recherche en faisceau ; `greedy_search` demandé avec des mots est
  refusé au démarrage plutôt que silencieusement sans effet.
- Liste d'exemple (`hearing/examples/hotwords.txt` jusqu'au 2026-10-13, aujourd'hui dans le
  fichier de données, voir « Le dictionnaire ») : Emory, Hippocampe, Omarchy, Dokploy, KrisNet,
  Navidrome. Kris et Traefik de la première liste d'essai sont retirés : « trafic » n'est jamais
  devenu Traefik (et ne doit pas : c'est un mot français), et « Kris » est court et proche de mots
  existants, sans effet mesuré seul.
- Effet sur les 2 minutes de Kris (22 phrases), parole non corrigée par la table :

| Score | Hippocampe | Omarchy | KrisNet | Navidrome | Dokploy | Dégâts |
|---|---|---|---|---|---|---|
| sans mots | « Hippocamp », « hypocamp » | « Homarchy » | « ChrisNet » | juste | « DocPuy » | aucun |
| 1,5 | corrigé (2 sur 2) | corrigé | corrigé | « Navidrom » | « DocPuy » | aucun |
| **2,0** | corrigé | corrigé | corrigé | corrigé | « DocPuy » | aucun |
| 3,0 | corrigé | corrigé | corrigé | corrigé | « DokPoy » | « Emoy », « Our Emoi », mots inventés |

  À 2,0, la liste corrige les noms de la liste que Parakeet écorchait, sans rien abîmer d'autre que la casse de quelques mots (« Kit », « Cx »).
  À 3,0 Emory lui-même est détruit : le score reste à 2,0. Ce que le vocabulaire ne rattrape pas
  reste à la table de corrections : « DocPuy » (Dokploy), « Emera » et « pour Mori » (Emory). Un
  essai plus tôt le même jour, sur un autre enregistrement, avait corrigé « DocPly » à 2,0 : l'effet dépend
  de ce que le modèle entend déjà de près.
- **Le coût.** La recherche en faisceau est plus lente que la gloutonne, et les mots ajoutent un
  peu. Mesuré en un seul processus, mêmes phrases, les cinq réglages en alternance, 4 threads,
  meilleur tour de 4 ou 12 (le PC de Kris était chargé par d'autres programmes, la charge à 26-48 :
  les médianes ne veulent rien dire, le meilleur tour est la seule mesure stable) :

| | Kris, 22 phrases (96 s de parole) | Fin coupée, 7 phrases (21 s) |
|---|---|---|
| gloutonne | 8,0 s (365 ms par phrase) | 1,54 s (220 ms) |
| faisceau, sans mots | 9,7 s (+21 %) | 2,05 s (+33 %) |
| faisceau, mots à 1,5 | 9,5 s (+18 %) | 1,89 s (+23 %) |
| faisceau, mots à 2,0 | 12,2 s (+53 %) | 1,89 s (+23 %) |

  Soit de 20 % à 50 % de calcul en plus, **de 0,05 à 0,2 s de plus par phrase** sur ce PC en
  4 threads. C'est ce qui s'ajoute à la dernière phrase après le second appui, dans une chaîne que
  Kris trouve déjà lente. Il peut le retirer en vidant `hotwords`. Sur les cœurs E du serveur, plus
  lents, le surcoût sera plus grand : à remesurer là.

**Les corrections** (table `corrections.tsv`) lisent aussi « A Mira, » ou « Amira, » au début
d'une phrase comme « Emory, », et « A Mira » ou « Amira » seul comme une phrase entière. Mira et
Amira sont des prénoms : rien n'est touché au milieu d'une phrase (« dis à Amira de venir »),
et la ligne est à supprimer si Kris a une Amira qu'il appelle par son prénom au début d'une phrase.

**Les chemins.** Un chemin relatif du fichier de configuration est lu depuis le dossier de ce
fichier, et non plus depuis le dossier où le programme est lancé : `hearing
hearing/examples/hearing.toml` marche depuis la racine du dépôt.

## Parakeet sur la carte graphique (2026-10-12)

Kris a décidé que le serveur reçoit une carte NVIDIA de 12 Go, pour la voix (Kyutai TTS 1.6B,
environ 4,5 Go) et la transcription : sur les cœurs E, Parakeet met environ 1,4 s par phrase,
trop pour la latence visée. `hearing` sait donc calculer sur CUDA, au choix de la configuration, et
rien d'autre n'a changé : le CPU reste le défaut.

**Comment c'est fait.**

- Une clé `provider` (`"cpu"` par défaut, `"cuda"`) dans `[transcription]` (Parakeet) et dans
  `[vad]` (Silero), chacun à part.
- La crate `sherpa-onnx` 1.13.8 ne télécharge que des bibliothèques CPU. Pour CUDA, il faut
  l'archive `...-cuda-13.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu` de la même version de
  sherpa-onnx (254 Mo; une variante CUDA 12.x existe) et le mode `shared` de la crate. C'est la
  feature cargo `cuda` de `hearing`; `hearing/scripts/build-cuda.sh` télécharge l'archive et
  compile, `hearing/build.rs` pose un `rpath` `$ORIGIN` pour que le binaire trouve les
  bibliothèques copiées à côté de lui.
- La compilation CPU ne change pas et ne demande rien à CUDA. `provider = "cuda"` avec une
  compilation CPU est une erreur au démarrage (pas un repli silencieux sur le CPU).
- Sur la machine, rien à compiler : le pilote NVIDIA, le runtime CUDA de la même version majeure
  que l'archive (`libcudart`, `libcublas`, `libcurand`) et cuDNN 9, que l'éditeur de liens
  dynamique doit trouver. Vérifié ici avec CUDA 13.3, cuDNN 9.25, pilote 610.57, RTX 3070 Ti
  8 Go. Sur le serveur, ce sont ces paquets (CUDA et cuDNN du système) qu'il faudra installer.

**Mesures** (mêmes enregistrements qu'à la première ronde : 22 phrases de Kris, 96 s de parole, et
`fin-coupee`, 7 phrases; un processus par essai, l'un après l'autre; 4 threads CPU). Le PC était
chargé par d'autres programmes (charge moyenne de 15 à 76, hors de ces essais) et la carte
partagée avec le moteur Kyutai (4,5 Go) : seul le meilleur tour est fiable, pas la médiane.

| Meilleur tour, modèle int8 | Kris, 22 phrases | `fin-coupee`, 7 phrases | VRAM du processus |
|---|---|---|---|
| CPU, greedy | 4,5 s (205 ms par phrase) | 1,24 s | 0 |
| CUDA, greedy | 5,3 s (242 ms) | 1,11 s | 750 Mo |
| CPU, beam + mots-clés | 5,3 s | 1,38 s | 0 |
| CUDA, beam + mots-clés | 8,0 s | 1,53 s | 750 Mo |

- **Le modèle int8 ne gagne rien sur la carte** : même temps que le CPU, parfois plus lent. Il
  garde les opérations quantifiées sur le CPU (le temps CPU consommé reste de 93 % de celui de
  l'exécution CPU), et cache de la mémoire pour rien. C'est le cas connu d'onnxruntime.
- **Silero sur la carte : à ne pas faire.** 25 ms de calcul par seconde d'audio contre 3,6 sur le
  CPU (7 fois plus lent): le modèle est trop petit, il attend ses données. Il reste sur `cpu`.
- Chargement du modèle int8 : 2 à 3 s sur la carte comme sur le CPU (jusqu'à 27 s observés quand
  la machine était saturée), avec 0,2 à 0,5 s de réchauffage.
- Les textes ne sont pas identiques à ceux du CPU : sur les phrases de Kris, 4 sur 21 diffèrent
  de quelques mots une fois la ponctuation et les majuscules mises de côté (« Harmony 16 » et
  « Harmony size », « un kit » et « Akit »), sans meilleur ni pire. Ce sont des écarts de calcul
  des opérations quantifiées, pas un défaut.
- Modèle fp32 sur le CPU, pour mémoire : 523 ms par phrase, 2,3 fois plus lent que l'int8.

**Ce qui n'a pas pu être mesuré : le modèle fp32 sur la carte.** C'est lui qui devrait profiter
du GPU (l'int8 ne le peut pas). Le fichier fait 2,4 Go de poids (modèle
`csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3`, téléchargé dans
`~/.cache/emory/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3`), et la carte de 8 Go du PC n'avait
que 1,6 Go de libre, le moteur Kyutai en tenant 4,5 : le chargement échoue faute de mémoire
(`BFCArena ... Failed to allocate memory`). Estimation à vérifier : 3 Go environ pour Parakeet
fp32, donc 7,5 Go avec Kyutai, qui tient dans 12 Go avec 4 Go de reste. Le moteur
Kyutai fait partie de la chaîne vivante de Kris et reste chargé : la mesure du fp32 se fera sur la
carte de 12 Go du serveur, ou sur ce PC quand Kyutai sera déchargé (même banc, mêmes
enregistrements).

**Décision pour le serveur** : ne pas compter sur l'int8 pour le GPU. Pour la carte de 12 Go,
le modèle fp32 (`encoder.onnx`, `decoder.onnx`, `joiner.onnx` au lieu des `.int8.onnx`) : voir
« Tout le calcul sur la carte du serveur » ci-dessous (2026-10-13), qui remplace le « en attendant,
l'int8 sur CPU reste le défaut » de cette section pour le serveur.

## Tout le calcul sur la carte du serveur (2026-10-13)

**Décision (Kris, 2026-10-13).** Sur le serveur, **tout le calcul d'IA tourne sur la carte
graphique** (une 12 Go, à acheter) : Kyutai pour la voix (environ 4,5 Go, voir
[`voice.md`](voice.md)) et Parakeet pour la transcription. Le processeur du serveur est pour les
films, la musique et tout le reste, pas pour Emory : il ne sert qu'en repli, et un repli se voit
(voir plus bas). Cela remplace « pas de carte graphique pour la version 1 » et « l'int8 sur CPU
reste le défaut » pour le serveur. Une seule exception, choisie et mesurée : **Silero reste sur le
processeur** (voir plus bas).

**Sur le PC de Kris (développement), Emory n'utilise pas la carte en permanence** : il reste sur
CPU, modèle int8, et `hearing/examples/hearing.toml` ne change pas. La carte du PC ne sert qu'aux
mesures, une fois que Kris les a autorisées (il arrête lui-même voxtype, qui la tient pour sa
dictée). Aucun chargement de modèle sur la carte n'a été fait pour préparer cette section.

**Configurer `hearing` pour la carte** (exemple complet : `hearing/examples/hearing-server.toml`).

```sh
hearing/scripts/build-cuda.sh                      # compilation avec la feature `cuda`
EMORY_FP32=1 hearing/scripts/download-models.sh    # ajoute Parakeet fp32 (2,5 Go) et son bpe.vocab
```

| Clé | Valeur pour le serveur | Défaut | Sens |
|---|---|---|---|
| `[models] parakeet` | le dossier du modèle fp32 | (obligatoire) | `encoder.onnx`, `decoder.onnx`, `joiner.onnx`, `tokens.txt`, `bpe.vocab` |
| `[transcription] provider` | `"cuda"` | `"cpu"` | où Parakeet calcule |
| `[transcription] precision` | `"fp32"` | absente | `"int8"` ou `"fp32"` ; absente, `hearing` prend les fichiers int8 du dossier s'il y en a, sinon les fp32 (comportement d'avant). Donnée, elle est suivie à la lettre : un fichier manquant est une erreur, jamais un autre modèle |
| `[transcription] fallback_to_cpu` | `false` | `false` | voir « Le repli » |
| `[vad] provider` | `"cpu"` | `"cpu"` | Silero |

**Le repli : une erreur au démarrage par défaut, le CPU seulement si la configuration le demande.**
Choix le plus simple et réversible, pour une raison : un serveur qui retombe sans bruit sur le
processeur ralentit les films et la musique sans que personne le sache, et rend Parakeet 2,3
fois plus lent en fp32. Donc `provider = "cuda"` sans carte utilisable (compilation sans la
feature `cuda`, pilote ou bibliothèque CUDA ou cuDNN absente, carte sans assez de mémoire) arrête
`hearing` au démarrage avec la cause. Avec `fallback_to_cpu = true`, `hearing` charge Parakeet sur
le processeur et écrit un `warn` : « Parakeet: cuda is unavailable, FALLING BACK TO THE CPU », avec
la cause ; la ligne `Parakeet ready` donne ensuite le `provider` réel et la `precision`. Les
modèles fp32 sur CPU font 523 ms par phrase contre 205 ms pour l'int8 : qui veut un repli rapide
configure l'int8 sur le CPU, pas ce mécanisme.
- **Le placement est vérifié**, pas seulement demandé. Demander CUDA à onnxruntime ne garantit pas
  qu'il l'a obtenu : selon la compilation, il peut continuer sur le CPU en ne laissant qu'une ligne
  sur stderr. Après le chargement, `hearing` vérifie donc (Linux, `/proc/self/maps`) que la
  bibliothèque `libonnxruntime_providers_cuda` est chargée dans le processus ; sinon c'est la même
  panne qu'une carte absente (erreur, ou repli annoncé). **À confirmer à la mesure** : cette
  vérification n'a jamais tourné avec une vraie carte (aucun chargement CUDA n'a été fait). Si elle
  refuse à tort une carte qui marche, la mesure le montrera au premier essai, et c'est la
  vérification qu'il faut corriger.
- Un échec qui arriverait plus tard (mémoire de la carte prise par un autre programme en cours de
  route) n'est pas un repli : il suit le chemin d'une erreur de transcription. Le repli ne joue
  qu'au chargement et au réchauffage.
- Sans effet sur Silero : `[vad] provider = "cuda"` sans carte est toujours une erreur.

**Silero reste sur le processeur** (`[vad] provider = "cpu"`). Mesuré le 2026-10-12 : 3,6 ms de
calcul par seconde d'audio sur le CPU contre 25 ms sur la carte (7 fois plus lent : le modèle est
trop petit, il attend ses données), soit environ 0,4 % d'un cœur pour une oreille qui parle en
continu. C'est l'exception à « tout sur la carte », et elle ne gêne pas le reste du serveur. La
mesure ci-dessous la refait sur le serveur.

**Le protocole de mesure (préparé le 2026-10-13, pas lancé).** `hearing/scripts/measure-gpu.sh`
lance, pour chaque enregistrement donné (les 22 phrases de Kris et `fin-coupee` du 2026-10-12, lus
sur place, jamais copiés ni envoyés), un processus par réglage, l'un après l'autre :

| Parakeet | Décodage | Silero |
|---|---|---|
| int8 sur CPU | glouton ; faisceau + mots | CPU |
| fp32 sur CPU | glouton ; faisceau + mots | CPU |
| fp32 sur CUDA | glouton ; faisceau + mots | CPU (et une fois, sur CUDA, pour refaire la comparaison) |

Par réglage, dans un tableau Markdown (`results.md`) : temps de chargement, de réchauffage, de
chaque tour (le meilleur est la mesure qui tient sur une machine chargée), par phrase et pour la
phrase la plus lente, temps CPU consommé pendant la transcription (en cœurs), VRAM du processus
(`nvidia-smi`, échantillonnée toutes les 0,2 s) et de la carte, Silero en ms par seconde d'audio,
et le nombre de phrases dont les mots diffèrent de l'int8 glouton sur CPU. Les textes des
enregistrements restent dans le dossier de résultats de la machine, hors du dépôt. Elle ne se
lance qu'avec `--owner-ok` (l'accord de Kris, la carte libérée) ; `--dry-run` montre le plan sans
rien lancer et `--cpu-only` ne touche pas à la carte.

**À mesurer** (rien de cela n'est connu) :
- Parakeet fp32 sur CUDA : le temps par phrase, glouton et faisceau (but : bien sous les 205 ms
  de l'int8 sur CPU), le chargement (le fp32 pèse 2,4 Go de poids), la **VRAM du processus**
  (estimation 3 Go, non vérifiée ; 7,5 Go avec Kyutai pour 12 Go de carte).
- Le temps CPU que Parakeet fp32 laisse encore sur le processeur pendant qu'il tourne sur la
  carte (la part du coût que la carte ne prend pas), à comparer aux 93 % de l'int8 sur la carte.
- Les phrases dont les mots changent entre int8 et fp32.
- La vérification du placement CUDA ci-dessus, avec une vraie carte.
- Le comportement de Silero sur la carte du serveur, refait pour confirmer le facteur 7.
- Kyutai et Parakeet ensemble sur la carte de 12 Go (pas fait ici : un seul programme à la fois).

## Le dictionnaire (2026-10-13)

Emory ajoute un mot au vocabulaire elle-même, en conversation, une fois que l'utilisateur a dit
oui (le côté `mind` est dans [`mind.md`](mind.md), le message dans [`contract.md`](contract.md)).
`hearing` possède le vocabulaire : il garde le fichier et prend les mots nouveaux sur le bus.

**Le vocabulaire est une donnée de l'utilisateur, hors du dépôt** (Emory est générique, Kris le
2026-10-13). Par défaut `hotwords.txt` et `corrections.tsv` dans le dossier de données,
`$XDG_DATA_HOME/emory`, soit `~/.local/share/emory` ; sur le serveur, un volume monté là, ou les
chemins donnés dans `[transcription]` (`hotwords`, `corrections`). Un fichier de mots absent est un
vocabulaire vide, créé au premier mot ajouté ; une table de corrections donnée dans la
configuration doit exister, celle du dossier par défaut peut manquer. Les fichiers de
`hearing/examples/` ne gardent que du générique (Emory, le nom du produit) et servent de point de
départ à copier. Les mots de Kris (Emory, Hippocampe, Omarchy, Dokploy, KrisNet, Navidrome) et
ses corrections sont passés dans `~/.local/share/emory/` sur son PC, avec Hemera en plus.

**Recharger sans redémarrer : il n'y a rien à recharger.** sherpa-onnx 1.13.8 crée un flux avec
ses propres mots (`create_stream_with_hotwords`, que l'implémentation NeMo de Parakeet prend en
recherche en faisceau) : `hearing` lui donne les mots tels qu'ils sont à chaque phrase, au lieu
d'un fichier lu une fois au chargement. Un mot ajouté sert dès la phrase suivante, sans toucher à
celle en cours. Le reconnaisseur charge toujours le `bpe.vocab` en faisceau, pour découper les
mots qui arrivent après le démarrage.
- **Le coût, mesuré** (Parakeet int8, 4 threads, ce PC, 200 tours) : créer le flux d'une phrase
  avec 10 mots prend 0,1 ms, avec 50 mots 0,26 ms, contre environ 365 ms pour la transcrire. Le
  chemin écarté, reconstruire le reconnaisseur en tâche de fond puis l'échanger entre deux
  phrases, aurait coûté 2,3 s de chargement et la mémoire du modèle en double le temps de
  l'échange (gênant sur la carte graphique).
- **Vérifié en vrai** sur un bus à part, sans redémarrer `hearing` : 22 s de l'enregistrement de
  Kris rejouées avec Emory seul dans le fichier donnent « Omarchi » et « Chris NetPoy » ; après
  l'ajout d'Omarchy, KrisNet et Dokploy par le bus, le même extrait donne « Omarchy » et
  « KrisNet ». « DocPuy » reste, comme mesuré le 2026-10-12 : le vocabulaire ne le rattrape pas.
- **La recherche gloutonne ne prend pas de mots.** Si le fichier est vide au démarrage (et sans
  `decoding_method`), `hearing` tourne en glouton : un mot ajouté est écrit, la réponse le dit
  (`in_use: false`) et il compte au prochain démarrage. Choix le plus simple et réversible :
  passer en faisceau pour un fichier vide coûterait 20 à 30 % de calcul pour rien, et
  reconstruire le reconnaisseur pour ce seul cas ne vaut pas sa complexité.

**L'ajout** (`hearing.vocabulary.add`, demande avec réponse) : le mot est validé (une ligne, 40
caractères au plus, des lettres de tout alphabet, chiffres, espaces, tirets, apostrophes et
points, au moins une lettre ; ni `/` qui sépare les mots donnés à sherpa-onnx, ni `:` qui
annonce un score, ni `#`), ajouté à la fin du fichier, et le fichier gardé tel que l'utilisateur
l'a écrit (commentaires compris). Un mot déjà là, quelle que soit la casse, n'est pas réécrit et
la réponse est un succès. Le mot n'entre pas dans les journaux.

**Pas de règle de correction ajoutée d'office.** `heard_as`, ce que la transcription a donné à la
place, est gardé en commentaire au-dessus du mot (`# 2026-10-13 : entendu « et mera »`), pas
transformé en règle : une règle s'applique à toutes les phrases, et une forme entendue une fois
peut être un vrai mot ou un prénom (« A Mira » a demandé des précautions à la main) ; le
vocabulaire seul suffit souvent. Le commentaire garde de quoi écrire la règle à la main si le mot
ne suffit pas.

**Après la fusion, pour la chaîne qui tourne** : l'exemple `hearing/examples/hearing.toml` ne
donne plus de chemin, donc `hearing` lit `~/.local/share/emory/hotwords.txt` et
`corrections.tsv`, où sont déjà les mots de Kris. Il suffit de redémarrer `hearing` (et `mind`,
pour les outils).

## Hors version 1

- L'oreille de pièce : détection de voix, mot de réveil, fin de tour, mémoire tampon, voix
  (conçue, pas réalisée).
- Plusieurs oreilles (conçu, pas réalisé).
- Comprendre qu'une phrase est adressée à Emory sans le mot de réveil : c'est `mind` qui le
  fera.
- La bouche, sauf l'interruption, qui la concerne aussi.
- Les yeux et les mains.

## Questions ouvertes

1. Le micro de pièce à acheter, après l'essai avec le casque.
2. Le partage des cœurs E avec la bouche quand les deux tournent en même temps (Kris coupe la
   parole à Emory) : pas mesuré.
