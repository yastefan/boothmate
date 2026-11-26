# Club checklist: first test with real CDJs

Goal: in about 30 minutes, find out whether Boothmate reads CDJ-2000NXS2 and CDJ-3000 correctly, and take recordings home so anything odd can be fixed without the hardware.

## Before the club

- [ ] Latest build on the laptop (`scripts/try.sh` or a release), web page opens at <http://127.0.0.1:8088/>.
- [ ] Simulator works (button **Simulator** in the DJ Link section), then switch it off.
- [ ] macOS: start Boothmate once at home; if macOS asks to allow incoming connections or access to the local network, allow it. Windows: allow it in the firewall prompt (private and public networks).
- [ ] rekordbox is closed (it holds the DJ Link ports).
- [ ] Ethernet adapter and a cable. Write down the player models and firmware (CDJ: Menu > Utility > Version).

## Connect

- [ ] Plug the laptop into the DJ Link switch or a free LAN port of the mixer. Wi-Fi off, so the right interface is used.
- [ ] The header shows **DJ Link: N devices**. Every player and the mixer has a card with the right number and name.
- [ ] The DJ Link line shows **Network: <interface> <IP>** and **Status via virtual player 7**.
- [ ] Press **Record** now and leave it on for the whole test. It writes `logs/djlink-<date>-<time>.pcap`.

If nothing shows up: check the cable and link light, and that the laptop has an IP in the same range as the players (often `169.254.x.x` with no DHCP). If the header says **port busy**, quit rekordbox and other DJ software.

## Test sequence (note the times)

Per player model (NXS2 and 3000):

1. [ ] Load a rekordbox-analysed track. The card shows the track (`USB #<id>`), state **cued**, and the BPM.
2. [ ] Play. The card turns green, the beat dots move on the beat, beat 1 is yellow, the bar counts up, and **beat** counts up. CDJ-3000: the progress bar and time move.
3. [ ] Move the pitch fader. BPM and pitch on the card follow, within about 0.2 s.
4. [ ] Pause, cue, play again, then a hot cue jump. The bar and beat number jump with it.
5. [ ] Press **Master** on the other player. The yellow master frame and the "Tempo master" line follow. Then make the mixer master, if it can.
6. [ ] Sync on and off on a player: the **Sync** chip follows.
7. [ ] Channel fader up and down: **On air** follows (needs the players on mixer channels with on-air display).
8. [ ] Load an unanalysed track, if there is one: BPM may be missing, the card must not show nonsense.
9. [ ] Track data: within a few seconds of loading, the card shows title, artist and key, and the timeline shows the waveform, beat grid, hot cues and (for tracks with phrase analysis in rekordbox 6+) the coloured phrases. The log says `track data for ... from player N`. If the card says **No track data**, note the message.
10. [ ] Track data over Link: load a track from the other player's USB. The data still comes (from the player with the USB).
11. [ ] Play an NXS2: the timeline's playhead moves smoothly and sits on the beat lines; the phrase box ("Chorus 1 in 12 beats") counts down with the player's own phrase display (CDJ-3000).
12. [ ] Unplug a player's network cable: its card disappears after about 5 s. Plug it back in: it comes back.

Also:

- [ ] Four or more players: everything still works, and no player shows a number conflict or drops off the link while Boothmate is running. Track data with four players: Boothmate asks as another player's number; check that the players' own Link browsing still works while it loads.
- [ ] The DJs notice nothing: no player shows a strange device, no link errors.

## After

- [ ] Stop recording. Copy `logs/djlink-*.pcap`, `logs/dbserver-*.pcap` (track data queries) and `logs/boothmate.log` home, with your notes (times, what was pressed, what looked wrong).
- [ ] At home: `boothmate --check logs/djlink-*.pcap` prints what Boothmate understood; `boothmate --replay logs/djlink-<...>.pcap` plays the recording into the web page as if the CDJs were there.
