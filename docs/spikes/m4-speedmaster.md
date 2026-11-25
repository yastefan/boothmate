# M4 spike: driving a speed master from an external BPM

Date: 2026-11-25 · grandMA3 onPC 2.5.1.1 on macOS · throwaway show `MCP-Test` (not saved) · probes run through the gma3-mcp Lua bridge (0.58.0, Lua on).

**Result:** a speed master can be set exactly from Lua. Use `SetFader` with a fader value computed from the BPM, or `Cmd('Master 3.<n> At BPM <x>')`. Both keep decimals to about 0.0003 BPM and cost microseconds. Neither lands in the Oops list. Calling it 5 times per second is harmless. Re-aligning the beat phase has no exact handle. `FastSync` / `ReSync` / `Learn Master 3.n` are accepted (the manual says they sync phasers on that master), but they cannot be measured without DMX output, and they do not touch time-triggered chases.

## 1. Master 3.1 object

`ShowData.Masters.Speed[1]`, class `MasterSpeed`, address `14.13.3.1`. Writable properties of interest (all from `Dump()`):

| Property | Value (example) | Note |
| --- | --- | --- |
| `NORMEDVALUE` | `64.57` | fader position 0..100. Schema type Int32: `:Get('NormedValue')` returns `74` (truncated). Read the exact value with `GetFader`. |
| `SPEEDSCALE` | `One` | `Div256..Div2, One, Mul2..Mul256` |
| `LEARNMODE` | `Learn Ignores Speed Scale` | also `LEARNRESPECTSSPEEDSCALE`, `AUTOINCREASESPEEDSCALE` (Yes/No) |
| `FADERENABLE` | `AlwaysOn` | |
| `LOCK`, `NAME`, `NOTE`, `APPEARANCE` | | |

There is no BPM, Rate, Phase or Learn property. The tempo is only the fader position (times the scale).

- `obj:GetFader({})` returns the full float, e.g. `74.393051862717`.
- `obj:GetFaderText({})` returns `"128 B"`. It is rounded to whole BPM (one decimal below 10 BPM, e.g. `"0.5 B"`). With a scale other than One it shows the effective BPM and a star, e.g. `"300 *B"`. `"Stop"` at 0.

**Fader to BPM (fits every point to < 0.0004 BPM):**

```
BPM   = 225 * (fader/100) ^ log2(225/60)      -- exponent 1.906890...
fader = 100 * (BPM/225) ^ (1/log2(225/60))
effective BPM = BPM * SpeedScale factor
```

Fader 50 = 60 BPM, 100 = 225 BPM (the maximum per scale), 0 = Stop. Measured pairs: 60 → 49.99997, 120 → 71.91742, 128 → 74.39305, 174.3 → 87.46821, 0.5 → 4.06070, 1 → 5.84071.

## 2. Setting an exact BPM

| Method | Result |
| --- | --- |
| `Master 3.1 At BPM 128` | works. 128 → fader 74.393052 (= 127.99966 BPM) |
| `Master 3.1 At BPM 127.5` / `174.3` / `128.37` | works. Read back 127.49968 / 174.29995 / 128.36980 BPM |
| `Master 3."Speed1" At BPM 128` | works |
| `Master 3.1 At Hz 2.9` / `At Seconds 0.5` | works (174 / 120 BPM) |
| `Master 3.1 At BPM 300` | clamped to 225 (fader 100). `At BPM 0` → Stop |
| with `SpeedScale Mul2`: `At BPM 300` | fader 80.845 (base 150), text `300 *B`. `At BPM` is the **effective** BPM |
| `Master 3.1 At 128` | **wrong**: that is the fader in percent, clamped to 100 → 225 BPM |
| `SpeedMaster 1 At 128` / `SpeedMaster 1 At BPM 128` | OK, but no effect on the master (`SpeedMaster` is the phaser layer keyword) |
| Lua `obj:SetFader({value = f})` | works, float kept exactly (74.24052357673645 → 127.5 BPM) |
| Lua `obj:Set('NormedValue', f)` | works (fader 87.468206882477 kept exactly). An immediate `Get` still shows the old value for a frame. |
| Lua `obj.NormedValue = 128` | means fader %, clamped to 100 |

Resolution: the fader is a float, so any BPM with 2 decimals is reproduced to about 0.0003 BPM. Minimum seen: 0.5 BPM (no lower bound tested). Maximum: 225 × SpeedScale.

Calls from Lua (`Cmd`, `SetFader`, `Set`) are applied when the Lua chunk yields or returns, not at once. `GetFader` in the same chunk still returns the old value, and the last write of a chunk wins.

## 3. Phase

What could be observed: with no DMX granted on onPC, phaser **output** cannot be read (no Lua API gives a phaser's position or the master's beat clock). So a time-triggered chase was measured instead: Sequence 20, 4 cues at TrigTime 1 s, Wrap Around, **RateMaster = Speed1**. `Sequence.CurrentCue` was sampled from Lua every ~20 ms, and each command was sent 0.5 s into a step.

