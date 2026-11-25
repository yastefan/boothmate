# Architecture

Boothmate is a core with pluggable input modules. It talks to grandMA3 over two channels: OSC for fast values, and a Lua plugin for everything OSC does not provide.

```
MIDI controllers <-> MIDI module --+
                                   +--> Core <-> OSC in/out      --> grandMA3
CDJs + mixer ----> CDJ module -----+      ^  <-> Lua plugin      --> grandMA3
 (read-only)                              |
                                     USB stick: devices/ ma-profiles/ shows/ logs/
```

## Principles

- Portable: one binary per platform, no installer, data next to the binary.
- Rust, small, no JVM.
- Runs on the same machine as grandMA3 onPC, or as a separate machine on a console network.
- Generic: controllers are described by device profiles.
- Version-proof: grandMA3 OSC addresses and Lua differences live in `ma-profiles/<version>.toml`. The Lua plugin reports the version on connect.
- Push over polling. Where grandMA3 offers no events, poll sparingly and send only changes.
- CDJs are read-only: no browsing, no track loading, no player control.

## grandMA3 channels

| Channel | Direction | Used for |
| --- | --- | --- |
| OSC | in | `/PageX/FaderY`, `/KeyY`, `/EncoderY` only. Never `/cmd`. |
| OSC (Send = Yes) | out, push | Key press/release with current cue, fader values (~30/s while moving) of sequences, groups and masters. Addressed by object (`/14.14.1.7.<seq>` in 2.5.1), not by executor |
| Lua plugin | both | Executor assignment, appearance colours, names, page changes (via `HookObjectChange`), version, resync |

M0 result: feedback works push-based. OSC carries playback state and fader values; `HookObjectChange` fires on labels, appearances and page changes, but not on playback or faders. Only the executor mapping and start state are read on connect. Details: [spikes/m0-feedback.md](spikes/m0-feedback.md).

### Lua plugin protocol

The plugin listens on TCP (default `127.0.0.1:9002`; `bind=0.0.0.0` only with a token). Boothmate connects and reconnects every 2 s. One JSON object per line:

| Direction | Message |
| --- | --- |
| Boothmate -> plugin | `{"op":"hello","token":"..."}`, `{"op":"resync"}` |
| plugin -> Boothmate | `{"ev":"hello","plugin":"0.1.0","ma":"2.5.1.1"}` |
| plugin -> Boothmate | `{"ev":"page","page":1,"executors":[{"no":201,"class":"Sequence","obj":2,"name":"...","addr":"14.14.1.7.2","color":"#ff0000","active":true,"fader":100,"cue":"2 Rot"}]}` |
| plugin -> Boothmate | `{"ev":"error","msg":"..."}` |

The plugin sends a page snapshot on connect, on `resync`, when a hooked object changes (assigned objects, their appearances, the page, the Sequences and Appearances pools) and when `CurrentExecPage()` changes (compared every frame, a local call). At most 10 snapshots per second.

## MIDI module

- MIDI learn: start learn, move a control (type detected: note, CC, 14-bit, relative), then pick an executor in the UI or press it in grandMA3.
- Feedback per element type:

| Element | From grandMA3 | Sent as |
| --- | --- | --- |
| RGB button | Appearance colour + running state | Nearest palette colour from the device profile; bright = active, dim = inactive, off = empty |
| Motor fader | Fader value 0-100 | Pitch bend or CC (7/14 bit); locked while touched |
| Endless encoder | Fader or encoder value | CC for the LED ring; input relative |
| Display | Executor or cue name | SysEx per device profile |

- Follow the current page or stay on a fixed page, per mapping. Full resync on page change, connect and console restart.
- No feedback loops; feedback rate limited per element.

## CDJ module (read-only)

Based on the [DJ Link Ecosystem Analysis](https://djl-analysis.deepsymmetry.org/).

| Stage | What Boothmate does | What it gets |
| --- | --- | --- |
| 0 Discovery | Listens on UDP 50000 | All players and mixers (name, number, IP). Keep-alive ~2 s on CDJs |
| 1 Beat | Listens on UDP 50001 | Per beat: BPM, pitch, beat in bar. CDJ-3000: position every 30 ms |
| 2 Status | Announces itself as a silent virtual player | Status every ~200 ms on UDP 50002: playing, track ID, beat number, master, on-air |
| 3 Metadata | Queries the loaded track only (TCP, port via 12523) | Title, beat grid, waveform, cues, phrases |

Events to grandMA3: beat, downbeat, 4-bar and 32-beat grid, track change, master change, phrase change, "next phrase in N beats". Rules live in `shows/<name>/cdj.toml`.

Known limits: metadata queries need a player number 1-4 that is actually in use (CDJ-3000 allows 5-6). Phrases only exist for tracks analysed in rekordbox. Opus Quad, XDJ-AZ and Denon StageLinQ are out of scope for now. The virtual player does not work with rekordbox running on the same machine.

## Testing without CDJs

1. Replay of captured network traffic (pcap) for regression tests.
2. [CDJ_DJM_Emulator](https://github.com/OllieCross/CDJ_DJM_Emulator) for daily development.
3. [cdj3k-emu](https://github.com/nsaintot/cdj3k-emu) (real firmware in QEMU) as a final check.
4. Built-in mock source for unit tests.

## Safety

- Only executor addresses are sent to grandMA3, never free commands. The Lua plugin accepts a fixed message set and a shared token.
- Default network binding is localhost. On a network: fixed console IP and sender checks.
- On connection loss controllers show "disconnected", motor faders stay put, and Boothmate resyncs on reconnect.
- Unknown grandMA3 version: warn and fall back to the closest profile in read-only mode.
- Logs go to `logs/` on the stick.

## Milestones

- **M0 Spikes (done):** `HookObjectChange` and native OSC feedback tested in onPC 2.5.1, see [spikes/m0-feedback.md](spikes/m0-feedback.md).
- **M1 Core:** Rust project, macOS + Windows builds, portable layout, OSC + Lua connection, MA profile 2.5, small web UI.
- **M2 MIDI mapper:** device discovery and profiles, MIDI learn, feedback, page follow, two reference profiles.
- **M3 CDJ discovery and beat:** automatic discovery of CDJs and mixers, beat grid, test setup (emulator, replay, mock).
- **M4 CDJ status:** silent virtual player, track change, master, on-air.
- **M5 Metadata and timeline:** beat grid, waveform, phrases, "drop in N beats", timeline view.

## Licensing notes

Boothmate is GPL-3.0-or-later. Compatible sources: MIT/Apache crates, rekordcrate (MPL-2.0), alphatheta-connect (MIT), GPL-3.0 projects such as pam-osc. beat-link and LJ-Link (EPL-2.0) are not used; the Pro DJ Link code is written from the Deep Symmetry protocol documentation.
