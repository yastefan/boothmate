//! OSC input from grandMA3 (OSC line with Send = Yes).

use crate::midi::mapping::Action;
use crate::profile::{InputAddresses, Profile, Target};
use crate::state::{now_ms, OscEvent, State};
use crate::App;
use rosc::{OscMessage, OscPacket, OscType};
use std::sync::Arc;
use tokio::net::UdpSocket;

pub async fn run(app: Arc<App>) {
    let listen = app.config.osc.listen.clone();
    let mut logged = false;
    let socket = loop {
        match bind(&listen) {
            Ok(s) => break s,
            Err(e) => {
                if !logged {
                    crate::log::warn(&format!(
                        "OSC: cannot listen on {listen} ({e}); retrying every 2 s"
                    ));
                    logged = true;
                }
                app.update(|st| st.connections.osc_error = Some(format!("cannot listen: {e}")));
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };
    crate::log::info(&format!("OSC: listening on {listen}"));
    app.update(|st| st.connections.osc_error = None);
    let mut buf = vec![0u8; 65536];
    loop {
        let (len, _peer) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                // Windows reports ICMP "port unreachable" as a receive error; keep listening.
                crate::log::debug(&format!("osc: receive error: {e}"));
                continue;
            }
        };
        let packet = match rosc::decoder::decode_udp(&buf[..len]) {
            Ok((_, p)) => p,
            Err(e) => {
                crate::log::debug(&format!("osc: undecodable packet ({len} bytes): {e:?}"));
                continue;
            }
        };
        let mut messages = Vec::new();
        flatten(packet, &mut messages);
        let profile = app.profile.read().unwrap().clone();
        let mut refresh = false;
        app.update(|st| {
            for msg in messages {
                st.connections.osc_packets += 1;
                st.connections.osc_last = Some(now_ms());
                match parse(&profile, &msg) {
                    Some(ev) => {
                        crate::log::debug(&format!("osc: {} {:?} -> {ev:?}", msg.addr, msg.args));
                        crate::midi::mapping::learn_from_osc(st, &ev);
                        refresh |=
                            switches_playback(st, &ev) || crate::state::state_unknown_after(&ev);
                        st.apply_osc(ev);
                    }
                    None => {
                        crate::log::debug(&format!("osc: unknown {} {:?}", msg.addr, msg.args));
                        let unknown = &mut st.connections.osc_unknown;
                        if unknown.len() < 50 || unknown.contains_key(&msg.addr) {
                            *unknown.entry(msg.addr).or_insert(0) += 1;
                        }
                    }
                }
            }
        });
        if refresh {
            crate::plugin::refresh_soon(&app);
        }
    }
}

/// Whether a fader event may switch a sequence on or off (moved away from or to 0). grandMA3
/// sends no key event for that, so the running state has to be read from the plugin.
fn switches_playback(st: &State, ev: &OscEvent) -> bool {
    let OscEvent::SequenceFader {
        seq, token, value, ..
    } = ev
    else {
        return false;
    };
    if token != "FaderMaster" {
        return false;
    }
    let old = st
        .sequences
        .get(seq)
        .and_then(|s| s.faders.get("FaderMaster").copied());
    match old {
        Some(old) => (old <= 0.0) != (*value <= 0.0),
        None => true,
    }
}

/// grandMA3 binds its OSC line's port itself (on all interfaces), also when it only sends.
/// Address reuse lets Boothmate bind the same port on a specific address next to it, in either
/// start order; the more specific binding receives the packets.
fn bind(listen: &str) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let addr: std::net::SocketAddr = listen
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{e}")))?;
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.bind(&addr.into())?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

