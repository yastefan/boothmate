//! The players' remote database ("dbserver", TCP): track metadata, beat grids, waveforms, cues
//! and phrases of the loaded track. Read-only: Boothmate only asks about tracks that are
//! already loaded on a player, it never browses or loads anything.
//!
//! Protocol after the Deep Symmetry analysis ("Track Metadata"). Checked against the dbserver
//! traffic between two CDJ-2000nexus in the public dysentery captures (`--check`).

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Every player answers on this port with the port of its database server (usually 1051).
pub const PORT_QUERY: u16 = 12523;
pub const MAGIC: u32 = 0x8723_49ae;
/// Transaction ID of the context setup message.
pub const SETUP_TX: u32 = 0xffff_fffe;
const MAX_FIELD: usize = 16 << 20;

// Request types.
pub const SETUP: u16 = 0x0000;
pub const METADATA: u16 = 0x2002;
pub const PREVIEW: u16 = 0x2004;
pub const CUES: u16 = 0x2104;
pub const BEAT_GRID: u16 = 0x2204;
pub const DETAIL: u16 = 0x2904;
pub const CUES_EXT: u16 = 0x2b04;
pub const ANLZ_TAG: u16 = 0x2c04;
pub const RENDER: u16 = 0x3000;
// Response types.
pub const OK: u16 = 0x4000;
pub const MENU_HEADER: u16 = 0x4001;
pub const MENU_ITEM: u16 = 0x4101;
pub const MENU_FOOTER: u16 = 0x4201;

/// Menu locations (the M byte of DMST).
pub const MENU_MAIN: u8 = 1;
pub const MENU_DATA: u8 = 8;

#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    N1(u8),
    N2(u16),
    N4(u32),
    /// Binary data. Empty blobs are left out on the wire (the length before them is 0).
    Blob(Vec<u8>),
    /// UTF-16BE on the wire, with a trailing NUL that is not kept here.
    Str(String),
}

impl Field {
    pub fn num(&self) -> Option<u32> {
        match *self {
            Field::N1(n) => Some(n as u32),
            Field::N2(n) => Some(n as u32),
            Field::N4(n) => Some(n),
            _ => None,
        }
    }