| Command 0.5 s into a 1 s step (60 BPM) | Effect on the chase |
| --- | --- |
| `FastSync Master 3.1` | none (steps stay on the 1.000 s grid) |
| `ReSync Master 3.1` | none |
| `Learn Master 3.1` (single tap, long gap before) | none, and the tempo stays at 60 BPM |
| `FastSync Sequence 20` | no lasting shift (one sample at +0.58 s, then back on the old grid) |
| `Master 3.1 At BPM 60` (same value) | none: setting the BPM does **not** restart anything |
| `Master 3.1 At BPM 120` (from 60) | the rest of the step is rescaled: next step +0.247 s (remaining 0.49 s / 2), then 0.5 s steps. Progress is kept, no restart. |

Learn / tap behaviour:

- **A single `Learn` tap does not change the tempo** when the previous tap is seconds ago.
- **`FastSync`/`ReSync`/`Learn` all count as taps.** A `Learn` sent ~100 ms after a `ReSync` set the master to 225 BPM (the clamp).
- Two `Learn` taps from Lua 479 ms apart (yield timing) gave 124.22 BPM (125.1 expected). Two taps over the bridge (468.75 ms requested, 452 ms sent) gave 137.6 BPM. **Tap tempo is ±1–8 %**: fine for phase, useless for an exact tempo.

What the manual says about phasers (2.3 release notes, `Learn`/`FastSync`/`ReSync` pages): speed masters have a common timeline. Phasers that follow a speed master (attribute layer, or a cue part with Sync on) are synced to it. "To sync phasers across sequences, set them to the same speed master and apply the keywords to the speed master." `FastSync` sets the start point at once, `ReSync` moves it gradually, and `Learn` = `ReSync` + `LearnSpeed`. All three commands are accepted for `Master 3.1`, but their effect on phasers is **not verified** here. That needs a visual check (a phaser on a dimmer with Speed1, `FastSync Master 3.1` on a downbeat).

## 4. OSC output

Not tested. Boothmate was not running at that point, and a separate UDP listener on 9001 was not possible. OSC line 2 "Boothmate spike" (Send to 127.0.0.1:9001, Send Command on) is configured. By analogy with M0 (Grand Master `/14.13.2.1 sif FaderMaster 1 80.0`), expect `/14.13.3.1` with `FaderMaster`, source, fader value (not BPM). Still to be confirmed.

## 5. Lua timing, load and Oops

- Cost per call inside Lua (20 calls each): `Cmd('Master 3.1 At BPM x')` 0.04 ms, `SetFader` 0.005 ms, `Set('NormedValue')` 0.012 ms. These are queue costs; the change is applied on the next frame.
- 5 Hz over the bridge for 20 s (50 × `cmd`, 50 × `setfader`): no failures. Round trip median 17 / 12 ms, p95 57 / 66 ms, max 84 ms. onPC CPU went from ~34 % to ~45–55 % (one sample at 71 %). No visible stalls in 20 s (not checked on screen). Most of that load is the bridge round trip; in the Boothmate plugin a set is one local call.
- **Oops list** (`Root.Temp.Cmdlines["Cmdline 1"].Undos`): `Master … At BPM`, `SetFader`, `Set`, `Learn`, `FastSync`, `ReSync` added **no** entries (16 before, 16 after 100+ sets). A `Label Sequence 20 …` from the same path added one (17). So the speed master writes do not flood Oops.

## Recommendation for Boothmate

1. Drive the tempo from the Lua plugin. `Cmd('Master 3.n At BPM x')` is exact and handles the speed scale itself (it sets the effective BPM); `SetFader` with `fader = 100 * (bpm / 225) ^ (1 / log2(3.75))` is the alternative. Never use `Master 3.n At x` without `BPM`. Above 225 BPM the master needs `SpeedScale Mul2`.
2. Send only on change, at most ~5–10 Hz. Show the BPM from the fader formula, not from `GetFaderText` (rounded).
3. Changing the BPM keeps progress: chases rescale and nothing restarts, so continuous small corrections are safe.
4. For phase: send **one** `FastSync Master 3.n` on the external downbeat, rarely. Never several quickly, and never next to `Learn` within ~2 s, because each counts as a tap and can jump the tempo up to 225. Do not use `Learn` taps to set the tempo. This only affects phasers on that speed master. Time-triggered chases (rate master) keep their own timer and can only be re-phased by restarting the cue (`Goto`).
5. Open: confirm on screen that `FastSync Master 3.n` snaps a running phaser to step 1 at once (and `ReSync` gradually), and capture the OSC message for `14.13.3.n` with Boothmate running.

**Implemented in M4:** `Master 3.n At BPM x` on change (≥ 0.05 BPM, ≤ 5 Hz); optional `FastSync` once per 8-bar phrase, rate-limited to one per 4 s in the plugin and followed by `At BPM` with the exact value again.