/// Sends a MIDI action to grandMA3 as OSC input.
pub fn send_action(app: &App, action: &Action) {
    let msg = {
        let profile = app.profile.read().unwrap();
        build(&profile.osc.input, &app.config.osc.prefix, action)
    };
    let bytes = match rosc::encoder::encode(&OscPacket::Message(msg)) {
        Ok(b) => b,
        Err(e) => {
            crate::log::debug(&format!("osc: cannot encode: {e:?}"));
            return;
        }
    };
    if let Err(e) = app.osc_out.send_to(&bytes, &app.config.osc.send) {
        crate::log::debug(&format!("osc: send to {} failed: {e}", app.config.osc.send));
    }
}

pub fn build(input: &InputAddresses, prefix: &str, action: &Action) -> OscMessage {
    let addr = |t: &str, page, exec| InputAddresses::address(t, prefix, page, exec);
    match *action {
        Action::Key {
            page,
            exec,
            pressed,
        } => OscMessage {
            addr: addr(&input.key, page, exec),
            args: vec![OscType::Int(pressed as i32)],
        },
        Action::Fader { page, exec, value } => OscMessage {
            addr: addr(&input.fader, page, exec),
            args: vec![OscType::Float(value / 100.0 * input.fader_range)],
        },
        Action::Encoder { page, exec, delta } => OscMessage {
            addr: addr(&input.encoder, page, exec),
            args: vec![OscType::Float(delta)],
        },
    }
}

fn flatten(p: OscPacket, out: &mut Vec<OscMessage>) {
    match p {
        OscPacket::Message(m) => out.push(m),
        OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
    }
}

fn number(a: &OscType) -> Option<f32> {
    match a {
        OscType::Float(f) => Some(*f),
        OscType::Double(d) => Some(*d as f32),
        _ => None,
    }
}