    pub fn blob(&self) -> Option<&[u8]> {
        match self {
            Field::Blob(b) => Some(b),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Field::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Argument tag in the message header.
    fn tag(&self) -> u8 {
        match self {
            Field::Str(_) => 0x02,
            Field::Blob(_) => 0x03,
            _ => 0x06,
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Field::N1(n) => out.extend([0x0f, *n]),
            Field::N2(n) => {
                out.push(0x10);
                out.extend(n.to_be_bytes());
            }
            Field::N4(n) => {
                out.push(0x11);
                out.extend(n.to_be_bytes());
            }
            Field::Blob(b) => {
                out.push(0x14);
                out.extend((b.len() as u32).to_be_bytes());
                out.extend(b);
            }
            Field::Str(s) => {
                let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
                out.push(0x26);
                out.extend((units.len() as u32).to_be_bytes());
                for u in units {
                    out.extend(u.to_be_bytes());
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub tx: u32,
    pub kind: u16,
    pub args: Vec<Field>,
}

impl Message {
    pub fn new(tx: u32, kind: u16, args: Vec<Field>) -> Self {
        Message { tx, kind, args }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![];
        Field::N4(MAGIC).encode(&mut out);
        Field::N4(self.tx).encode(&mut out);
        Field::N2(self.kind).encode(&mut out);
        Field::N1(self.args.len() as u8).encode(&mut out);
        let mut tags = [0u8; 12];
        for (t, a) in tags.iter_mut().zip(&self.args) {
            *t = a.tag();
        }
        Field::Blob(tags.to_vec()).encode(&mut out);
        for a in &self.args {
            if !matches!(a, Field::Blob(b) if b.is_empty()) {
                a.encode(&mut out);
            }
        }
        out
    }

    pub fn num(&self, i: usize) -> Option<u32> {
        self.args.get(i).and_then(Field::num)
    }

    pub fn blob(&self, i: usize) -> Option<&[u8]> {
        self.args.get(i).and_then(Field::blob)
    }

    pub fn str(&self, i: usize) -> Option<&str> {
        self.args.get(i).and_then(Field::str)
    }
}

/// Malformed data (not just incomplete).
#[derive(Debug, Clone, PartialEq)]
pub struct Malformed(pub String);

/// Reads one field at `o`. `Ok(None)`: more bytes are needed.
fn field(d: &[u8], o: usize) -> Result<Option<(Field, usize)>, Malformed> {
    let Some(&t) = d.get(o) else {
        return Ok(None);
    };
    let need = |n: usize| d.len() >= o + 1 + n;
    let len_at = |p: usize| u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) as usize;
    Ok(match t {
        0x0f if need(1) => Some((Field::N1(d[o + 1]), o + 2)),
        0x10 if need(2) => Some((Field::N2(u16::from_be_bytes([d[o + 1], d[o + 2]])), o + 3)),
        0x11 if need(4) => Some((Field::N4(len_at(o + 1) as u32), o + 5)),
        0x14 | 0x26 if need(4) => {
            let n = len_at(o + 1);
            let bytes = if t == 0x26 { n * 2 } else { n };
            if bytes > MAX_FIELD {
                return Err(Malformed(format!("field of {bytes} bytes")));
            }
            if d.len() < o + 5 + bytes {
                return Ok(None);
            }
            let raw = &d[o + 5..o + 5 + bytes];
            let f = if t == 0x14 {
                Field::Blob(raw.to_vec())
            } else {
                let units: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                let s = String::from_utf16_lossy(&units);
                Field::Str(s.trim_end_matches('\0').to_string())
            };
            Some((f, o + 5 + bytes))
        }
        0x0f | 0x10 | 0x11 | 0x14 | 0x26 => None,
        _ => return Err(Malformed(format!("unknown field type {t:#04x}"))),
    })
}

/// Decodes one message from the start of `d`: the message and its length, or `None` while
/// incomplete. A blob argument right after a number 0 (its length) is not on the wire.
pub fn decode(d: &[u8]) -> Result<Option<(Message, usize)>, Malformed> {
    let mut o = 0;
    let mut head = vec![];
    for _ in 0..5 {
        let Some((f, n)) = field(d, o)? else {
            return Ok(None);
        };
        head.push(f);
        o = n;
    }
    if head[0].num() != Some(MAGIC) {
        return Err(Malformed("no message start".into()));
    }
    let (Some(tx), Some(kind), Some(argc), Some(tags)) =
        (head[1].num(), head[2].num(), head[3].num(), head[4].blob())
    else {
        return Err(Malformed("bad message header".into()));
    };
    let argc = (argc as usize).min(12).min(tags.len());
    let tags = tags[..argc].to_vec();
    let mut args = vec![];
    for t in tags {
        if t == 0x03 && args.last().and_then(Field::num) == Some(0) {
            args.push(Field::Blob(vec![]));
            continue;
        }
        let Some((f, n)) = field(d, o)? else {
            return Ok(None);
        };
        args.push(f);
        o = n;
    }
    Ok(Some((
        Message {
            tx,
            kind: kind as u16,
            args,
        },
        o,
    )))
}

/// All messages of a recorded TCP stream (either direction), after the greeting.
pub fn decode_stream(d: &[u8]) -> Result<Vec<Message>, Malformed> {
    let mut o = if d.starts_with(&GREETING) { 5 } else { 0 };
    let mut out = vec![];
    while o < d.len() {
        match decode(&d[o..])? {
            Some((m, n)) => {
                out.push(m);
                o += n;
            }
            None => break,
        }
    }
    Ok(out)
}

/// The greeting both sides send first: the number 1 as a 4-byte field.
pub const GREETING: [u8; 5] = [0x11, 0, 0, 0, 1];

/// The request for the database port, sent to port 12523.
pub fn port_query() -> Vec<u8> {
    let mut b = vec![0, 0, 0, 0x0f];
    b.extend(b"RemoteDBServer\0");
    b
}

/// DMST argument: asking device, menu location, slot, track type.
pub fn dmst(d: u8, menu: u8, slot: u8, kind: u8) -> u32 {
    u32::from_be_bytes([d, menu, slot, kind])
}

/// A four-character code as the protocol sends it (ASCII backwards, padded with NUL).
pub fn fourcc(s: &str) -> u32 {
    let mut b = [0u8; 4];
    for (i, c) in s.bytes().take(4).enumerate() {
        b[i] = c;
    }
    u32::from_le_bytes(b)
}

/// Asks a device for its database port.
pub async fn db_port(ip: Ipv4Addr, wait: Duration) -> io::Result<u16> {
    let mut s = timeout(
        wait,
        tokio::net::TcpStream::connect(SocketAddr::from((ip, PORT_QUERY))),
    )
    .await?;
    timeout(wait, s.write_all(&port_query())).await?;
    let mut b = [0u8; 2];
    timeout(wait, s.read_exact(&mut b)).await?;
    Ok(u16::from_be_bytes(b))
}

async fn timeout<T, E: Into<io::Error>>(
    wait: Duration,
    f: impl std::future::Future<Output = Result<T, E>>,
) -> io::Result<T> {
    match tokio::time::timeout(wait, f).await {
        Ok(r) => r.map_err(Into::into),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "no answer")),
    }
}

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// A connection to one database server, set up for queries as device `d`.
pub struct Client<S> {
    s: S,
    buf: Vec<u8>,
    tx: u32,
    /// The number we ask as (the D byte of DMST).
    pub d: u8,
    wait: Duration,
    /// Copy of everything sent and received, for recordings.
    pub tap: Option<(Vec<u8>, Vec<u8>)>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    /// Greeting and query context setup. Fails if the server does not accept `d`. With `tap`,
    /// the client keeps a copy of the conversation.
    pub async fn open(s: S, d: u8, wait: Duration, tap: bool) -> io::Result<Self> {
        let mut c = Client {
            s,
            buf: vec![],
            tx: 0,
            d,
            wait,
            tap: tap.then(Default::default),
        };
        c.write(&GREETING).await?;
        let mut g = [0u8; 5];
        timeout(wait, c.s.read_exact(&mut g)).await?;
        if let Some(t) = &mut c.tap {
            t.1.extend_from_slice(&g);
        }
        if g != GREETING {
            return Err(bad(format!("unexpected greeting {g:02x?}")));
        }
        let m = Message::new(SETUP_TX, SETUP, vec![Field::N4(d as u32)]);
        c.write(&m.encode()).await?;
        let r = c.reply(SETUP_TX).await?;
        if r.kind != OK {
            return Err(bad(format!(
                "setup as player {d} refused (answer {:#06x})",
                r.kind
            )));
        }
        Ok(c)
    }

    async fn write(&mut self, b: &[u8]) -> io::Result<()> {
        if let Some(t) = &mut self.tap {
            t.0.extend_from_slice(b);
        }
        timeout(self.wait, self.s.write_all(b)).await
    }

    async fn read(&mut self) -> io::Result<Message> {
        loop {
            match decode(&self.buf) {
                Ok(Some((m, n))) => {
                    self.buf.drain(..n);
                    return Ok(m);
                }
                Ok(None) => {}
                Err(Malformed(e)) => return Err(bad(e)),
            }
            let mut chunk = [0u8; 8192];
            let n = timeout(self.wait, self.s.read(&mut chunk)).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
            if let Some(t) = &mut self.tap {
                t.1.extend_from_slice(&chunk[..n]);
            }
        }
    }

    /// The next message of transaction `tx` (others are skipped).
    async fn reply(&mut self, tx: u32) -> io::Result<Message> {
        loop {
            let m = self.read().await?;
            if m.tx == tx {
                return Ok(m);
            }
        }
    }

    pub async fn send(&mut self, kind: u16, args: Vec<Field>) -> io::Result<u32> {
        self.tx = self.tx.wrapping_add(1);
        let m = Message::new(self.tx, kind, args);
        self.write(&m.encode()).await?;
        Ok(self.tx)
    }

    /// A request answered by one message.
    pub async fn request(&mut self, kind: u16, args: Vec<Field>) -> io::Result<Message> {
        let tx = self.send(kind, args).await?;
        self.reply(tx).await
    }

    /// A request whose answer is a blob (beat grid, waveforms, cues, analysis tags): the blob,
    /// or `None` if the server has none (or does not know the request).
    pub async fn data(&mut self, kind: u16, args: Vec<Field>) -> io::Result<Option<Message>> {
        let r = self.request(kind, args).await?;
        let has = r.blob(3).is_some_and(|b| !b.is_empty());
        Ok(has.then_some(r))
    }

    /// A menu request (like track metadata): the items, empty if the server has none.
    pub async fn menu(
        &mut self,
        kind: u16,
        dmst: u32,
        args: Vec<Field>,
    ) -> io::Result<Vec<Message>> {
        let mut a = vec![Field::N4(dmst)];
        a.extend(args);
        let r = self.request(kind, a).await?;
        let count = r.num(1).unwrap_or(0);
        if r.kind != OK || count == 0 || count == 0xffff_ffff {
            return Ok(vec![]);
        }
        let count = count.min(64);
        let tx = self
            .send(
                RENDER,
                vec![
                    Field::N4(dmst),
                    Field::N4(0),
                    Field::N4(count),
                    Field::N4(0),
                    Field::N4(count),
                    Field::N4(0),
                ],
            )
            .await?;
        let mut items = vec![];
        loop {
            let m = self.reply(tx).await?;
            match m.kind {
                MENU_ITEM => items.push(m),
                MENU_HEADER => {}
                _ => return Ok(items),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let m = Message::new(
            7,
            MENU_ITEM,
            vec![
                Field::N4(1),
                Field::N4(0xc8),
                Field::N4(0x2c),
                Field::Str("Lönely – Klub Cut".into()),
                Field::N4(0),
                Field::Blob(vec![]),
                Field::N4(4),
                Field::Blob(vec![1, 2, 3]),
            ],
        );
        let b = m.encode();
        let (back, n) = decode(&b).unwrap().unwrap();
        assert_eq!(n, b.len());
        assert_eq!(back, m);
        // Incomplete input asks for more instead of failing.
        for cut in 0..b.len() {
            assert_eq!(decode(&b[..cut]), Ok(None), "cut at {cut}");
        }
    }

    #[test]
    fn empty_blob_is_left_out() {
        // Waveform preview request as a CDJ-2000nexus sent it (dysentery capture S06).
        let m = Message::new(
            0x0380_036c,
            PREVIEW,
            vec![
                Field::N4(0x0108_0301),
                Field::N4(3),
                Field::N4(0xc8),
                Field::N4(0),
                Field::Blob(vec![]),
            ],
        );
        let b = m.encode();
        assert_eq!(b.len(), 32 + 4 * 5);
        assert_eq!(&b[20..25], &[0x06, 0x06, 0x06, 0x06, 0x03][..]);
        assert_eq!(decode(&b).unwrap().unwrap().0, m);
    }

    #[test]
    fn captured_menu_item() {
        // Track title item from a CDJ-2000nexus (dysentery capture S06, transaction 0x3800366).
        let mut b = vec![];
        for f in [
            Field::N4(MAGIC),
            Field::N4(0x0380_0366),
            Field::N2(MENU_ITEM),
            Field::N1(12),
            Field::Blob(vec![6, 6, 6, 2, 6, 2, 6, 6, 6, 6, 6, 6]),
            Field::N4(1),
            Field::N4(0xc8),
            Field::N4(0x2c),
            Field::Str("Loneliness - Klub Cut".into()),
            Field::N4(2),
            Field::Str(String::new()),
            Field::N4(4),
            Field::N4(0x0100_0000),
            Field::N4(0xba),
            Field::N4(0),
            Field::N4(0x100),
            Field::N4(0),
        ] {
            f.encode(&mut b);
        }
        let (m, _) = decode(&b).unwrap().unwrap();
        assert_eq!(m.kind, MENU_ITEM);
        assert_eq!(m.str(3), Some("Loneliness - Klub Cut"));
        assert_eq!(m.num(6), Some(4));
    }

    #[test]
    fn codes() {
        assert_eq!(fourcc("PWV4"), 878_073_680);
        assert_eq!(fourcc("EXT"), 5_527_621);
        assert_eq!(fourcc("PSSI"), 1_230_197_584);
        assert_eq!(dmst(1, 8, 3, 1), 0x0108_0301);
        assert_eq!(port_query().len(), 19);
    }
}
