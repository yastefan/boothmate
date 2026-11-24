# M0 spikes: push feedback from grandMA3

Date: 2026-11-24 · grandMA3 onPC 2.5.1.1 on macOS · throwaway show `MCP-Test` · probes run through the gma3-mcp Lua bridge (0.58.0) plus manual input in onPC.

**Result:** feedback can be push-based. OSC output covers playback state and fader values. `HookObjectChange` covers names, appearances and page changes. Only the executor-to-object mapping and the start state need a read on connect (and after a page or assignment change).

## Spike 2: native OSC output ("Send = Yes")

Setup: OSC line with `Send = Yes`, destination `127.0.0.1:9001`, UDP, Enable Output on. Captured with a small UDP listener.

### What arrives

| Event | Address | Types | Arguments (example) |
| --- | --- | --- | --- |
| Key press on executor or `Go+` command | `/14.14.1.7.<seq>` | `sis` | `Go+`, `1`, `MCP Test;2 Rot` |
| Key release | same | `sis` | `Go+`, `0`, `MCP Test;1 Warm` |
| `Go-`, `Goto`, `On` | same | `sis` | `Go-`, `1`, `MCP Test;1 Warm` |
| `Off` | same | `sis` | `Off`, `1`, `MCP Test` (no cue) |
| `Pause` | same | `sis` | `Pause`, `2`, `MCP Test` |
| Follow / time-triggered cue | same | `sis` | `Go+`, `1`, `MCP Test;2 Rot` (press only, no release) |
| Fader by command or `SetFader` | same | `sifs` | `FaderMaster`, `1`, `42.0`, `MCP Test` |
| Fader moved with the mouse on screen | same | `sifs` | `FaderMaster`, `3`, `45.97`, `MCP Test` |
| Grand Master (`Master 2.1`) | `/14.13.2.1` | `sif` | `FaderMaster`, `1`, `80.0` |
| Group master (`Group 1`) | `/14.14.1.6.1` | `sif` | `FaderMaster`, `1`, `60.0` |

Details:

- **The address is the object, not the executor.** `14.14.1.7.<n>` is `ShowData.DataPools.Default.Sequences.<n>`; the last part is the pool number (checked with Sequence 10 next to Sequences 1 and 2). Boothmate must map object to executor itself (from the Lua plugin).
- **Addresses differ from the manual.** The 2.5.1 manual still shows `/13.13.1.6.X` for sequences and `/13.12.X.Y` for masters. The real prefix depends on the version. This goes into `ma-profiles/<version>.toml`, or the Lua plugin reports `obj:Addr()` on connect.
- **Argument 2:** for keys `1` = pressed, `0` = released (`Pause` sent `2`). For faders it is the source: `1` = command or Lua, `3` = fader handle (the mouse on the onPC fader also counts as `3`).
- **Argument 3/4** carries the sequence name and, for key events, `<cue number> <cue name>` after a `;`. That is enough to show the current cue without polling.
- **Fader stream:** about 30 messages per second while a fader moves, one per frame (~33 ms), only on change. Good for motor faders.
- **Fades are not streamed.** `FaderMaster ... At 70 Fade 1` sent the target value (twice), no intermediate values.
- **Not sent:** page changes, labels, appearances, assignment changes. `Send Command = Yes` sent nothing for commands from Lua `Cmd()`.
- The FaderTemp and FaderX command tests were not meaningful: on an executor with `Fader = Master` they moved the master. Other fader functions are still to be checked on an executor that has them assigned.

## Spike 1: Lua `HookObjectChange`

Hooks were registered from the bridge plugin (`HookObjectChange(fn, obj, pluginObject)`, one closure per hook) on Sequence 2, its cue 2, executor `Page 1.202`, Page 1, the Pages pool, Grand Master, Group 1, the Appearances pool, the user, the user profile and its children, and the current environment with two levels of children (about 100 hooks).

| Action | Hook fired? |
| --- | --- |
| `Go+`, `Off` on the sequence | no |
| Fader master (sequence, group, grand) by command | no |
| `Label Sequence 2` | yes, sequence: change `13` |
| `Label Page 1.202` (renames the sequence) | yes, executor and sequence: change `11` |
| `Assign Appearance 1 At Sequence 2` | yes, sequence: change `11` |
| `Page 2` / `Page 1` | yes, `UserProfiles.Default.Environments.Normal.ObjectSelection`: change `11` (read `CurrentExecPage()` in the callback) |
| (idle) | `UserProfiles.Default.StatusCenter` fires about once a second: do not hook it |

Callbacks run while the plugin loop yields. Hooks are not recursive (known from gma3-mcp probes): hook each object Boothmate shows, plus the pools for added or deleted objects.

## Consequences for Boothmate

| Feedback | Source | Push? |
| --- | --- | --- |
| Button state (running, current cue) | OSC key events, `Off` | yes |
| Motor fader / LED ring | OSC `FaderMaster` stream | yes (fades: target only) |
| Button colour | Lua hook on the assigned object (appearance) | yes |
| Display names | Lua hook (label) | yes |
| Page follow | Lua hook on `Environments.<env>.ObjectSelection` | yes |
| Executor assignment, start state | Lua read on connect, page change, pool change | read on event |

Open points for M1:

- Sequence playing state after a time-based fade to 0 or an automatic off (no OSC event seen yet).
- Executors with other fader functions (Temp, X, Rate, Speed) and other object types (presets, timers).
- Effect of a real console or MA hardware (`3` = physical fader is expected there as well).
- Whether the hook on `ObjectSelection` also fires for other selection changes (filter: compare `CurrentExecPage()` with the last page).
