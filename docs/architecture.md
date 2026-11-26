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
| OSC | in | `/PageX/FaderY`, `/PageX/KeyY`, `/PageX/EncoderY` only, on a second OSC line (Receive = Yes, own port: one line uses one port for both directions, and Boothmate already holds the output port on 127.0.0.1). Never `/cmd`. |
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
| Boothmate -> plugin | `{"op":"speed","master":1,"bpm":128.0}`: `Master 3.1 At BPM 128.00` (tempo link) |
| Boothmate -> plugin | `{"op":"phase","master":1}`: `FastSync Master 3.1`, at most every 4 s, followed by the exact BPM again |

The plugin sends a page snapshot on connect, on `resync`, when a hooked object changes (assigned objects, their appearances, the page, the Sequences and Appearances pools) and when `CurrentExecPage()` changes (compared every frame, a local call). At most 10 snapshots per second.

## MIDI module

- Ports via `midir` on a thread of their own; rescanned every 2 s (hot plug). Each input is paired with its output by name (Windows: `MIDIIN2 (X)` / `MIDIOUT2 (X)`) or by the profile's port pattern. macOS: a virtual port pair "Boothmate". Windows needs loopMIDI for virtual ports.
- Device profiles `devices/<id>.toml`: control groups (button, fader, encoder, display) with note/CC/pitch bend addresses and a feedback type. Reference profiles: Akai APC mini mk2 (RGB pads by palette, brightness by channel) and Mackie Control (motor faders as pitch bend with touch notes, LED rings, scribble strips). A built-in virtual controller in the web UI (switchable) uses the same path.
- Devices without a profile: learned controls get mirrored feedback (LED 127/0, fader value). CC controls are guessed from the values while learning (absolute fader, 0/127 button, relative encoder) and can be changed in the mapping list.
- MIDI learn: start learn, move a control, then click an executor in the UI or press its key / move its fader in grandMA3 (the OSC event is matched to the executor via the plugin's page snapshot). Learn stays on for the next control.
- Mappings in `midi.toml`: device (profile id or port name), control, kind, MIDI address, executor, page (absent = follow the current page).
- Feedback is computed from the state on every change and sent as a diff per control:

| Element | From grandMA3 | Sent as |
| --- | --- | --- |
| RGB button | Appearance colour + running state | Nearest palette colour; bright = running, dim = assigned, off = empty |
| LED button | Running state | Profile values for on / dim / off |
| Motor fader | Fader value 0-100 | Pitch bend or CC; held while touched and 400 ms after the last move |
| Endless encoder | Fader value | CC for the LED ring; input relative, sent as `/PageX/EncoderY` |
| Display | Executor name | SysEx template per device profile |

- Only the current page is known (plugin snapshot). Mappings fixed to another page stay dark until that page is shown.
- No plugin connection: buttons go dark, motor faders stay. Unknown grandMA3 version (fallback profile): MIDI does not send.

## CDJ module (read-only)

Based on the [DJ Link Ecosystem Analysis](https://djl-analysis.deepsymmetry.org/). Code in `src/cdj/`: `packet` (parse and build), `tracker` (device list, tempo, beat and bar position, phrases), `sim` and `simdb` (fake network and fake track database), `dbserver` (track database protocol), `analysis` (track data formats), `metadata` (fetching and cache), `mod` (sockets, virtual player).

| Stage | What Boothmate does | What it gets |
| --- | --- | --- |
| 0 Discovery | Listens on UDP 50000 | All players and mixers (name, number, IP). Keep-alive every 1.5-2 s; a device counts as gone after 5 s |
| 1 Beat | Listens on UDP 50001 | Per beat: BPM, pitch, beat in bar. CDJ-3000: position every 30 ms. Mixer status |
| 2 Status | Announces itself as a silent virtual player | Status every ~200 ms on UDP 50002: playing, track ID and slot, beat number, master, sync, on-air, beats to next cue |
| 3 Track data | Asks the database of the player that holds the media about the loaded track (TCP, port via 12523) | Title, artist, key, beat grid, waveforms, cues, rekordbox phrases |

- Stages 0 and 1 are passive: Boothmate sends nothing.
- Stage 2: Boothmate sends a keep-alive (name "Boothmate", type player) every 1.5 s by unicast to each device it has seen (never to Pioneer Stagehand, which crashes on it). It uses device number 7 by default (`cdj.player`, 7-15): outside the player numbers 1-6, so a full set of four or six players is no problem and no CDJ is pushed off its number. If a device defends the number, Boothmate takes the next free one. It sends nothing else: no status, no claims, no queries.
- The network interface is the one the first device is seen on (its IP and MAC go into the keep-alive), or `cdj.interface`.
- Bars: from the beat number and beat-in-bar of the status packet (the grid may start on any beat of a bar). Without status, bars are counted from the first downbeat Boothmate sees and marked as counted. 4-bar and 8-bar (32-beat) groups are counted from the start of the beat grid; real rekordbox phrases need the track metadata (M5).
- Between status packets, each beat packet advances the beat number, so the bar position moves on the beat and not 200 ms later.
- Tempo master: the player whose status has the master flag. Without status, the player that sends beats stands in.
- The tracker emits events (device found/lost, master, track, play, beat with bar) for the log and, later, for grandMA3.

### Track data (M5)

When a player loads a track, Boothmate asks the database server ("dbserver") of the device that holds the media (the player itself, another player over Link, or rekordbox) about that one track. It never browses or loads anything. One request series per track, one at a time per player, retried after 10, 30 and 90 s; the last 12 tracks stay in memory.

| Request | Answer | Used for |
| --- | --- | --- |
| Metadata menu (0x2002) | Title, artist, album, key, genre, duration, BPM, rating, colour | Player cards, live view |
| Beat grid (0x2204) | Time of every beat at 0 % pitch, beat in bar | Exact playhead of NXS2 and older players (timecode), timeline grid |
| `PWV5` / `PWV4` tags (0x2c04) | Colour waveform detail (150 columns per second) and preview (1200 columns) | Timeline |
| Waveform detail and preview (0x2904, 0x2004) | Blue waveforms of older players | Fallback when there is no colour waveform |
| Extended cues (0x2b04), cues (0x2104) | Hot cues with colours and comments, memory points, loops | Timeline |
| `PSSI` tag (0x2c04) | rekordbox phrases (mood, kind, start beat, fill-in), unmasked when rekordbox 6 garbled them | Phrase now and next, 4/8-bar groups, timeline |

- The web UI gets a summary per player in the state (`meta`, `phrase`, `playhead_ms`) and the full data once per track from `/api/cdj/track/<key>` (waveforms as base64, four bytes per column: height, red, green, blue). The timeline is drawn on canvases and moves the playhead on between updates.
- Inside a rekordbox phrase, the 4- and 8-bar groups count from the phrase start, so "phrase" beat actions and the phrase FastSync of the tempo link follow the real song structure. Without phrase data they count from the start of the beat grid as before.
- The database only answers questions asked as a player number (1-4 on older players) that is on the network and is not the player being asked. Boothmate's virtual player is 7, so it borrows the number of another player, preferring one that has not loaded a track from that player over Link, and tries free numbers 1-4 last. Not yet verified on hardware.
- The simulator has its own database server (two tracks with colour waveforms, cues and phrases, one of them masked like rekordbox 6), reached over an in-memory stream, so the client code is the same.
- **Record** also writes each track data conversation to `logs/dbserver-<time>-<ip>.pcap`; `--check` decodes it.

### Tempo link (M4)

Rules in `tempo.toml` (data folder), edited in the web UI. Code in `src/tempo.rs`.

- **Speed masters** follow the effective BPM (pitch included) of the tempo master or a fixed player, times a factor (half or double time). Boothmate sends a new value only on a change of at least 0.05 BPM, at most 5 times a second, and holds the last value while the followed player is stopped. A master change on the CDJs moves the source with it. The plugin sets the tempo with `Master 3.n At BPM x`: exact to about 0.0003 BPM, no oops entries, microseconds per call ([spike](spikes/m4-speedmaster.md)).
- **Phase:** grandMA3 has no command that restarts a speed master's beat. `FastSync Master 3.n` re-syncs the phasers on that master but counts as a Learn tap (two taps 100 ms apart set 225 BPM). Option `phase = "phrase"` sends one FastSync on the first beat of each 8-bar phrase; the plugin rate-limits it and restores the exact BPM right after. Chases with cue timing from a speed master are not re-phased by it.
- **Beat actions** press an executor key (OSC input, 60 ms tap) on every beat, downbeat, 4 bars, phrase (8 bars), track change or master change, from the tempo master or a fixed player. The page is fixed or the current one.
- Nothing is sent without the plugin connection or with a fallback profile.

Known limits: track data queries borrow a player number (see Track data). Phrases only exist for tracks analysed in rekordbox. Opus Quad, XDJ-AZ and Denon StageLinQ are out of scope for now. rekordbox on the same machine holds UDP 50002, so status is not available next to it (beats still are, where the OS shares the ports). Keep-alive, beat, CDJ status and mixer status are checked against captures of nexus-generation hardware; the CDJ-3000 position packet and NXS2/3000 status are not verified on hardware yet.

## Testing without CDJs

1. Public captures: `scripts/check-captures.sh` runs 18 captures by Deep Symmetry (CDJ-2000nexus players, DJM-2000nexus mixer; startup, load and play, sync and master handoff) through the parser. Nine of them hold dbserver conversations between two players: every message must decode, and the metadata, beat grids, blue waveforms and cue lists in them are decoded like Boothmate does it. CI runs it on every push. The captures are downloaded, not stored in this repository. Colour waveforms, extended cues and phrases (nxs2 and CDJ-3000) are not in any public capture; they are checked against the documentation and the simulator only.
2. Recording in the club: the **Record** button writes all received and sent DJ Link packets to `logs/djlink-<time>.pcap` (raw IPv4, opens in Wireshark).
3. `boothmate --check <capture>...` prints what Boothmate understood (devices, packet types, BPM range, beat and bar consistency, master and track events) and fails if a packet it should read did not parse. Reads pcap and pcapng from Wireshark, tcpdump and Boothmate.
4. `boothmate --replay <capture>` plays a capture in real time into the web UI, as if the devices were on the network (loops).
5. Built-in simulator: the **Simulator** button in the web UI starts two fake players (a CDJ-2000NXS2 and a CDJ-3000) and a mixer. It builds real packets that go through the same parser; the scenario (play, cue, master handoff, pause) repeats every 64 s. Unit tests use it too, plus captured packets.
6. [CDJ_DJM_Emulator](https://github.com/OllieCross/CDJ_DJM_Emulator) and [cdj3k-emu](https://github.com/nsaintot/cdj3k-emu) (real firmware in QEMU) as further checks.

Checklist for the first test with real players: [club-checklist.md](club-checklist.md).

## Window

`src/window.rs` shows the web UI in a native window: tao for the window, wry for the system web view (WebKit on macOS, WebView2 on Windows), muda for the macOS menu bar (without an Edit menu, copy and paste do not work in WKWebView). No browser engine is bundled. The window owns the main thread, as macOS requires; the core (tokio runtime, web server) runs on a second thread and tells the window when the web UI listens or fails. The web server always runs, so a browser or tablet can connect at the same time. Linux builds and `--no-window` / `web.window = false` run without a window.

On Windows a double-click gives the program its own console; Boothmate releases it when the window opens (a short flash), but keeps a console it was started from, so logs and Ctrl+C still work in a terminal. Web view data goes to the temp folder, not onto the stick.

Later, a tray or menu bar icon and autostart hook into the same event loop: closing the window then hides it instead of quitting.

## Safety

- Only executor addresses are sent to grandMA3, never free commands. The Lua plugin accepts a fixed message set and a shared token.
- Default network binding is localhost. On a network: fixed console IP and sender checks.
- On connection loss controllers show "disconnected", motor faders stay put, and Boothmate resyncs on reconnect.
- Unknown grandMA3 version: warn and fall back to the closest profile in read-only mode.
- Logs go to `logs/` on the stick.

## Milestones

- **M0 Spikes (done):** `HookObjectChange` and native OSC feedback tested in onPC 2.5.1, see [spikes/m0-feedback.md](spikes/m0-feedback.md).
- **M1 Core (done):** Rust project, macOS + Windows builds, portable layout, OSC + Lua connection, MA profile 2.5, small web UI.
- **M2 MIDI mapper (done):** device discovery and profiles, MIDI learn, feedback, page follow, two reference profiles, test controller in the web UI.
- **M3 CDJ discovery, beat and status (done):** automatic discovery of CDJs and mixers, BPM and beat in bar, CDJ-3000 position, silent virtual player for status (play state, track, beat number, master, on-air), bar and 4/8-bar counting, simulator, DJ Link in the web UI.
- **M4 Tempo link (done):** speed masters follow the CDJ tempo through the plugin, phaser sync per phrase, beat, bar, phrase, track and master actions on executor keys.
- **M5 Track data and timeline (in review):** metadata, beat grid, colour waveforms, cues and rekordbox phrases from the players' databases; timeline with waveform in the live view and the player cards; phrase-aware 8-bar groups; exact NXS2 timecode from the beat grid.

## Licensing notes

Boothmate is GPL-3.0-or-later. Compatible sources: MIT/Apache crates, rekordcrate (MPL-2.0), alphatheta-connect (MIT), GPL-3.0 projects such as pam-osc. beat-link and LJ-Link (EPL-2.0) are not used; the Pro DJ Link code is written from the Deep Symmetry protocol documentation.
