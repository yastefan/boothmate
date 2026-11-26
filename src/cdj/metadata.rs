//! Fetches the data of loaded tracks (metadata, beat grid, waveforms, cues, phrases) from the
//! player that holds the media, or from the simulator, and keeps the last few in memory.
//!
//! Runs inside the DJ Link module, so switching DJ Link off stops it. Only tracks that are
//! loaded on a player are asked for, one request series per track and one at a time per
//! player. Nothing is browsed and nothing is written.

use super::analysis::{self, TrackData};
use super::dbserver::{self as db, Client, Field};
use super::packet::Kind;
use super::simdb;
use super::tracker::{CdjState, Device};
use crate::state::now_ms;
use crate::App;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task::JoinSet;

/// Wait for each answer.
const WAIT: Duration = Duration::from_secs(4);
/// Give up on a track after this long.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Retry a failed track after 10 s, 30 s, 90 s, then not again until it is loaded anew.
const RETRY_MS: [u64; 3] = [10_000, 30_000, 90_000];
/// Tracks kept in memory.
const KEEP: usize = 12;

/// The last fetched tracks, for the web UI (`/api/cdj/track/<key>`).
#[derive(Default)]
pub struct Store(Mutex<VecDeque<Arc<TrackData>>>);

impl Store {
    pub fn get(&self, key: &str) -> Option<Arc<TrackData>> {
        let mut v = self.0.lock().unwrap();
        let i = v.iter().position(|t| t.key == key)?;
        let t = v.remove(i)?;
        v.push_front(t.clone());
        Some(t)
    }

    fn put(&self, t: Arc<TrackData>) {
        let mut v = self.0.lock().unwrap();
        v.retain(|x| x.key != t.key);
        v.push_front(t);
        v.truncate(KEEP);
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// Where a loaded track's data comes from.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub key: String,
    /// The simulator's database instead of the network.
    pub sim: bool,
    pub ip: Option<Ipv4Addr>,
    /// Player (or rekordbox) that holds the media.
    pub owner: u8,
    pub slot: u8,
    pub kind: u8,
    pub id: u32,
    /// Device numbers to ask as, best first.
    pub ask: Vec<u8>,
    /// Record the conversation into this folder (Record button).
    pub record: Option<std::path::PathBuf>,
}

/// Track types that have data on the database server: rekordbox, unanalysed (CDJ-3000),
/// streaming.
fn has_data(kind: u8) -> bool {
    matches!(kind, 1 | 2 | 6)
}

/// The source for the track on player `d`, or why there is none.
pub fn source(cdj: &CdjState, d: &Device) -> Result<Source, &'static str> {
    let t = d.track.as_ref().ok_or("no track")?;
    if !has_data(t.type_id) || t.id == 0 {
        return Err("no track data for this kind of track");
    }
    let owner = cdj
        .devices
        .get(&t.player)
        .ok_or("the player with the media is not seen")?;
    let ip = owner.ip;
    let sim = owner.simulated && cdj.simulator && ip.is_some_and(|i| i.is_loopback());
    if owner.simulated && !sim {
        return Err("no track data in a replay");
    }
    let src = if sim {
        "sim".to_string()
    } else {
        ip.ok_or("no IP address for the player with the media")?
            .to_string()
    };
    Ok(Source {
        key: format!("{src}-{}-{}-{}-{}", t.player, t.slot_id, t.type_id, t.id),
        sim,
        ip,
        owner: t.player,
        slot: t.slot_id,
        kind: t.type_id,
        id: t.id,
        ask: ask_numbers(cdj, t.player),
        record: None,
    })
}

