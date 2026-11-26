//! MIDI ports (midir) on a thread of their own: finds controllers, pairs each input with its
//! output, reconnects after unplugging, and on macOS offers a virtual "Boothmate" port pair.

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// Name of the virtual ports and prefix of everything Boothmate opens.
pub const CLIENT: &str = "Boothmate";
/// Key of the virtual port pair.
pub const VIRTUAL: &str = "Boothmate (virtual)";

pub enum Cmd {
    /// Send to the output paired with this input port.
    Send { port: String, bytes: Vec<u8> },
}

#[derive(Debug)]
pub enum Event {
    /// A message from an input port.
    Message {
        port: String,
        bytes: Vec<u8>,
    },
    /// Current inputs and whether each has an output.
    Ports(Vec<(String, bool)>),
    /// An output was (re)connected: send all feedback again.
    OutputConnected(String),
    Error(String),
}

pub struct Options {
    pub virtual_port: bool,
    pub ignore: Vec<String>,
    /// Port name patterns from device profiles, used to pair inputs with outputs.
    pub patterns: Vec<String>,
}

struct Port {
    _input: Option<MidiInputConnection<()>>,
    output: Option<MidiOutputConnection>,
}

pub fn spawn(opts: Options, events: UnboundedSender<Event>) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("midi".into())
        .spawn(move || run(opts, events, rx))
        .expect("spawn MIDI thread");
    tx
}

