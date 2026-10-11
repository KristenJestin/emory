# La bouche d'Emory : version 1

Statut : brouillon du 2026-10-11, à valider par Kris.

La bouche fait parler Emory : elle reçoit le texte de l'esprit, le transforme en voix sur le
serveur, et envoie le flux audio au poste qui a entendu Kris (voir le [README](README.md)).

## Principes

1. **La voix se fabrique sur le serveur, en local.** Le poste ne fait que jouer le flux qui
   revient. Aucun service de synthèse dans le cloud en version 1.
2. **En flux, phrase par phrase.** La bouche commence à parler dès que l'esprit a écrit sa
   première phrase. Le but : le premier son moins de 0,2 s après la réception d'une phrase.
3. **Pas de phrase d'attente.** Emory ne dit jamais « je regarde » pour faire patienter. Il répond
   avec ce qu'il sait ; quand un travail lancé en parallèle aboutit, l'esprit jette les phrases
   pas encore dites et la bouche enchaîne au bord de la phrase en cours (« … ah, je l'ai : »),
   jamais au milieu d'un mot. Ce qui est dit en attendant doit être utile, pas du remplissage.
4. **Le moteur est un adaptateur.** Changer de moteur (passer sur la carte graphique, ou au cloud
   un jour) ne touche ni à l'esprit ni au poste.
5. **Emory ne gêne pas le reste du serveur**, avec les mêmes limites que les oreilles.

## L'interruption

En version 1, un appui sur le bouton pendant qu'Emory parle le fait taire : la bouche coupe le
flux et jette ce qui restait à dire, et le poste se met à écouter. Avec l'oreille de pièce,
plus tard, ce sera la reprise de parole de Kris (voir [`oreilles.md`](oreilles.md)).

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
en Python, il tourne donc dans un conteneur à lui, que `bouche` (en Rust) appelle
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

## Ce qui est lu

- **Les mots anglais et les noms propres** : un moteur français lit « Neo4j » ou « MCP » à la
  française. Un lexique de prononciation les remplace avant la synthèse (« MCP » → « èm cé pé »),
  tiré, comme le vocabulaire des oreilles, des titres et des alias d'Hippocampe.
- **Les nombres, les dates et les heures** sont écrits en toutes lettres avant la synthèse ; le
  plus simple est de le demander à l'esprit.

## Questions ouvertes

1. Une meilleure voix qu'estelle : clonage de Pocket (jeton Hugging Face) ou Kyutai 1.6B avec
   une carte graphique.
2. La lecture des nombres, des sigles et des noms propres par Pocket : pas encore écoutée en
   détail, le lexique et l'écriture en toutes lettres restent prévus.
3. Le format du flux entre l'esprit et la bouche (texte phrase par phrase, annulation des phrases
   pas encore dites) : sujets `bouche.dire` et `bouche.stop` du bus, format à fixer dans `contrat/`.
