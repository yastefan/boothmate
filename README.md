# Boothmate

A portable companion for grandMA3. Boothmate maps MIDI controllers to executors, sends feedback back to them, and reads the beat and song structure from Pioneer CDJs. It runs without installation from the same USB stick as your show files.

> **Status:** milestone M1. Boothmate connects to grandMA3 and shows executors, cues and fader values live in a web page. MIDI and CDJ support come next. See [docs/architecture.md](docs/architecture.md).

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
2. In grandMA3, add an OSC line: destination `127.0.0.1`, port `9001`, UDP, **Send = Yes**, and switch on **Enable Output** (Menu > In & Out > OSC).
3. Import the plugin from `lua/boothmate.xml` into your show and start it with `Plugin "boothmate"`.
4. Start `boothmate-mac` or `boothmate-win.exe` and open <http://127.0.0.1:8088/>.

grandMA3 itself binds the OSC line's port. Boothmate shares it on `127.0.0.1` (address reuse); if it cannot, the web page shows the error and Boothmate keeps retrying. In that case switch Enable Output off, wait until Boothmate listens, and switch it on again.

On macOS, allow the unsigned binary once: right-click > Open, or `xattr -d com.apple.quarantine boothmate-mac`.

Boothmate on another machine than grandMA3: point the OSC line at that machine, set `osc.listen = "0.0.0.0:9001"` and `plugin.host` (the console IP) and `plugin.token` in `boothmate.toml`, and start the plugin with `Plugin "boothmate" "bind=0.0.0.0 token=<same token>"`.

```
Boothmate/
  boothmate-mac, boothmate-win.exe
  boothmate.toml        settings, created on first start
  ma-profiles/          grandMA3 version profiles (OSC addresses)
  lua/                  grandMA3 plugin
  logs/                 log of the last two runs
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