/// Device numbers to ask the owner's database as. It has to be a player number (1-4 on older
/// players) that is on the network and is not the owner itself. Boothmate's own virtual player
/// (7 and up) is outside that range, so it borrows the number of another player, preferring
/// one that has not loaded a track from the owner over Link. Free numbers come last.
pub fn ask_numbers(cdj: &CdjState, owner: u8) -> Vec<u8> {
    let players: Vec<&Device> = cdj
        .devices
        .values()
        .filter(|d| {
            !d.simulated
                && (1..=6).contains(&d.number)
                && d.number != owner
                && d.kind != Some(Kind::Mixer)
        })
        .collect();
    let linked = |d: &&Device| d.track.as_ref().is_some_and(|t| t.player == owner);
    let mut v: Vec<u8> = vec![];
    v.extend(players.iter().filter(|d| !linked(d)).map(|d| d.number));
    v.extend(players.iter().filter(|d| linked(d)).map(|d| d.number));
    v.sort_by_key(|&n| n > 4);
    v.extend((1..=4).filter(|n| *n != owner && !cdj.devices.contains_key(n)));
    v.dedup();
    v
}

/// Fetches everything there is about a track.
pub async fn fetch(src: Source) -> Result<TrackData, String> {
    let r = tokio::time::timeout(FETCH_TIMEOUT, async {
        if src.sim {
            let (a, b) = tokio::io::duplex(1 << 16);
            let (_, r) = tokio::join!(simdb::serve(b, src.owner), async {
                let mut c = Client::open(a, 1, WAIT, false)
                    .await
                    .map_err(|e| e.to_string())?;
                query(&mut c, &src).await
            });
            return r;
        }
        let ip = src.ip.ok_or("no IP address")?;
        let port = db::db_port(ip, WAIT)
            .await
            .map_err(|e| format!("{ip}: no database port ({e})"))?;
        let mut err = String::from("no player number to ask as");
        for &d in src.ask.iter().take(3) {
            let s = match tokio::time::timeout(WAIT, tokio::net::TcpStream::connect((ip, port)))
                .await
            {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return Err(format!("{ip}:{port}: {e}")),
                Err(_) => return Err(format!("{ip}:{port}: no answer")),
            };
            match Client::open(s, d, WAIT, src.record.is_some()).await {
                Ok(mut c) => {
                    let r = query(&mut c, &src).await;
                    if let (Some(dir), Some(tap)) = (&src.record, c.tap.take()) {
                        save_recording(dir, ip, port, &tap);
                    }
                    return r;
                }
                Err(e) => err = format!("{ip}:{port}: {e}"),
            }
        }
        Err(err)
    })
    .await;
    r.unwrap_or_else(|_| Err("took too long".into()))
}

