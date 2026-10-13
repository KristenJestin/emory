# voice

The voice of Emory, version 1 (`specs/voice.md`): receives the sentences to say (`voice.say`),
has them synthesised by the engine (Pocket on the processor, Kyutai TTS 1.6B on the graphics
card) and sends the audio back to the mouth (`mouth.<name>.audio`), chunk by chunk.

```sh
docker compose -f dev/compose.yml up -d          # the development NATS and Pocket
cargo run -p voice -- dev/voice.toml             # from the repository root
cargo run -p voice --bin voice-try -- "Bonjour."  # says a sentence, writes voice.wav
```

## Configuration

`voice <file.toml>`: see `dev/voice.toml` and the commented list of keys at the top of
`src/config.rs`. A value out of its range stops the start with a message that names the key.

### The loudness of the voice: `[loudness]`, or `gain`

Two ways, the first one is the one to use:

**`[loudness]`: a compressor, a look-ahead limiter and a target level.** Off by default.

```toml
[loudness]
enabled = true
target = -14.0       # LUFS the speech is brought to (-40 to -6), default -16
# ceiling = -1.0     # dBFS nothing goes past (-12 to -0.1)
# lookahead_ms = 5.0 # how far ahead the limiter looks, and the delay the stage adds (1 to 20)
# ratio = 3.0        # compression ratio, 1 is none (1 to 20)
# threshold = 3.0    # dB above the speech level where the compression starts (-20 to 20)
# attack_ms = 5.0    # 0.1 to 100
# release_ms = 80.0  # 1 to 2000
```

It works on the audio of every engine, chunk by chunk as it streams, and not on the waiting cue.
Its steps, in `src/loudness.rs`:

1. the level of the speech is measured in LUFS (K-weighted, 10 ms blocks, gated, so pauses and
   breaths do not count); nothing is trusted before 40 ms of speech;
2. a compressor, relative to that level, lowers the louder syllables towards the average;
3. a make-up gain brings the compressed speech to `target` (at most +24 dB, never on silence or on
   a noise floor under -50 LUFS);
4. a limiter that looks ahead (`lookahead_ms`) lowers the gain *before* a peak, with a smooth
   ramp, so nothing goes past `ceiling` and nothing is clipped.

The state belongs to the sentence: it carries over the chunks (the output is the same whatever
the cutting) and the last chunk, the one with `Emory-End`, also carries what the look-ahead was
holding, so nothing is lost. The delay is the look-ahead in the content of the sound: the first
chunk leaves as soon as the engine sends its first packet, `lookahead_ms` shorter. Compute: about
2 ms per second of audio. When it is on, `gain` is ignored (a warning at start-up if it is not 1).
`voice` logs the level and the deepest limiting of each sentence (`level_lufs`, `limited_db`).

The numbers measured on Pocket, and how, are in `specs/voice.md` ("La sonie de la voix");
`cargo run --release -p voice --example loudness-check` measures files (`sentences`, `tones`,
`timing`, `measure`) with the same code.

**`gain`: a plain factor, used when `[loudness]` is off.**

```toml
gain = 2.0    # 0.1 to 8, default 1
```

A factor on the amplitude of what the engine says, whatever the engine: 1 sends the audio as it
comes, 2 doubles it (about +6 dB), 0.5 halves it. It applies chunk by chunk as the audio streams
(no wait, no added latency) and does not touch the waiting cue, which has its own `[cue] volume`.

Up to 0.8 of full scale a sample is only multiplied. Past that the curve bends smoothly towards
full scale (a `tanh` that starts with the same slope), so a loud syllable is rounded, never cut:
a hard clip would crackle. With a gain of exactly 1 the bytes are left as the engine sent them.
Pocket peaks around 0.3 to 0.4 of full scale as it comes: `gain = 2.0` brings that to 0.6 to 0.9.
A gain much above 2 makes the speech crackle (the soft curve flattens the peaks, which is
distortion): that is what `[loudness]` is for.

### The pronunciation lexicon: `lexicon`, `lexicon_enabled`

A French engine reads « GitHub » as « jithub » and « SSH » as a word. Before the synthesis, whatever
the engine, every sentence goes through a text stage (`src/text.rs`):

1. **The lexicon**: whole words replaced by how they must be written to be read right
   (`GitHub` → « guite-heub »). Case-sensitive unless the entry says otherwise, longest entry first,
   the punctuation around the word kept, a word inside another one untouched. An entry wins over 2.
2. **The spelled acronyms**: a word of 2 to 6 capitals A to Z or digits (at least one letter) that no
   entry covers is spelled with the names of the letters, joined by hyphens: `SSH` → « èsse-èsse-ache »,
   `MP3` → « èm-pé-trois », `4G` → « quatre-jé ». A final lowercase `s` is dropped (`PCs` → « pé-cé »).
   Not spelled: a single letter, a word of more than 6, a word with lowercase letters (`iPhone`), a
   number, a word that starts with two digits (`10H`), accented capitals (`ÉTÉ`), a word with `_`.
   Acronyms read as words (`NASA`, `RAM`…) are listed in `not_spelled`.

```toml
lexicon_enabled = true             # default; false: the sentence goes as the mind wrote it
lexicon = "lexicon.fr.toml"        # default: the built-in lexicon (data/lexicon.fr.toml)
```

The stage is **on by default with the built-in lexicon**. A file given in `lexicon` **replaces** the
built-in one (copy it and complete it; `dev/lexicon.fr.toml` is an example). `~/` stands for the home
folder and a relative path is read from the folder of the configuration file (like `hearing`). The
file is read once at the start: an unreadable file or a malformed entry stops the start with a message
that names the file and the entry.

The file is TOML, commented in French (see `data/lexicon.fr.toml`):

```toml
not_spelled = ["NASA", "RAM"]      # acronyms read as words

[replace]                          # case-sensitive
GitHub = "guite-heub"
"Wi-Fi" = "ouifi"                  # a key that is not only letters is quoted

[replace_any_case]                 # whatever the case
okay = "oké"

[letters]                          # the name of a letter or digit, only to change one
R = "èrre"
```

The written forms of the built-in lexicon were chosen with Pocket (voice estelle): each candidate was
synthesised and transcribed back by Parakeet, and only the words read wrong are in it. Nothing was
listened to. Kyutai was not tried: if it reads English words well, turn the stage off or use its own
file.