/// Turns one message into an event, using the profile for the address.
pub fn parse(profile: &Profile, msg: &OscMessage) -> Option<OscEvent> {
    let target = profile.target(&msg.addr)?;
    let args = &msg.args;
    let action = match args.first() {
        Some(OscType::String(s)) => s.clone(),
        _ => return None,
    };
    let value = args.iter().find_map(number);
    let text = args.iter().skip(1).find_map(|a| match a {
        OscType::String(s) => Some(s.clone()),
        _ => None,
    });
    match (target, value) {
        (Target::Master { key, label }, value) => Some(OscEvent::Master {
            key,
            label,
            token: action,
            value,
            text,
        }),
        (Target::Sequence(seq), Some(value)) => Some(OscEvent::SequenceFader {
            seq,
            token: action,
            value,
            name: text,
        }),
        (Target::Sequence(seq), None) => {
            let state = match args.get(1) {
                Some(OscType::Int(i)) => *i,
                _ => 1,
            };
            Some(OscEvent::SequenceKey {
                seq,
                action,
                state,
                text: text.unwrap_or_default(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn profile() -> Profile {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ma-profiles");
        crate::profile::load_all(&dir)
            .unwrap()
            .into_iter()
            .find(|p| p.id == "2.5")
            .unwrap()
    }

    fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
        OscMessage {
            addr: addr.into(),
            args,
        }
    }

    fn s(v: &str) -> OscType {
        OscType::String(v.into())
    }

    // Messages as captured from onPC 2.5.1.1 in the M0 spike.
    #[test]
    fn captured_messages() {
        let p = profile();
        assert_eq!(
            parse(
                &p,
                &msg(
                    "/14.14.1.7.2",
                    vec![s("Go+"), OscType::Int(1), s("MCP Test;2 Rot")]
                )
            ),
            Some(OscEvent::SequenceKey {
                seq: 2,
                action: "Go+".into(),
                state: 1,
                text: "MCP Test;2 Rot".into()
            })
        );
        assert_eq!(
            parse(
                &p,
                &msg(
                    "/14.14.1.7.2",
                    vec![
                        s("FaderMaster"),
                        OscType::Int(3),
                        OscType::Float(45.97),
                        s("MCP Test")
                    ]
                )
            ),
            Some(OscEvent::SequenceFader {
                seq: 2,
                token: "FaderMaster".into(),
                value: 45.97,
                name: Some("MCP Test".into())
            })
        );
        assert_eq!(
            parse(
                &p,
                &msg(
                    "/14.13.2.1",
                    vec![s("FaderMaster"), OscType::Int(1), OscType::Float(80.0)]
                )
            ),
            Some(OscEvent::Master {
                key: "14.13.2.1".into(),
                label: "Grand".into(),
                token: "FaderMaster".into(),
                value: Some(80.0),
                text: None
            })
        );
        assert_eq!(
            parse(
                &p,
                &msg(
                    "/14.14.1.6.1",
                    vec![s("FaderMaster"), OscType::Int(1), OscType::Float(60.0)]
                )
            ),
            Some(OscEvent::Master {
                key: "14.14.1.6.1".into(),
                label: "Group 1".into(),
                token: "FaderMaster".into(),
                value: Some(60.0),
                text: None
            })
        );
        // Speed master 1 (Schmiddi's report from onPC 2.5.1.1); argument layout assumed like other masters.
        assert!(matches!(
            parse(&p, &msg("/14.13.3.1", vec![s("FaderMaster"), OscType::Int(3), OscType::Float(50.0)])),
            Some(OscEvent::Master { ref label, value: Some(v), .. }) if label == "Speed 1" && v == 50.0
        ));
        assert_eq!(parse(&p, &msg("/foo", vec![s("Go+")])), None);
        assert_eq!(parse(&p, &msg("/14.14.1.7.2", vec![])), None);
    }

    #[test]
    fn builds_executor_input() {
        let i = InputAddresses::default();
        let m = build(
            &i,
            "",
            &Action::Key {
                page: 1,
                exec: 201,
                pressed: true,
            },
        );
        assert_eq!(
            (m.addr.as_str(), m.args),
            ("/Page1/Key201", vec![OscType::Int(1)])
        );
        let m = build(
            &i,
            "gma3",
            &Action::Fader {
                page: 2,
                exec: 202,
                value: 50.0,
            },
        );
        assert_eq!(
            (m.addr.as_str(), m.args),
            ("/gma3/Page2/Fader202", vec![OscType::Float(50.0)])
        );
        let m = build(
            &i,
            "",
            &Action::Encoder {
                page: 1,
                exec: 203,
                delta: -2.0,
            },
        );
        assert_eq!(
            (m.addr.as_str(), m.args),
            ("/Page1/Encoder203", vec![OscType::Float(-2.0)])
        );
    }

    #[test]
    fn decodes_bundles() {
        let inner =
            OscPacket::Message(msg("/14.14.1.7.1", vec![s("Off"), OscType::Int(1), s("X")]));
        let bundle = OscPacket::Bundle(rosc::OscBundle {
            timetag: rosc::OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: vec![inner.clone(), inner],
        });
        let bytes = rosc::encoder::encode(&bundle).unwrap();
        let (_, p) = rosc::decoder::decode_udp(&bytes).unwrap();
        let mut out = Vec::new();
        flatten(p, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn fader_through_zero_needs_a_resync() {
        let fader = |value| OscEvent::SequenceFader {
            seq: 1,
            token: "FaderMaster".into(),
            value,
            name: None,
        };
        let mut st = State::default();
        assert!(switches_playback(&st, &fader(0.0)), "unknown start value");
        st.apply_osc(fader(0.0));
        assert!(
            switches_playback(&st, &fader(4.0)),
            "up from 0: may switch on"
        );
        st.apply_osc(fader(4.0));
        assert!(!switches_playback(&st, &fader(30.0)));
        st.apply_osc(fader(30.0));
        assert!(
            switches_playback(&st, &fader(0.0)),
            "down to 0: may switch off"
        );
        let other = OscEvent::SequenceFader {
            seq: 1,
            token: "FaderTemp".into(),
            value: 50.0,
            name: None,
        };
        assert!(!switches_playback(&st, &other));
    }
}