async fn query<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Client<S>,
    src: &Source,
) -> Result<TrackData, String> {
    let mut t = TrackData {
        key: src.key.clone(),
        ..Default::default()
    };
    let empty = |t: &TrackData| {
        t.title.is_none() && t.grid.is_none() && t.detail.is_none() && t.preview.is_none()
    };
    match steps(c, src, &mut t).await {
        Ok(()) if empty(&t) => Err("the player has no data for this track".into()),
        Ok(()) => Ok(t),
        Err(e) if t.grid.is_some() || t.title.is_some() => {
            t.missing.push(format!("rest ({e})"));
            Ok(t)
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn steps<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Client<S>,
    src: &Source,
    t: &mut TrackData,
) -> std::io::Result<()> {
    let (d, s, k, id) = (c.d, src.slot, src.kind, src.id);
    let main = db::dmst(d, db::MENU_MAIN, s, k);
    let data = db::dmst(d, db::MENU_DATA, s, k);
    let tag = |cc: &str| {
        vec![
            Field::N4(main),
            Field::N4(id),
            Field::N4(db::fourcc(cc)),
            Field::N4(db::fourcc("EXT")),
        ]
    };

    if k == 1 {
        let items = c.menu(db::METADATA, main, vec![Field::N4(id)]).await?;
        if items.is_empty() {
            t.missing.push("metadata".into());
        }
        analysis::apply_metadata(t, &items);
    }

    let r = c
        .data(db::BEAT_GRID, vec![Field::N4(data), Field::N4(id)])
        .await?;
    t.grid = r.and_then(|m| analysis::grid(m.blob(3)?));
    if t.grid.is_none() {
        t.missing.push("beat grid".into());
    }

    let r = c.data(db::ANLZ_TAG, tag("PWV5")).await?;
    t.detail = r.and_then(|m| analysis::color_detail(m.blob(3)?));
    if t.detail.is_none() {
        let r = c
            .data(
                db::DETAIL,
                vec![Field::N4(main), Field::N4(id), Field::N4(0)],
            )
            .await?;
        t.detail = r.and_then(|m| analysis::mono_detail(m.blob(3)?));
    }

    let r = c.data(db::ANLZ_TAG, tag("PWV4")).await?;
    t.preview = r.and_then(|m| analysis::color_preview(m.blob(3)?));
    if t.preview.is_none() {
        let args = vec![
            Field::N4(data),
            Field::N4(4),
            Field::N4(id),
            Field::N4(0),
            Field::Blob(vec![]),
        ];
        let r = c.data(db::PREVIEW, args).await?;
        t.preview = r.and_then(|m| analysis::mono_preview(m.blob(3)?));
    }
    if t.detail.is_none() && t.preview.is_none() {
        t.missing.push("waveform".into());
    }

    let r = c
        .data(
            db::CUES_EXT,
            vec![Field::N4(data), Field::N4(id), Field::N4(0)],
        )
        .await?;
    t.cues = r
        .map(|m| analysis::cues_ext(m.blob(3).unwrap_or(&[])))
        .unwrap_or_default();
    if t.cues.is_empty() {
        let r = c
            .data(db::CUES, vec![Field::N4(data), Field::N4(id)])
            .await?;
        t.cues = r
            .map(|m| analysis::cues(m.blob(3).unwrap_or(&[])))
            .unwrap_or_default();
    }

    let r = c.data(db::ANLZ_TAG, tag("PSSI")).await?;
    match r.and_then(|m| analysis::phrases(m.blob(3)?)) {
        Some((mood, p)) => {
            t.mood = Some(mood);
            t.phrases = p;
        }
        None => t.missing.push("phrases".into()),
    }
    Ok(())
}

/// Keeps the track data of all players current. Ends only when DJ Link is switched off (the
/// future is dropped, which also stops running requests).
pub async fn run(app: Arc<App>) {
    let mut tasks: JoinSet<(Source, Result<TrackData, String>)> = JoinSet::new();
    let mut inflight: HashSet<String> = HashSet::new();
    let mut failed: HashMap<String, (u64, usize, String)> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                let Ok((src, r)) = done else { continue };
                inflight.remove(&src.key);
                match r {
                    Ok(t) => {
                        crate::log::info(&format!(
                            "DJ Link: track data for {} from player {}: {}{}",
                            t.title.as_deref().unwrap_or("track"),
                            src.owner,
                            summary(&t),
                            if t.missing.is_empty() { String::new() } else { format!(" (missing: {})", t.missing.join(", ")) },
                        ));
                        failed.remove(&src.key);
                        app.tracks.put(Arc::new(t));
                    }
                    Err(e) => {
                        let tries = failed.get(&src.key).map_or(0, |f| f.1) + 1;
                        crate::log::warn(&format!("DJ Link: no track data from player {} ({e})", src.owner));
                        let wait = RETRY_MS.get(tries - 1).copied().unwrap_or(u64::MAX / 2);
                        failed.insert(src.key.clone(), (now_ms() + wait, tries, e));
                    }
                }
            }
            _ = tick.tick() => {}
        }
        let now = now_ms();
        let mut start = vec![];
        let mut changed = false;
        {
            let mut st = app.state.lock().unwrap();
            let cdj = &mut st.cdj;
            let record = cdj.record.then(|| app.dir.join("logs"));
            let wanted: Vec<(u8, Result<Source, &str>)> = cdj
                .devices
                .values()
                .filter(|d| d.track.is_some() && d.kind != Some(Kind::Mixer))
                .map(|d| (d.number, source(cdj, d)))
                .collect();
            let mut busy: HashSet<String> = inflight
                .iter()
                .map(|k| k.split('-').next().unwrap_or("").to_string())
                .collect();
            for (n, src) in wanted {
                let Some(d) = cdj.devices.get_mut(&n) else {
                    continue;
                };
                let src = match src {
                    Ok(s) => s,
                    Err(why) => {
                        changed |= d.set_track_note("none", Some(why));
                        continue;
                    }
                };
                if let Some(t) = app.tracks.get(&src.key) {
                    changed |= d.set_data(t);
                    continue;
                }
                if inflight.contains(&src.key) {
                    changed |= d.set_track_note("loading", None);
                    continue;
                }
                if let Some((until, _, e)) = failed.get(&src.key) {
                    changed |= d.set_track_note("failed", Some(e));
                    if now < *until {
                        continue;
                    }
                }
                let host = src.key.split('-').next().unwrap_or("").to_string();
                if busy.insert(host) {
                    changed |= d.set_track_note("loading", None);
                    start.push(Source {
                        record: record.clone(),
                        ..src
                    });
                }
            }
        }
        if changed {
            app.changes.send_modify(|n| *n = n.wrapping_add(1));
        }
        for src in start {
            inflight.insert(src.key.clone());
            tasks.spawn(async move {
                let r = fetch(src.clone()).await;
                (src, r)
            });
        }
    }
}

