//! Traffic accounting: per-flow ring buffers of 1-second slots.

use crate::filter::Filter;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Seconds of history kept per flow (enough for the longest averaging window).
pub const FLOW_HIST: usize = 304;
/// Seconds of history kept for the totals graph.
pub const TOTAL_HIST: usize = 900;
/// A flow is forgotten after this many idle seconds (at least; see expire()).
pub const FLOW_IDLE: u64 = 60;

/// One conversation as seen from this machine. `local` is the side that is
/// ours (packet sent from it / received by it), `remote` the peer.
#[derive(Clone, Copy, Hash, Eq, PartialEq, Debug)]
pub struct FlowKey {
    pub proto: u8,
    pub local: IpAddr,
    pub lport: u16,
    pub remote: IpAddr,
    pub rport: u16,
}

/// Ring buffer of per-second sent/received byte counts.
#[derive(Clone, Debug)]
pub struct Hist {
    len: u64,
    // u32 per 1-second slot keeps long flow histories cheap (saturates at ~34 Gbit/s).
    sent: Vec<u32>,
    recv: Vec<u32>,
    last: u64,
    pub total_sent: u64,
    pub total_recv: u64,
}

impl Hist {
    pub fn new(len: usize, slot: u64) -> Self {
        Hist {
            len: len as u64,
            sent: vec![0; len],
            recv: vec![0; len],
            last: slot,
            total_sent: 0,
            total_recv: 0,
        }
    }

    fn advance(&mut self, slot: u64) {
        if slot <= self.last {
            return;
        }
        let gap = (slot - self.last).min(self.len);
        for i in 1..=gap {
            let idx = ((self.last + i) % self.len) as usize;
            self.sent[idx] = 0;
            self.recv[idx] = 0;
        }
        self.last = slot;
    }

    pub fn add(&mut self, slot: u64, sent: u64, recv: u64) {
        self.advance(slot);
        self.total_sent += sent;
        self.total_recv += recv;
        if self.last - slot >= self.len {
            return;
        }
        let idx = (slot % self.len) as usize;
        self.sent[idx] = self.sent[idx].saturating_add(sent.min(u32::MAX as u64) as u32);
        self.recv[idx] = self.recv[idx].saturating_add(recv.min(u32::MAX as u64) as u32);
    }

    pub fn last_slot(&self) -> u64 {
        self.last
    }

    /// (sent, recv) bytes in a given slot.
    pub fn get(&self, slot: u64) -> (u64, u64) {
        if slot > self.last || self.last - slot >= self.len {
            return (0, 0);
        }
        let idx = (slot % self.len) as usize;
        (self.sent[idx] as u64, self.recv[idx] as u64)
    }

    /// Average bytes/s over the `n` complete slots before `now`.
    pub fn avg(&self, now: u64, n: u64) -> (f64, f64) {
        let (mut s, mut r) = (0u64, 0u64);
        for k in 1..=n {
            if k > now {
                break;
            }
            let (a, b) = self.get(now - k);
            s += a;
            r += b;
        }
        (s as f64 / n as f64, r as f64 / n as f64)
    }

    /// Last `n` complete slots, oldest first: (sent, recv).
    pub fn series(&self, now: u64, n: usize) -> (Vec<f64>, Vec<f64>) {
        let mut s = vec![0.0; n];
        let mut r = vec![0.0; n];
        for i in 0..n {
            let k = (n - i) as u64;
            if k > now {
                continue;
            }
            let (a, b) = self.get(now - k);
            s[i] = a as f64;
            r[i] = b as f64;
        }
        (s, r)
    }
}

pub struct Flow {
    pub hist: Hist,
}

pub struct Stats {
    start: Instant,
    pub flows: HashMap<FlowKey, Flow>,
    /// Everything seen on the interface.
    pub total: Hist,
    /// Only what passes the current filter.
    pub filtered: Hist,
    pub filter: Filter,
    pub error: Option<String>,
    pub pkts: u64,
}

pub type SharedStats = Arc<Mutex<Stats>>;

impl Stats {
    pub fn new(filter: Filter) -> Self {
        Stats {
            start: Instant::now(),
            flows: HashMap::new(),
            total: Hist::new(TOTAL_HIST, 0),
            filtered: Hist::new(TOTAL_HIST, 0),
            filter,
            error: None,
            pkts: 0,
        }
    }

    pub fn slot(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    pub fn ingest(&mut self, batch: &HashMap<FlowKey, [u64; 2]>, pkts: u64) {
        let slot = self.slot();
        self.pkts += pkts;
        for (k, v) in batch {
            let flow = self.flows.entry(*k).or_insert_with(|| Flow {
                hist: Hist::new(FLOW_HIST, slot),
            });
            flow.hist.add(slot, v[0], v[1]);
            self.total.add(slot, v[0], v[1]);
            if self.filter.matches(k) {
                self.filtered.add(slot, v[0], v[1]);
            }
        }
    }

    pub fn expire(&mut self, idle: u64) {
        let slot = self.slot();
        let idle = idle.max(FLOW_IDLE);
        self.flows
            .retain(|_, f| slot.saturating_sub(f.hist.last_slot()) <= idle);
    }

    /// Install a new filter and rebuild the filtered totals from flow history
    /// (so the graph reacts immediately, at least for the last minute).
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        let now = self.slot();
        let mut h = Hist::new(TOTAL_HIST, now);
        let (mut ts, mut tr) = (0, 0);
        for (k, f) in &self.flows {
            if !self.filter.matches(k) {
                continue;
            }
            ts += f.hist.total_sent;
            tr += f.hist.total_recv;
            for back in 0..FLOW_HIST as u64 {
                if back > now {
                    break;
                }
                let slot = now - back;
                let (s, r) = f.hist.get(slot);
                if s + r > 0 {
                    h.add(slot, s, r);
                }
            }
        }
        h.total_sent = ts;
        h.total_recv = tr;
        self.filtered = h;
    }

    pub fn reset(&mut self) {
        let now = self.slot();
        self.flows.clear();
        self.total = Hist::new(TOTAL_HIST, now);
        self.filtered = Hist::new(TOTAL_HIST, now);
        self.error = None;
        self.pkts = 0;
    }
}