fn run(opts: Options, events: UnboundedSender<Event>, rx: mpsc::Receiver<Cmd>) {
    let mut ports: BTreeMap<String, Port> = BTreeMap::new();
    if opts.virtual_port {
        open_virtual(&mut ports, &events);
    }
    let mut last_list = Vec::new();
    loop {
        scan(&opts, &mut ports, &events);
        let list: Vec<(String, bool)> = ports
            .iter()
            .map(|(k, p)| (k.clone(), p.output.is_some()))
            .collect();
        if list != last_list {
            let _ = events.send(Event::Ports(list.clone()));
            last_list = list;
        }
        // Handle sends until the next scan.
        let until = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let left = until.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(Cmd::Send { port, bytes }) => {
                    if let Some(out) = ports.get_mut(&port).and_then(|p| p.output.as_mut()) {
                        if let Err(e) = out.send(&bytes) {
                            crate::log::debug(&format!("midi: send to {port} failed: {e}"));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

fn own(name: &str) -> bool {
    name.starts_with(CLIENT)
}

fn ignored(opts: &Options, name: &str) -> bool {
    let l = name.to_lowercase();
    own(name) || opts.ignore.iter().any(|i| l.contains(&i.to_lowercase()))
}

/// The part of a port name that inputs and outputs share: Windows names them
/// `MIDIIN2 (APC mini mk2)` / `MIDIOUT2 (APC mini mk2)`, macOS uses the same name for both.
fn core(name: &str) -> String {
    let n = name.trim();
    let n = match (n.find('('), n.rfind(')')) {
        (Some(a), Some(b)) if b > a && n.to_uppercase().starts_with("MIDI") => &n[a + 1..b],
        _ => n,
    };
    n.to_lowercase()
}

fn pair<'a>(opts: &Options, input: &str, outputs: &'a [String]) -> Option<&'a String> {
    if let Some(o) = outputs.iter().find(|o| *o == input) {
        return Some(o);
    }
    let c = core(input);
    if let Some(o) = outputs.iter().find(|o| core(o) == c) {
        return Some(o);
    }
    let l = input.to_lowercase();
    let pat = opts
        .patterns
        .iter()
        .find(|p| l.contains(&p.to_lowercase()))?;
    let pat = pat.to_lowercase();
    outputs.iter().find(|o| o.to_lowercase().contains(&pat))
}

fn scan(opts: &Options, ports: &mut BTreeMap<String, Port>, events: &UnboundedSender<Event>) {
    let Ok(probe) = MidiInput::new(CLIENT) else {
        return;
    };
    let inputs: Vec<String> = probe
        .ports()
        .iter()
        .filter_map(|p| probe.port_name(p).ok())
        .filter(|n| !ignored(opts, n))
        .collect();
    let outputs: Vec<String> = MidiOutput::new(CLIENT)
        .map(|o| {
            o.ports()
                .iter()
                .filter_map(|p| o.port_name(p).ok())
                .filter(|n| !own(n))
                .collect()
        })
        .unwrap_or_default();

    // Unplugged: close.
    ports.retain(|name, _| {
        let keep = name == VIRTUAL || inputs.contains(name);
        if !keep {
            crate::log::info(&format!("MIDI: input {name} gone"));
        }
        keep
    });
    for name in &inputs {
        if !ports.contains_key(name) {
            match connect_input(name, events) {
                Ok(conn) => {
                    crate::log::info(&format!("MIDI: input {name}"));
                    ports.insert(
                        name.clone(),
                        Port {
                            _input: Some(conn),
                            output: None,
                        },
                    );
                }
                Err(e) => {
                    let _ = events.send(Event::Error(format!("{name}: {e}")));
                    continue;
                }
            }
        }
        let port = ports.get_mut(name).expect("inserted above");
        if port.output.is_none() {
            if let Some(out_name) = pair(opts, name, &outputs) {
                match connect_output(out_name) {
                    Ok(conn) => {
                        crate::log::info(&format!("MIDI: output {out_name} for {name}"));
                        port.output = Some(conn);
                        let _ = events.send(Event::OutputConnected(name.clone()));
                    }
                    Err(e) => crate::log::debug(&format!("midi: output {out_name}: {e}")),
                }
            }
        }
    }
}

fn connect_input(
    name: &str,
    events: &UnboundedSender<Event>,
) -> Result<MidiInputConnection<()>, String> {
    let input = MidiInput::new(CLIENT).map_err(|e| e.to_string())?;
    let port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).ok().as_deref() == Some(name))
        .ok_or("port gone")?;
    let tx = events.clone();
    let key = name.to_string();
    input
        .connect(
            &port,
            "boothmate-in",
            move |_, bytes, _| {
                let _ = tx.send(Event::Message {
                    port: key.clone(),
                    bytes: bytes.to_vec(),
                });
            },
            (),
        )
        .map_err(|e| e.to_string())
}

fn connect_output(name: &str) -> Result<MidiOutputConnection, String> {
    let output = MidiOutput::new(CLIENT).map_err(|e| e.to_string())?;
    let port = output
        .ports()
        .into_iter()
        .find(|p| output.port_name(p).ok().as_deref() == Some(name))
        .ok_or("port gone")?;
    output
        .connect(&port, "boothmate-out")
        .map_err(|e| e.to_string())
}

#[cfg(unix)]
fn open_virtual(ports: &mut BTreeMap<String, Port>, events: &UnboundedSender<Event>) {
    use midir::os::unix::{VirtualInput, VirtualOutput};
    let tx = events.clone();
    let input = MidiInput::new(CLIENT)
        .map_err(|e| e.to_string())
        .and_then(|i| {
            i.create_virtual(
                CLIENT,
                move |_, bytes, _| {
                    let _ = tx.send(Event::Message {
                        port: VIRTUAL.into(),
                        bytes: bytes.to_vec(),
                    });
                },
                (),
            )
            .map_err(|e| e.to_string())
        });
    let output = MidiOutput::new(CLIENT)
        .ok()
        .and_then(|o| o.create_virtual(CLIENT).ok());
    match input {
        Ok(conn) => {
            crate::log::info("MIDI: virtual port \"Boothmate\" open");
            let has_out = output.is_some();
            ports.insert(
                VIRTUAL.into(),
                Port {
                    _input: Some(conn),
                    output,
                },
            );
            if has_out {
                let _ = events.send(Event::OutputConnected(VIRTUAL.into()));
            }
        }
        Err(e) => {
            let _ = events.send(Event::Error(format!("virtual port: {e}")));
        }
    }
}

#[cfg(not(unix))]
fn open_virtual(_: &mut BTreeMap<String, Port>, _: &UnboundedSender<Event>) {
    // Windows has no virtual MIDI ports without a driver such as loopMIDI.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_inputs_with_outputs() {
        let opts = Options {
            virtual_port: false,
            ignore: vec!["IAC".into()],
            patterns: vec!["APC mini mk2".into()],
        };
        let outs = vec![
            "MIDIOUT2 (APC mini mk2)".to_string(),
            "X-Touch".to_string(),
            "APC mini mk2 Notes".to_string(),
        ];
        assert_eq!(pair(&opts, "X-Touch", &outs).unwrap(), "X-Touch");
        assert_eq!(
            pair(&opts, "MIDIIN2 (APC mini mk2)", &outs).unwrap(),
            "MIDIOUT2 (APC mini mk2)"
        );
        assert_eq!(
            pair(&opts, "APC mini mk2 Control", &outs).unwrap(),
            "MIDIOUT2 (APC mini mk2)"
        );
        assert_eq!(pair(&opts, "Other", &outs), None);
        assert!(ignored(&opts, "IAC Driver Bus 1"));
        assert!(ignored(&opts, "Boothmate"));
    }
}
