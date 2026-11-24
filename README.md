# Boothmate

A portable companion for grandMA3. Boothmate maps MIDI controllers to executors, sends feedback back to them, and reads the beat and song structure from Pioneer CDJs. It runs without installation from the same USB stick as your show files.

> **Status:** early planning. Nothing works yet. See [docs/architecture.md](docs/architecture.md).

## Why

Getting a MIDI controller onto grandMA3 executors is easy. Getting proper feedback back (button colours, motor fader positions, encoder LED rings) is not. grandMA3's built-in MIDI remotes only receive, and the existing tools either poll the console all the time or need a lot of manual mapping. DJ shows add a second problem: the light desk needs to follow the beat and know when the drop is coming.

Boothmate wants to solve both in one small, robust tool:

- **MIDI learn:** move a control, press an executor, done. Feedback is set up automatically.
- **Feedback** for colours, faders and encoders, using push events where grandMA3 provides them.
- **Device profiles:** controllers are described in files, not hard-coded.
- **CDJ link (read-only):** BPM, beats, bars, phrases ("drop in 16 beats") and a track timeline via Pro DJ Link.
- **Portable:** one binary per platform (macOS, Windows), all data in the folder next to it.
- **Version-proof:** everything specific to a grandMA3 version lives in profile files you can update without a rebuild.

## Usage (planned)

1. Copy the `Boothmate` folder next to your show files on a USB stick.
2. Import the Lua plugin from `lua/` into your grandMA3 show.
3. Start `boothmate-mac` or `boothmate-win.exe` and open the web UI it prints.
4. Pick your controller profile, press **Learn**, and map your controls.

```
Boothmate/
  boothmate-mac, boothmate-win.exe
  boothmate.toml        global settings (network, console IP)
  devices/              controller profiles
  ma-profiles/          grandMA3 version profiles
  lua/                  grandMA3 plugin
  shows/<name>/         mappings and CDJ rules per show
```

## Building

```sh
cargo build --release
```

## Credits

Pro DJ Link support is written from the protocol analysis by [Deep Symmetry](https://djl-analysis.deepsymmetry.org/). Thanks to the authors of [alphatheta-connect](https://github.com/chrisle/alphatheta-connect), [rekordcrate](https://github.com/Holzhaus/rekordcrate) and [pam-osc](https://github.com/xxpasixx/pam-osc) for their work.

grandMA3 is a trademark of MA Lighting. Pioneer DJ, CDJ and rekordbox are trademarks of AlphaTheta. Boothmate is not affiliated with either company.

## License

[GPL-3.0-or-later](LICENSE)
