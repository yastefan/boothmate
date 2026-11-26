# Boothmate

A portable companion for grandMA3. Boothmate maps MIDI controllers to executors, sends feedback back to them, and reads the beat and song structure from Pioneer CDJs. It runs without installation from the same USB stick as your show files.

> **Status:** milestone M4. Boothmate connects to grandMA3, shows executors live in a web page, maps MIDI controllers to executors with feedback, reads tempo, beats and bars from Pioneer CDJs, and drives speed masters and executors from them. See [docs/architecture.md](docs/architecture.md).

## Why

Getting a MIDI controller onto grandMA3 executors is easy. Getting proper feedback back (button colours, motor fader positions, encoder LED rings) is not. grandMA3's built-in MIDI remotes only receive, and the existing tools either poll the console all the time or need a lot of manual mapping. DJ shows add a second problem: the light desk needs to follow the beat and know when the drop is coming.

Boothmate wants to solve both in one small, robust tool:

- **MIDI learn:** move a control, press an executor, done. Feedback is set up automatically.
- **Feedback** for colours, faders and encoders, using push events where grandMA3 provides them.
- **Device profiles:** controllers are described in files, not hard-coded.
- **CDJ link (read-only):** BPM, beats, bars, phrases ("drop in 16 beats") and a track timeline via Pro DJ Link.
- **Portable:** one binary per platform (macOS, Windows), all data in the folder next to it.
- **Version-proof:** everything specific to a grandMA3 version lives in profile files you can update without a rebuild.

## Usage

1. Download `Boothmate-mac.zip` or `Boothmate-win.zip` from the [build runs](https://github.com/yastefan/boothmate/actions/workflows/build.yml) or releases and unpack the `Boothmate` folder, for example next to your show files on a USB stick.
2. In grandMA3 (Menu > In & Out > OSC), add two OSC lines, both UDP with destination `127.0.0.1`, and switch on **Enable Output** and **Enable Input**:
   - port `9001`, **Send = Yes** (feedback to Boothmate),
   - port `8000`, **Receive = Yes** (keys and faders from Boothmate).
3. Import the plugin from `lua/boothmate.xml` into your show and start it with `Plugin "boothmate"`.
4. Start `boothmate-mac` or `boothmate-win.exe` and open <http://127.0.0.1:8088/>.

The web page opens on **Status**: one tile per connection (grandMA3, OSC, MIDI, DJ Link, tempo and timecode) that turns yellow or red and says what to do, plus the recent events. **Live** shows the tempo master large, for a tablet or second screen. The other tabs hold the details: CDJs, MIDI & Mapping, Test controller, grandMA3 and Log.

## MIDI controllers

Plug in the controller, then press **Learn** in the MIDI & Mapping tab, move a control, and click an executor (or press its key in grandMA3). Repeat for more controls, then stop learning. Buttons light up in the executor's appearance colour (bright = running, dim = assigned), motor faders and LED rings follow the fader. Mappings follow the current page unless you set a fixed page; they are saved in `midi.toml`.

Controllers with a profile in `devices/` get full feedback: Akai APC mini mk2 and Mackie Control devices (Behringer X-Touch in MC mode, iCON, ...). Other controllers work too, with simple LED and fader feedback. No controller at hand? Use the Test controller tab. On macOS, Boothmate also offers a virtual MIDI port "Boothmate" for software controllers.

## CDJs (Pro DJ Link)

Connect the computer to the DJ Link network (the switch, or a LAN port of the mixer). Boothmate finds players and mixers by itself and shows them in the CDJs tab: BPM, pitch, beat in bar, bar and 4/8-bar position, play state, tempo master, on-air and the loaded track. It only reads: it never browses media, loads tracks or controls a player.

To get play state and beat numbers, Boothmate announces itself as a silent virtual player with device number 7 (`cdj.player` in `boothmate.toml`), so it never takes a player's number. Without this (`cdj.status = false`), it still reads BPM and beats. rekordbox on the same computer blocks the status port. No CDJs at hand? Press **Simulator** in the CDJs tab.

**Tempo to grandMA3** (in the CDJs tab): link speed masters to the CDJs. Speed master 3.1 then follows the tempo master's BPM exactly, also when the master moves to another player, optionally at half or double time and with a phaser sync at the start of each phrase. Beat actions press an executor on every beat, bar, 4 bars, phrase, track change or master change. Needs the plugin (version 0.2.0 or later: re-import `lua/boothmate.xml`). Saved in `tempo.toml`.

**Timecode (experimental):** the **Timecode** button in the CDJs tab sends the playhead of the CDJs as Art-Net timecode (ArtTimeCode) to grandMA3: stream 0 follows the tempo master, stream n player n, only while playing. In grandMA3, set a timecode slot to Art-Net input and point a timecode show at that slot. CDJ-3000s send their exact position; older players are estimated from the beat number (shown with `~`). Target and frame rate are in `boothmate.toml` under `[timecode]`.

**Record** in the CDJs tab saves the DJ Link traffic to `logs/djlink-*.pcap`. `boothmate --replay <file>` plays such a recording back as if the players were there, and `boothmate --check <file>` shows what Boothmate understood. Before a first test with real players, see [docs/club-checklist.md](docs/club-checklist.md).

## Notes

grandMA3 itself binds the OSC line's port. Boothmate shares it on `127.0.0.1` (address reuse); if it cannot, the web page shows the error and Boothmate keeps retrying. In that case switch Enable Output off, wait until Boothmate listens, and switch it on again.

On macOS, allow the unsigned binary once: right-click > Open, or `xattr -d com.apple.quarantine boothmate-mac`.

Boothmate on another machine than grandMA3: point the OSC lines at that machine, set `osc.send` to the console IP and port, set `osc.listen = "0.0.0.0:9001"` and `plugin.host` (the console IP) and `plugin.token` in `boothmate.toml`, and start the plugin with `Plugin "boothmate" "bind=0.0.0.0 token=<same token>"`.

```
Boothmate/
  boothmate-mac, boothmate-win.exe
  boothmate.toml        settings, created on first start
  midi.toml             MIDI mappings
  tempo.toml            tempo link (speed masters, beat actions)
  ma-profiles/          grandMA3 version profiles (OSC addresses)
  devices/              MIDI controller profiles
  lua/                  grandMA3 plugin
  logs/                 log of the last two runs, DJ Link recordings
```

## Trying a branch (macOS)

`scripts/try.sh` downloads the CI build of `main`, a branch or a pull request and starts it, no Rust needed (only the gh CLI): `scripts/try.sh 2` runs pull request #2. Put your `boothmate.toml` in `~/Boothmate-try` to keep your settings.

## Building

```sh
cargo build --release
cargo run -- --verbose            # uses ma-profiles/ in the current folder
lua5.4 tools/fake-ma3.lua         # fake grandMA3 (needs LuaSocket + dkjson), no console needed
```

## Credits

Pro DJ Link support is written from the protocol analysis by [Deep Symmetry](https://djl-analysis.deepsymmetry.org/). Thanks to the authors of [alphatheta-connect](https://github.com/chrisle/alphatheta-connect), [rekordcrate](https://github.com/Holzhaus/rekordcrate) and [pam-osc](https://github.com/xxpasixx/pam-osc) for their work.

grandMA3 is a trademark of MA Lighting. Pioneer DJ, CDJ and rekordbox are trademarks of AlphaTheta. Boothmate is not affiliated with either company.

## License

[GPL-3.0-or-later](LICENSE)
