//! Replays a packet capture in real time, as if the devices were on the network
//! (`boothmate --replay <file>`). Loops at the end.

use super::pcap::Datagram;

pub struct Replay {
    pub name: String,
    packets: Vec<Datagram>,
    next: usize,
    /// Wall time (ms) that corresponds to the first packet.
    start: u64,
    /// Rounds played so far.
    pub rounds: u32,
}

impl Replay {
    pub fn new(name: String, packets: Vec<Datagram>, now: u64) -> Self {
        Replay {
            name,
            packets,
            next: 0,
            start: now,
            rounds: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    /// Packets due at `now`.
    pub fn tick(&mut self, now: u64) -> Vec<&Datagram> {
        let Some(t0) = self.packets.first().map(|d| d.time_us) else {
            return vec![];
        };
        if self.next >= self.packets.len() {
            // Loop after a short pause.
            let end = (self.packets[self.packets.len() - 1].time_us - t0) / 1000;
            if now < self.start + end + 2_000 {
                return vec![];
            }
            self.next = 0;
            self.start = now;
            self.rounds += 1;
        }
        let elapsed = now.saturating_sub(self.start);
        let from = self.next;
        while self.next < self.packets.len()
            && (self.packets[self.next].time_us - t0) / 1000 <= elapsed
        {
            self.next += 1;
        }
        self.packets[from..self.next].iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn plays_in_time_and_loops() {
        let d = |ms: u64| Datagram {
            time_us: 5_000_000 + ms * 1000,
            src: Ipv4Addr::LOCALHOST,
            dst: Ipv4Addr::LOCALHOST,
            src_port: 50001,
            dst_port: 50001,
            payload: vec![ms as u8],
        };
        let mut r = Replay::new("x".into(), vec![d(0), d(100), d(250)], 1_000);
        assert_eq!(r.tick(1_000).len(), 1);
        assert_eq!(r.tick(1_050).len(), 0);
        assert_eq!(r.tick(1_300).len(), 2);
        assert_eq!(r.tick(2_000).len(), 0);
        assert_eq!(r.tick(3_300).len(), 1);
        assert_eq!(r.rounds, 1);
    }
}
