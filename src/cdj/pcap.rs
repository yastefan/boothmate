//! Reading and writing packet captures, so DJ Link traffic can be recorded in the club and
//! replayed later without CDJs.
//!
//! Reads pcap and pcapng (as written by Wireshark and tcpdump) with Ethernet, raw IPv4, BSD
//! loopback, Linux cooked and macOS pktap link types. Writes pcap with raw IPv4 frames, which
//! Wireshark opens directly.

use std::io::{self, Write};
use std::net::Ipv4Addr;

/// One UDP datagram from a capture.
#[derive(Debug, Clone, PartialEq)]
pub struct Datagram {
    /// Capture time in microseconds since 1970.
    pub time_us: u64,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: Vec<u8>,
}

const LINK_NULL: u32 = 0;
const LINK_ETHERNET: u32 = 1;
const LINK_RAW: u32 = 101;
const LINK_LINUX_SLL: u32 = 113;
const LINK_IPV4: u32 = 228;
const LINK_PKTAP: u32 = 258;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Reads all UDP/IPv4 datagrams from a pcap or pcapng file.
pub fn read(data: &[u8]) -> io::Result<Vec<Datagram>> {
    if data.len() < 24 {
        return Err(invalid("file too short for a capture"));
    }
    match &data[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => read_pcap(data, true),
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => read_pcap(data, false),
        [0x0a, 0x0d, 0x0d, 0x0a] => read_pcapng(data),
        _ => Err(invalid("not a pcap or pcapng file")),
    }
}

struct Rd<'a> {
    d: &'a [u8],
    le: bool,
}

impl Rd<'_> {
    fn u16(&self, o: usize) -> u16 {
        let b = [self.d[o], self.d[o + 1]];
        if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }
    fn u32(&self, o: usize) -> u32 {
        let b = [self.d[o], self.d[o + 1], self.d[o + 2], self.d[o + 3]];
        if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }
}

fn read_pcap(data: &[u8], le: bool) -> io::Result<Vec<Datagram>> {
    let r = Rd { d: data, le };
    let nanos = matches!(
        data[..4],
        [0x4d, 0x3c, 0xb2, 0xa1] | [0xa1, 0xb2, 0x3c, 0x4d]
    );
    let link = r.u32(20) & 0x0fff_ffff;
    let mut out = vec![];
    let mut o = 24;
    while o + 16 <= data.len() {
        let (sec, frac, incl) = (r.u32(o) as u64, r.u32(o + 4) as u64, r.u32(o + 8) as usize);
        let start = o + 16;
        let end = start + incl;
        if end > data.len() {
            break;
        }
        let us = sec * 1_000_000 + if nanos { frac / 1000 } else { frac };
        if let Some(d) = frame(link, &data[start..end], us) {
            out.push(d);
        }
        o = end;
    }
    Ok(out)
}

fn read_pcapng(data: &[u8]) -> io::Result<Vec<Datagram>> {
    // Byte order from the section header's magic.
    let le = data[8..12] == [0x4d, 0x3c, 0x2b, 0x1a];
    let r = Rd { d: data, le };
    let mut links: Vec<(u32, u64)> = vec![]; // (link type, ticks per second)
    let mut out = vec![];
    let mut o = 0;
    while o + 12 <= data.len() {
        let ty = r.u32(o);
        let len = r.u32(o + 4) as usize;
        if len < 12 || o + len > data.len() {
            break;
        }
        let body = o + 8;
        match ty {
            0x0a0d_0d0a => links.clear(),
            1 => {
                // Interface description: link type, options (if_tsresol = 9).
                let link = r.u16(body) as u32;
                let mut res = 1_000_000u64;
                let mut p = body + 8;
                while p + 4 <= o + len - 4 {
                    let (code, olen) = (r.u16(p), r.u16(p + 2) as usize);
                    if code == 0 {
                        break;
                    }
                    if code == 9 && olen >= 1 {
                        let v = data[p + 4];
                        res = if v & 0x80 != 0 {
                            1u64 << (v & 0x7f)
                        } else {
                            10u64.pow(v as u32)
                        };
                    }
                    p += 4 + olen.div_ceil(4) * 4;
                }
                links.push((link, res));
            }
            6 => {
                // Enhanced packet block.
                let iface = r.u32(body) as usize;
                let ts = ((r.u32(body + 4) as u64) << 32) | r.u32(body + 8) as u64;
                let cap = r.u32(body + 12) as usize;
                let start = body + 20;
                if let Some(&(link, res)) = links.get(iface) {
                    if start + cap <= o + len {
                        let us = (ts as u128 * 1_000_000 / res as u128) as u64;
                        if let Some(d) = frame(link, &data[start..start + cap], us) {
                            out.push(d);
                        }
                    }
                }
            }
            3 => {
                // Simple packet block: no timestamp.
                let start = body + 4;
                if let Some(&(link, _)) = links.first() {
                    let cap = (len - 16).min(r.u32(body) as usize);
                    if let Some(d) = frame(link, &data[start..start + cap], 0) {
                        out.push(d);
                    }
                }
            }
            _ => {}
        }
        o += len;
    }
    Ok(out)
}

