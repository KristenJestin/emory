# Les mains d'Emory : version 1

Statut : brouillon du 2026-10-11, à valider par Kris.

Les mains sont ce qu'Emory fait dans le monde. L'esprit décide ; une main exécute, sans réfléchir.

## Comment une main marche

- **Une main tourne là où se trouve ce qu'elle pilote.** La musique joue sur le PC de Kris : sa
  main tourne sur le PC. Une main n'a ni modèle ni décision : elle reçoit une commande par le bus,
  l'exécute, et répond avec le résultat (demande avec réponse NATS, sujets `mains.<main>.<action>`).
- **Côté esprit, chaque main est un module du serveur MCP d'Emory** (voir
  [`esprit.md`](esprit.md)) : ses actions deviennent des outils pour le LLM, et `esprit` les
  relaie sur le bus à la main concernée.
- Une main absente (le PC éteint) répond par un délai dépassé ; Emory le dit simplement.
- Les mains sont des programmes à nous, en Rust, dans `mains/`, un par main.

## La première main : la musique

Kris s'en sert tous les jours, et elle pose le modèle de toutes les mains (décidé le 2026-10-11).

Elle pilote `poste music`, le lecteur Navidrome de Kris (dépôt `poste`, module `music`), par sa
ligne de commande (sorties `--json`) ou son socket de contrôle. Ce qu'Emory sait faire :

- **chercher et jouer** : un morceau, un album, un artiste, une playlist, un mix
  (`search`, `playlists`, `mixes`, `play`, `queue`) : « mets du Daft Punk », « lance ma playlist
  du soir » ;
- **contrôler** : pause, reprise, suivant, précédent, arrêt, volume ;
- **dire ce qui joue** (`now`) : « c'est quoi ce morceau ? » ;
- **aimer** un morceau (`like`).

**La musique baisse quand Emory parle**, et remonte quand il a fini. La bouche prévient la main
du poste où elle parle ; c'est un réglage du volume, rien de plus.

## Plus tard

- **Home Assistant, pour les appareils de la maison.** Kris n'a aucun appareil connecté
  aujourd'hui. Le jour où il en aura, Home Assistant les pilote et expose un serveur MCP : il
  devient une main d'Emory, sans réécrire la gestion des appareils. Sa partie vocale (Assist) ne
  remplace pas la chaîne d'Emory : elle n'a ni Hippocampe, ni mémoire tampon, ni reconnaissance
  des voix, ni la règle des outils fournis par Emory seul.
- Le bureau de Kris (`poste.island` : minuteurs de l'île, verrouillage, fenêtres) : à voir à
  l'usage. Les minuteurs et rappels d'Emory vivent dans `esprit`, pas sur le bureau.

## Questions ouvertes

1. Piloter `poste music` par la ligne de commande ou par son socket : le socket évite de lancer
   un processus par commande, la ligne de commande est la façade stable du module.
