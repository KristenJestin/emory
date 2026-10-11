# Pocket TTS, le moteur de voix de la bouche

Kyutai Pocket TTS (`pocket-tts` 3.3.0), modèle français `french` (6 couches), voix `estelle`.
Le serveur est celui de la bibliothèque (`pocket-tts serve`) ; `serve.py` ne fait que le lancer
avec les réglages de l'environnement, parce que la bibliothèque fixe elle-même le nombre de fils
de torch à 1 et qu'on veut pouvoir le changer.

| Variable | Défaut | |
|---|---|---|
| `POCKET_LANGUAGE` | `french` | le modèle (`french_24l` : trop lent sans carte graphique) |
| `POCKET_VOICE` | `estelle` | la voix par défaut |
| `POCKET_THREADS` | `1` | les fils de torch |
| `POCKET_PORT` | `8000` | |

L'API : `GET /health`, et `POST /tts` (formulaire `text`, `voice_url` facultatif) qui rend un WAV
mono 16 bits à 24 kHz diffusé pendant le calcul. Le modèle et l'état de la voix sont chargés une
fois au démarrage. Les poids se téléchargent au premier démarrage dans `/cache` (`HF_HOME`) : y
monter un volume. Fermer la connexion arrête le calcul.

```
docker compose -f dev/compose.yml up -d     # le bus et le moteur, sur 127.0.0.1
curl -s -X POST http://127.0.0.1:8000/tts -F 'text=Bonjour.' -o bonjour.wav
```
