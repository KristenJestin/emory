# emory-ear

The ear of an Emory device (`specs/hearing.md`, `specs/README.md`): it captures the microphone
**only while a turn is open**, turns it into 16 kHz mono 16-bit chunks of 20 ms and sends them to
the hearing on the bus (`ear.<device>.audio`), framed by `Button { start }` and `Button { end }`
(`ear.<device>.button`). What opens and closes a turn is a *trigger*, a module of the program:
`shortcut` for now (a keyboard shortcut, through the GlobalShortcuts portal); `button` and
`continuous` come later.

```sh
cargo build --release -p ear                       # target/release/emory-ear
emory-ear                                          # reads ~/.config/emory/config.toml
emory-ear --config FILE                            # another configuration file
emory-ear --wav FILE                               # a WAV file for one turn, instead of the mic
emory-ear --list-mics | --check-mic | --check-trigger
```

## Configuration

One file per device, `~/.config/emory/config.toml`, shared by `emory-ear`, `emory-mouth` and
`emory-hands`: see [`examples/config.toml`](examples/config.toml). `emory-ear` needs the top
level (`device`, `bus`, `bus_ca`, `bus_nkey_seed_file`) and the `[ear]` section; without `[ear]`
it says so and exits, since this device has no ear.

| Key | Default | |
|---|---|---|
| `device` | the hostname | goes into the bus subjects: lowercase letters, digits, dashes |
| `bus` | `nats://127.0.0.1:4222` | |
| `bus_ca`, `bus_nkey_seed_file` | none | TLS and nkey, for the locked-down server |
| `[ear] mic` | `"default"` | `"default"` or a device name (`--list-mics`) |
| `[ear] trigger` | `"shortcut"` | |
| `[ear] hold_threshold_ms` | `300` | a press held at least this long is a hold, else a tap |

The shortcut works like the dictation key: a **tap** opens a turn and the next tap closes it; a
**hold** opens a turn that closes on release. The turn opens on the press, so no word is lost.
The microphone itself takes about 180 ms to deliver its first sound once opened (`--check-mic`
shows it).

## The shortcut trigger on Hyprland

`emory-ear` registers one shortcut with the portal (`xdg-desktop-portal-hyprland`): app id
`dev.emory.ear`, shortcut id `talk`, description « Talk to Emory ». The portal does not choose the
key; the compositor does. Two things to set up, once.

**1. An identity for the portal.** A program outside a sandbox is known to the portal only through
a desktop file. Install [`dev.emory.ear.desktop`](dev.emory.ear.desktop) as
`~/.local/share/applications/dev.emory.ear.desktop`, and make its `Exec=` line a program that
exists: the portal refuses (« App info not found ») an app whose `Exec` is not found, so put the
absolute path of `emory-ear` there if it is not in the `PATH` of the user session.

**2. A bind** in `~/.config/hypr/bindings.lua`, one for the press and one for the release, like the
dictation key (`SUPER + SHIFT + A` is free at the time of writing):

```lua
-- Talk to Emory: tap = open / close, hold = push-to-talk.
o.bind("SUPER + SHIFT + A", "Talk to Emory", hl.dsp.global("dev.emory.ear:talk"))
o.bind("SUPER + SHIFT + A", nil, hl.dsp.global("dev.emory.ear:talk"), { release = true })
```

(`hl.dsp.global("<app id>:<shortcut id>")` is Hyprland's `global` dispatcher; in the old syntax,
`bind = SUPER SHIFT, A, global, dev.emory.ear:talk` and `bindr = SUPER SHIFT, A, global, dev.emory.ear:talk`.
Do not add the `repeat` flag.) With only the press bind, the key still works, as a plain toggle.

While `emory-ear` runs, `hyprctl globalshortcuts` lists `dev.emory.ear:talk -> Talk to Emory`.
`emory-ear --check-trigger` runs the trigger alone, with neither microphone nor bus, and prints
`Open` / `Close` as the key is pressed: the way to try the bind.

## Try without a voice

`emory-ear --wav FILE` replaces the microphone by a 16 kHz mono 16-bit WAV file for one turn
(`ffmpeg -i in.wav -ar 16000 -ac 1 -c:a pcm_s16le out.wav`): `start`, the file at real-time pace,
`end`. With the `hearing` running (`hearing/README.md`) and a configuration for the device:

```sh
emory-ear --config my.toml --wav sentence.wav
```

`--check-mic` opens the microphone for half a second and prints how many chunks came and their
peak level; it keeps and sends nothing.

## Layout

| | |
|---|---|
| `src/config.rs` | the device configuration |
| `src/trigger/` | the `Trigger` trait, and the `shortcut` module (feature `shortcut`, on by default) with its tap / hold state machine (`gesture.rs`) |
| `src/capture.rs`, `src/resample.rs` | the microphone (cpal) and its conversion to the bus format; if the device offers 16 kHz mono it is used as it is, otherwise a windowed-sinc resampler does the job |
| `src/bus.rs`, `src/wav.rs` | the bus, and the WAV test input |

Logs go through `tracing` (`RUST_LOG=info` by default); they never contain audio.
