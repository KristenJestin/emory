# Third-party pieces

Emory is under the [AGPL-3.0](LICENSE) (only). This is what it uses from others, under which
licence, and whether it is in this repository, built into a binary, or only downloaded or run
beside it. Every licence here allows Emory to be published and distributed under the AGPL-3.0.

## In this repository

| Piece | Where | Licence | Note |
|---|---|---|---|
| [Figtree](https://github.com/erikdkennedy/figtree), by Erik Kennedy | `assets/brand/fonts/` | SIL Open Font License 1.1, text in `OFL-Figtree.txt` | the fonts of the pictures only; shipped with their licence |
| [Bricolage Grotesque](https://github.com/ateliertriay/bricolage) | `assets/brand/*.svg` | SIL Open Font License 1.1 | the name « Emory » is drawn as outlines of this font; no font file is shipped |
| Recorded Wikipedia answers and a Wikipedia page | `mind/tests/fixtures/web/summary-*.json`, `search-*.json`, `page-fr-saint-veran.html` | CC BY-SA 4.0 ([Wikipedia](https://www.wikipedia.org/), its authors) | test data, as the service returned it |
| Recorded Open-Meteo answers | `mind/tests/fixtures/web/weather-*.json` | CC BY 4.0 ([Open-Meteo](https://open-meteo.com/)) | test data |
| Recorded DuckDuckGo pages | `mind/tests/fixtures/web/search-ddg-*.html` | DuckDuckGo's; the results quote other sites | test data, kept so the parser is tested on the real page |

## Built into the binaries

The Rust crates in `Cargo.lock`. Their licences, from `cargo metadata`: MIT, Apache-2.0 (most
dual MIT OR Apache-2.0), BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0, Unlicense, CC0-1.0,
BSL-1.0, CDLA-Permissive-2.0 (the root certificates of `webpki-roots`), and MPL-2.0 (`cssparser`,
`selectors`, `dtoa-short`, used unchanged). All are compatible with the AGPL-3.0.

`hearing` links [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) (Apache-2.0) and
[ONNX Runtime](https://github.com/microsoft/onnxruntime) (MIT), which the `sherpa-onnx` crate
downloads at build time.

## Downloaded or run beside Emory, not shipped

| Piece | Used by | Licence |
|---|---|---|
| [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), NVIDIA, as converted by sherpa-onnx | `hearing`, `hearing/scripts/download-models.sh` | CC BY 4.0 |
| [Silero VAD](https://github.com/snakers4/silero-vad) | `hearing`, same script | MIT |
| [Pocket TTS](https://github.com/kyutai-labs/pocket-tts), Kyutai: the program | `engines/pocket` | MIT |
| [Pocket TTS weights](https://huggingface.co/kyutai/pocket-tts) | `engines/pocket` | CC BY 4.0 |
| [Kyutai TTS 1.6B](https://huggingface.co/kyutai/tts-1.6b-en_fr) and the [`moshi`](https://github.com/kyutai-labs/moshi) package | `engines/kyutai` | CC BY 4.0 (weights), Apache-2.0 (`moshi`) |
| [Kyutai TTS voices](https://huggingface.co/kyutai/tts-voices): `estelle` (`unmute-prod-website/developpeuse-3.wav`) and `cml-tts/fr/5476_3103_000072-0001.wav` | `dev/voice.toml`, `dev/voice-kyutai.toml` | CC0 and CC BY 4.0 ([CML-TTS](https://openslr.org/146/)). Some other voices of that repository are non-commercial (Expresso): check a voice's licence before choosing it |
| [PyTorch](https://pytorch.org/) | `engines/*` | BSD-3-Clause |
| [NATS server](https://github.com/nats-io/nats-server) (`nats:2.12-alpine` image) | `dev/compose.yml` | Apache-2.0 |
| [claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) (run with `npx`) | `mind`, ACP mode | Apache-2.0 |
| [Claude](https://www.anthropic.com/claude), through Claude Code or the Anthropic API | `mind` | a service under Anthropic's terms; nothing of it is shipped |

The CC BY 4.0 models ask for credit when they are redistributed: Emory does not redistribute
them, it downloads them from their authors.