/// Writes a dbserver conversation as `logs/dbserver-<time>-<ip>.pcap`, for `--check`.
fn save_recording(dir: &std::path::Path, ip: Ipv4Addr, port: u16, tap: &(Vec<u8>, Vec<u8>)) {
    let me = Ipv4Addr::UNSPECIFIED;
    let path = dir.join(format!("dbserver-{}-{ip}.pcap", super::stamp()));
    let r = std::fs::create_dir_all(dir).and_then(|_| {
        let f = std::fs::File::create(&path)?;
        let mut w = super::pcap::Writer::new(std::io::BufWriter::new(f))?;
        let us = now_ms() * 1000;
        w.write_tcp(us, me, 50_123, ip, port, 1, &tap.0)?;
        w.write_tcp(us, ip, port, me, 50_123, 1, &tap.1)?;
        w.flush()
    });
    match r {
        Ok(()) => crate::log::info(&format!(
            "DJ Link: recorded track data queries to {}",
            path.display()
        )),
        Err(e) => crate::log::warn(&format!("DJ Link: cannot record track data queries: {e}")),
    }
}

fn summary(t: &TrackData) -> String {
    let mut s = vec![];
    if let Some(g) = &t.grid {
        s.push(format!("{} beats", g.ms.len()));
    }
    if let Some(w) = &t.detail {
        s.push(format!(
            "{} waveform",
            if w.color { "colour" } else { "blue" }
        ));
    }
    if !t.cues.is_empty() {
        s.push(format!("{} cues", t.cues.len()));
    }
    if !t.phrases.is_empty() {
        s.push(format!("{} phrases", t.phrases.len()));
    }
    if s.is_empty() {
        "nothing".into()
    } else {
        s.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdj::tracker::Track;

    fn sim_source(owner: u8, id: u32) -> Source {
        Source {
            key: format!("sim-{owner}-3-1-{id}"),
            sim: true,
            ip: None,
            owner,
            slot: 3,
            kind: 1,
            id,
            ask: vec![],
            record: None,
        }
    }

    #[tokio::test]
    async fn simulated_tracks_end_to_end() {
        let t = fetch(sim_source(1, 101)).await.unwrap();
        assert_eq!(t.title.as_deref(), Some("Warehouse Lights (Extended Mix)"));
        assert_eq!(t.artist.as_deref(), Some("Simulated Artist"));
        assert_eq!(t.musical_key.as_deref(), Some("8A"));
        assert_eq!(t.duration_s, Some(372));
        assert_eq!(t.bpm, Some(128.0));
        assert_eq!(t.color, None);
        let g = t.grid.as_ref().unwrap();
        assert_eq!(g.ms[..3], [0, 468, 937]);
        assert_eq!(g.bar[..5], [1, 2, 3, 4, 1]);
        let w = t.detail.as_ref().unwrap();
        assert!(w.color);
        assert_eq!(w.columns, 372 * 150);
        assert_eq!(t.preview.as_ref().unwrap().columns, 1200);
        assert_eq!(t.cues.len(), 4);
        assert_eq!(t.cues[0].hot, 1);
        assert_eq!(t.cues[0].comment.as_deref(), Some("Start"));
        assert_eq!(t.cues[0].color.as_deref(), Some("#30d26e"));
        assert!(t.cues.iter().any(|c| c.loop_ms.is_some()));
        assert_eq!(t.mood.as_deref(), Some("high"));
        let labels: Vec<&str> = t.phrases.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels[..3], ["Intro 1", "Up 1", "Chorus 1"]);
        assert_eq!((t.phrases[0].beat, t.phrases[0].end), (1, 65));
        assert_eq!(t.phrases[2].fill, Some(65 + 64 + 128 - 4));
        assert!(t.missing.is_empty(), "{:?}", t.missing);
    }

    #[tokio::test]
    async fn masked_phrases_are_unmasked() {
        let t = fetch(sim_source(2, 202)).await.unwrap();
        assert_eq!(t.mood.as_deref(), Some("mid"));
        let labels: Vec<&str> = t.phrases.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Intro", "Verse 1", "Verse 2", "Chorus", "Bridge", "Verse 3", "Chorus", "Outro"]
        );
        assert_eq!(t.phrases.last().unwrap().end, 1 + 160 * 4);
    }

    #[tokio::test]
    async fn recorded_conversation_passes_check() {
        // The simulator's server over real TCP, recorded like the Record button does it.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            simdb::serve(s, 1).await
        });
        let s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let mut c = Client::open(s, 2, WAIT, true).await.unwrap();
        let t = query(&mut c, &sim_source(1, 101)).await.unwrap();
        assert_eq!(t.phrases.len(), 9);
        let dir = std::env::temp_dir().join(format!("boothmate-test-{}", std::process::id()));
        save_recording(&dir, Ipv4Addr::LOCALHOST, port, c.tap.as_ref().unwrap());
        let file = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("dbserver-"))
            .unwrap()
            .path();
        let (report, problems) =
            crate::cdj::check::report("rec", &std::fs::read(&file).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(problems, 0, "{report}");
        assert!(
            report.contains("track \"Warehouse Lights (Extended Mix)\" by Simulated Artist, 372 s"),
            "{report}"
        );
        assert!(report.contains("beat grid: 793 beats"), "{report}");
        assert!(report.contains("extended cues: 4"), "{report}");
        assert!(report.contains("analysis tag PSSI"), "{report}");
    }

    #[tokio::test]
    async fn unknown_track_fails() {
        assert!(fetch(sim_source(1, 999)).await.is_err());
    }

    #[test]
    fn borrowed_player_numbers() {
        let mut cdj = CdjState::default();
        for n in [1u8, 2, 3, 7] {
            let mut d = Device {
                number: n,
                kind: Some(Kind::Player),
                ..Default::default()
            };
            if n == 2 {
                // Player 2 plays a track from player 1's USB over Link.
                d.track = Some(Track {
                    player: 1,
                    slot: "USB".into(),
                    slot_id: 3,
                    id: 5,
                    kind: "rekordbox".into(),
                    type_id: 1,
                });
            }
            cdj.devices.insert(n, d);
        }
        assert_eq!(ask_numbers(&cdj, 1), vec![3, 2, 4]);
        assert_eq!(ask_numbers(&cdj, 3), vec![1, 2, 4]);
    }
}
