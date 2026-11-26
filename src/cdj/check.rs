//! `boothmate --check <capture>`: runs a packet capture through the DJ Link parser and tracker
//! and prints what Boothmate understood. For club recordings and public captures.

use super::analysis::{self};
use super::dbserver;
use super::packet::{self, Packet, ANNOUNCE_PORT, BEAT_PORT, STATUS_PORT};
use super::pcap::{self, Datagram};
use super::tracker::{CdjState, Event};
use std::collections::BTreeMap;
use std::fmt::Write;

/// DJ Link datagrams of a capture, in order. A capture on several interfaces at once (macOS
/// pktap) holds broadcasts twice; copies within 5 ms are dropped.
pub fn djlink(all: Vec<Datagram>) -> Vec<Datagram> {
    let mut out: Vec<Datagram> = vec![];
    for d in all {
        if !(ANNOUNCE_PORT..=STATUS_PORT).contains(&d.dst_port)
            || !d.payload.starts_with(packet::MAGIC)
        {
            continue;
        }
        let dup = out
            .iter()
            .rev()
            .take_while(|p| d.time_us.saturating_sub(p.time_us) < 5_000)
            .any(|p| p.src == d.src && p.dst_port == d.dst_port && p.payload == d.payload);
        if !dup {
            out.push(d);
        }
    }
    out
}

#[derive(Default)]
struct PlayerStats {
    name: String,
    beats: u32,
    statuses: u32,
    positions: u32,
    bpm_min: f32,
    bpm_max: f32,
    /// Beat packets whose beat-in-bar did not follow the previous one.
    bar_jumps: u32,
    /// Status beat numbers that went backwards while playing.
    beat_back: u32,
    /// Status beat numbers that differ from the tracker's count (beat packets between statuses).
    predicted: u32,
    mispredicted: u32,
    /// Status beat-in-bar that does not match the beat number on a grid starting on a downbeat.
    grid_offsets: BTreeMap<u8, u32>,
    firmware: String,
}

/// The report, and the number of packets of a kind Boothmate reads that it could not parse.
pub fn report(name: &str, data: &[u8]) -> Result<(String, u32), String> {
    let (mut out, problems) = report_udp(name, data)?;
    let (db, db_problems) = dbserver(data);
    out.push_str(&db);
    Ok((out, problems + db_problems))
}