/// Strips the link layer and parses IPv4 + UDP.
fn frame(link: u32, f: &[u8], time_us: u64) -> Option<Datagram> {
    let ip = match link {
        LINK_ETHERNET => ethernet(f)?,
        LINK_RAW | LINK_IPV4 => f,
        LINK_NULL => f.get(4..)?,
        LINK_LINUX_SLL => {
            (u16::from_be_bytes([*f.get(14)?, *f.get(15)?]) == 0x0800).then(|| &f[16..])?
        }
        LINK_PKTAP => {
            // macOS pktap: little-endian header length and inner link type, then the frame.
            let hl = u32::from_le_bytes(f.get(0..4)?.try_into().ok()?) as usize;
            let inner = u32::from_le_bytes(f.get(4..8)?.try_into().ok()?);
            return frame(inner, f.get(hl..)?, time_us);
        }
        _ => return None,
    };
    if ip.len() < 20 || ip[0] >> 4 != 4 || ip[9] != 17 {
        return None;
    }
    let ihl = (ip[0] & 0x0f) as usize * 4;
    let total = (u16::from_be_bytes([ip[2], ip[3]]) as usize).min(ip.len());
    // Fragments are not reassembled; DJ Link packets fit into one frame.
    if u16::from_be_bytes([ip[6], ip[7]]) & 0x3fff != 0 {
        return None;
    }
    let udp = ip.get(ihl..total)?;
    if udp.len() < 8 {
        return None;
    }
    Some(Datagram {
        time_us,
        src: Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]),
        dst: Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]),
        src_port: u16::from_be_bytes([udp[0], udp[1]]),
        dst_port: u16::from_be_bytes([udp[2], udp[3]]),
        payload: udp[8..].to_vec(),
    })
}

fn ethernet(f: &[u8]) -> Option<&[u8]> {
    let mut o = 12;
    let mut ty = u16::from_be_bytes([*f.get(o)?, *f.get(o + 1)?]);
    // VLAN tags.
    while ty == 0x8100 || ty == 0x88a8 {
        o += 4;
        ty = u16::from_be_bytes([*f.get(o)?, *f.get(o + 1)?]);
    }
    (ty == 0x0800).then(|| f.get(o + 2..))?
}

/// Writes a pcap file with raw IPv4 frames.
pub struct Writer<W: Write> {
    w: W,
}

