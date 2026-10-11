# oreilles

Les oreilles d'Emory, version 1 : l'oreille à bouton (`specs/oreilles.md`). Du son des postes
(`poste.*.audio`) aux phrases (`oreilles.phrase`) et au tour entier (`oreilles.tour`), par Silero VAD
et Parakeet TDT 0.6B v3 int8 (sherpa-onnx, dans le binaire).

```sh
oreilles/scripts/telecharger-modeles.sh          # modèles dans ~/.cache/emory/modeles/
docker compose -f dev/compose.yml up -d           # le NATS de développement
cargo run --release -p oreilles -- oreilles/exemples/oreilles.toml
```

Essai de bout en bout, dans un autre terminal (le fichier doit être en 16 kHz mono 16 bits :
`ffmpeg -i entree.wav -ar 16000 -ac 1 -c:a pcm_s16le sortie.wav`) :

```sh
cargo run --release -p oreilles --bin rejouer -- sortie.wav
```

`rejouer` joue le fichier comme un poste (`debut`, morceaux de 20 ms au rythme du temps réel,
`fin`), affiche les phrases et le tour renvoyés, et le temps de la fin au tour.

Les journaux (`RUST_LOG=info`) donnent, par phrase, sa durée et son temps de transcription, et par
tour le temps de la `fin` au `Tour` publié. Ils ne contiennent jamais le texte dit.