/// The dbserver (track metadata) conversations in a capture: every message must parse, and
/// the track data in the answers is decoded like Boothmate does it.
fn dbserver(data: &[u8]) -> (String, u32) {
    let Ok(streams) = pcap::read_tcp(data) else {
        return (String::new(), 0);
    };
    let mut out = String::new();
    let mut problems = 0;
    let answers: Vec<_> = streams
        .iter()
        .filter(|s| {
            s.data.len() > 5
                && s.src_port != dbserver::PORT_QUERY
                && s.dst_port != dbserver::PORT_QUERY
        })
        .filter(|s| s.data.starts_with(&dbserver::GREETING))
        .collect();
    if answers.is_empty() {
        return (out, 0);
    }
    let _ = writeln!(out, "\nTrack metadata (dbserver, TCP):");
    for s in answers {
        let msgs = match dbserver::decode_stream(&s.data) {
            Ok(m) => m,
            Err(e) => {
                problems += 1;
                let _ = writeln!(
                    out,
                    "  {}:{} -> {}:{}: not understood: {}",
                    s.src, s.src_port, s.dst, s.dst_port, e.0
                );
                continue;
            }
        };
        let used: usize = {
            let mut n = 5;
            for m in &msgs {
                n += m.encode().len();
            }
            n
        };
        let rest = s.data.len().saturating_sub(used);
        let server = msgs.iter().any(|m| m.kind >= 0x4000);
        let _ = writeln!(
            out,
            "  {}:{} -> {}:{}: {} messages{}",
            s.src,
            s.src_port,
            s.dst,
            s.dst_port,
            msgs.len(),
            if rest > 0 {
                format!(", {rest} bytes not understood (should be 0)")
            } else {
                String::new()
            }
        );
        if rest > 0 {
            problems += 1;
        }
        if !server {
            continue;
        }
        let mut menu = vec![];
        for m in &msgs {
            match m.kind {
                dbserver::MENU_ITEM => menu.push(m.clone()),
                dbserver::MENU_FOOTER => {
                    let mut x = analysis::TrackData::default();
                    analysis::apply_metadata(&mut x, &menu);
                    if x.title.is_some() && x.duration_s.is_some() {
                        let _ = writeln!(
                            out,
                            "    track \"{}\" by {}, {} s, {:.2} BPM, key {}",
                            x.title.unwrap_or_default(),
                            x.artist.unwrap_or_default(),
                            x.duration_s.unwrap_or(0),
                            x.bpm.unwrap_or(0.0),
                            x.musical_key.unwrap_or_default()
                        );
                    }
                    menu.clear();
                }
                0x4602 => {
                    if let Some(g) = m.blob(3).and_then(analysis::grid) {
                        let ok = g.ms.windows(2).all(|w| w[0] < w[1])
                            && g.bar.windows(2).all(|w| w[1] == w[0] % 4 + 1);
                        if !ok {
                            problems += 1;
                        }
                        let _ = writeln!(
                            out,
                            "    beat grid: {} beats, first at {} ms{}",
                            g.ms.len(),
                            g.ms[0],
                            if ok { "" } else { ", NOT in order (should be)" }
                        );
                    }
                }
                0x4402 => {
                    if let Some(w) = m.blob(3).and_then(analysis::mono_preview) {
                        let _ = writeln!(out, "    waveform preview: {} columns", w.columns);
                    }
                }
                0x4a02 => {
                    if let Some(w) = m.blob(3).and_then(analysis::mono_detail) {
                        let _ = writeln!(
                            out,
                            "    waveform detail: {} columns = {:.1} s",
                            w.columns,
                            w.columns as f32 / analysis::DETAIL_PER_S as f32
                        );
                    }
                }
                0x4702 => {
                    let c = analysis::cues(m.blob(3).unwrap_or(&[]));
                    let _ = writeln!(
                        out,
                        "    cues: {}",
                        c.iter()
                            .map(|c| format!(
                                "{}{} ms",
                                if c.hot > 0 {
                                    format!("hot {} ", (b'A' + c.hot - 1) as char)
                                } else {
                                    String::new()
                                },
                                c.ms
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                0x4e02 => {
                    let c = analysis::cues_ext(m.blob(3).unwrap_or(&[]));
                    let _ = writeln!(out, "    extended cues: {}", c.len());
                }
                0x4f02 => {
                    let b = m.blob(3).unwrap_or(&[]);
                    let _ = writeln!(
                        out,
                        "    analysis tag {}: {} bytes",
                        String::from_utf8_lossy(b.get(..4).unwrap_or(&[])),
                        b.len()
                    );
                }
                _ => {}
            }
        }
    }
    (out, problems)
}

fn report_udp(name: &str, data: &[u8]) -> Result<(String, u32), String> {
    let all = pcap::read(data).map_err(|e| format!("{name}: {e}"))?;
    let total = all.len();
    let dl = djlink(all);
    let mut out = String::new();
    let _ = writeln!(out, "{name}: {total} UDP datagrams, {} DJ Link", dl.len());
    let Some(t0) = dl.first().map(|d| d.time_us) else {
        return Ok((out, 0));
    };

    let mut st = CdjState::default();
    let mut kinds: BTreeMap<(u16, u8), (u32, u32)> = BTreeMap::new(); // (port, type) -> (parsed, skipped)
    let mut players: BTreeMap<u8, PlayerStats> = BTreeMap::new();
    let mut last_bb: BTreeMap<u8, u8> = BTreeMap::new();
    let mut last_beat: BTreeMap<u8, u32> = BTreeMap::new();
    let mut events = vec![];
    for d in &dl {
        let now = (d.time_us - t0) / 1000 + 1;
        let ty = d.payload.get(0x0a).copied().unwrap_or(0xff);
        let p = packet::parse(d.dst_port, &d.payload);
        let k = kinds.entry((d.dst_port, ty)).or_default();
        if p.is_some() {
            k.0 += 1
        } else {
            k.1 += 1
        }
        let Some(p) = p else { continue };
        match &p {
            Packet::Beat(b) => {
                let s = players.entry(b.number).or_default();
                s.name.clone_from(&b.name);
                s.beats += 1;
                if let Some(bpm) = b.track_bpm.map(|t| t * (1.0 + b.pitch / 100.0)) {
                    s.bpm_min = if s.bpm_min == 0.0 {
                        bpm
                    } else {
                        s.bpm_min.min(bpm)
                    };
                    s.bpm_max = s.bpm_max.max(bpm);
                }
                if let Some(prev) = last_bb.insert(b.number, b.beat_in_bar) {
                    if b.beat_in_bar != prev % 4 + 1 {
                        s.bar_jumps += 1;
                    }
                }
            }
            Packet::Status(x) => {
                let s = players.entry(x.number).or_default();
                s.name.clone_from(&x.name);
                s.statuses += 1;
                s.firmware.clone_from(&x.firmware);
                if let (Some(b), Some(d)) = (x.beat, st.devices.get(&x.number)) {
                    if x.playing && d.playing == Some(true) && d.beat.is_some() {
                        s.predicted += 1;
                        if d.beat != Some(b) && d.beat.map(|p| p + 1) != Some(b) {
                            s.mispredicted += 1;
                        }
                    }
                }
                if let Some(b) = x.beat {
                    if x.playing && last_beat.get(&x.number).is_some_and(|&p| b + 1 < p) {
                        s.beat_back += 1;
                    }
                    last_beat.insert(x.number, b);
                    if (1..=4).contains(&x.beat_in_bar) {
                        let off = ((x.beat_in_bar as i64 - 1 - (b as i64 - 1)).rem_euclid(4)) as u8;
                        *s.grid_offsets.entry(off).or_default() += 1;
                    }
                }
            }
            Packet::Position(x) => players.entry(x.number).or_default().positions += 1,
            _ => {}
        }
        for e in st.apply(p, Some(d.src), now, false) {
            if !matches!(e, Event::Beat { .. }) {
                events.push((now, e));
            }
        }
    }

    let _ = writeln!(out, "\nPackets (port, type: parsed / not understood):");
    for ((port, ty), (ok, skip)) in &kinds {
        let what = match (*port, *ty) {
            (ANNOUNCE_PORT, 0x06) => "keep-alive",
            (ANNOUNCE_PORT, 0x0a | 0x00 | 0x02 | 0x04) => "startup (ignored)",
            (ANNOUNCE_PORT, 0x08) => "number conflict",
            (BEAT_PORT, 0x28) => "beat",
            (BEAT_PORT, 0x0b) => "CDJ-3000 position",
            (BEAT_PORT, 0x26 | 0x27) => "master handoff (ignored)",
            (BEAT_PORT, 0x2a) => "sync control (ignored)",
            (BEAT_PORT, 0x03) => "mixer on-air (ignored)",
            (ANNOUNCE_PORT, 0x05) => "startup (ignored)",
            (STATUS_PORT, 0x0a) => "CDJ status",
            (STATUS_PORT, 0x29) => "mixer status",
            (STATUS_PORT, 0x05 | 0x06) => "media query (ignored)",
            _ => "unknown",
        };
        let _ = writeln!(out, "  {port} 0x{ty:02x} {what:<26} {ok:>6} / {skip}");
    }

    let problems: u32 = kinds
        .iter()
        .filter(|((port, ty), _)| {
            matches!(
                (*port, *ty),
                (ANNOUNCE_PORT, 0x06) | (BEAT_PORT, 0x28 | 0x0b) | (STATUS_PORT, 0x29)
            )
        })
        .map(|(_, (_, skip))| skip)
        .sum();
    let _ = writeln!(out, "  not understood but should be: {problems}");

    let _ = writeln!(out, "\nDevices:");
    for d in st.devices.values() {
        let _ = writeln!(
            out,
            "  {:>2} {:<16} {:<9} {}",
            d.number,
            d.name,
            d.kind
                .map(|k| format!("{k:?}").to_lowercase())
                .unwrap_or_default(),
            d.ip.map(|i| i.to_string()).unwrap_or_default()
        );
    }
    let _ = writeln!(out, "\nPlayers:");
    for (n, s) in &players {
        let _ = writeln!(
            out,
            "  {n:>2} {:<16} fw {:<5} beats {:>5}, status {:>5}, position {:>5}, BPM {:.2}-{:.2}, beat-in-bar breaks {}, beat number back {}, tracker off {}/{}, grid offsets {:?}",
            s.name, s.firmware, s.beats, s.statuses, s.positions, s.bpm_min, s.bpm_max, s.bar_jumps, s.beat_back, s.mispredicted, s.predicted, s.grid_offsets
        );
    }
    let _ = writeln!(out, "\nEvents (ms from start):");
    for (t, e) in events.iter().take(60) {
        let _ = writeln!(out, "  {t:>8} {e:?}");
    }
    if events.len() > 60 {
        let _ = writeln!(out, "  ... {} more", events.len() - 60);
    }
    Ok((out, problems))
}