impl<W: Write> Writer<W> {
    pub fn new(mut w: W) -> io::Result<Self> {
        let mut h = Vec::with_capacity(24);
        h.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&[0; 8]);
        h.extend_from_slice(&65_535u32.to_le_bytes());
        h.extend_from_slice(&LINK_IPV4.to_le_bytes());
        w.write_all(&h)?;
        Ok(Writer { w })
    }

    pub fn write(&mut self, d: &Datagram) -> io::Result<()> {
        let udp_len = 8 + d.payload.len();
        let total = 20 + udp_len;
        let mut f = Vec::with_capacity(16 + total);
        f.extend_from_slice(&((d.time_us / 1_000_000) as u32).to_le_bytes());
        f.extend_from_slice(&((d.time_us % 1_000_000) as u32).to_le_bytes());
        f.extend_from_slice(&(total as u32).to_le_bytes());
        f.extend_from_slice(&(total as u32).to_le_bytes());
        let mut ip = [0u8; 20];
        ip[0] = 0x45;
        ip[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        ip[8] = 64;
        ip[9] = 17;
        ip[12..16].copy_from_slice(&d.src.octets());
        ip[16..20].copy_from_slice(&d.dst.octets());
        let sum = checksum(&ip);
        ip[10..12].copy_from_slice(&sum.to_be_bytes());
        f.extend_from_slice(&ip);
        f.extend_from_slice(&d.src_port.to_be_bytes());
        f.extend_from_slice(&d.dst_port.to_be_bytes());
        f.extend_from_slice(&(udp_len as u16).to_be_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&d.payload);
        self.w.write_all(&f)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

fn checksum(h: &[u8]) -> u16 {
    let mut s: u32 = h
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]) as u32)
        .sum();
    while s > 0xffff {
        s = (s & 0xffff) + (s >> 16);
    }
    !(s as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dg(t: u64, port: u16, payload: &[u8]) -> Datagram {
        Datagram {
            time_us: t,
            src: Ipv4Addr::new(169, 254, 1, 2),
            dst: Ipv4Addr::new(169, 254, 255, 255),
            src_port: port,
            dst_port: port,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn write_then_read() {
        let mut buf = vec![];
        let mut w = Writer::new(&mut buf).unwrap();
        let a = dg(1_700_000_000_123_456, 50000, b"hello");
        let b = dg(1_700_000_001_000_000, 50001, &[0u8; 96]);
        w.write(&a).unwrap();
        w.write(&b).unwrap();
        assert_eq!(read(&buf).unwrap(), vec![a, b]);
    }

    #[test]
    fn ethernet_with_vlan_in_pcapng() {
        // Minimal pcapng: section header, interface (Ethernet, default µs), one enhanced packet.
        let mut ip = vec![
            0x45, 0, 0, 0, 0, 0, 0x40, 0, 64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 255,
        ];
        let payload = b"Qspt1WmJOL";
        let total = (20 + 8 + payload.len()) as u16;
        ip[2..4].copy_from_slice(&total.to_be_bytes());
        let mut frame = vec![0xff; 6];
        frame.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0x81, 0x00, 0, 5, 0x08, 0x00]);
        frame.extend_from_slice(&ip);
        frame.extend_from_slice(&[0xc3, 0x50, 0xc3, 0x51, 0, 18, 0, 0]);
        frame.extend_from_slice(payload);
        while !frame.len().is_multiple_of(4) {
            frame.push(0);
        }
        let mut f = vec![];
        let block = |f: &mut Vec<u8>, ty: u32, body: &[u8]| {
            let len = (12 + body.len()) as u32;
            f.extend_from_slice(&ty.to_le_bytes());
            f.extend_from_slice(&len.to_le_bytes());
            f.extend_from_slice(body);
            f.extend_from_slice(&len.to_le_bytes());
        };
        let mut shb = 0x1a2b_3c4du32.to_le_bytes().to_vec();
        shb.extend_from_slice(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        block(&mut f, 0x0a0d_0d0a, &shb);
        block(&mut f, 1, &[1, 0, 0, 0, 0, 0, 0, 0]);
        let mut epb = vec![0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x42, 0x0f, 0];
        let cap = (frame.len() as u32).to_le_bytes();
        epb.extend_from_slice(&cap);
        epb.extend_from_slice(&cap);
        epb.extend_from_slice(&frame);
        block(&mut f, 6, &epb);
        let d = read(&f).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].time_us, 1_000_000);
        assert_eq!((d[0].src_port, d[0].dst_port), (50000, 50001));
        assert_eq!(d[0].src, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(&d[0].payload, payload);
    }
}
