//! The table's other views (Tab): traffic per service, country and network,
//! a log of new hosts, and all interfaces with their own history.

use crate::app::SPARK;
use crate::filter::is_internal;
use crate::geo::Geo;
use crate::procs::Procs;
use crate::stats::{FlowKey, Stats};
use crate::util::{self, has_ports, Services};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Hosts,
    Procs,
    Services,
    Countries,
    Networks,
    Log,
    Ifaces,
}

impl View {
    pub const ALL: [View; 7] = [View::Hosts, View::Procs, View::Services, View::Countries, View::Networks, View::Log, View::Ifaces];

    pub fn name(self) -> &'static str {
        match self {
            View::Hosts => "hosts",
            View::Procs => "processes",
            View::Services => "services",
            View::Countries => "countries",
            View::Networks => "networks",
            View::Log => "new hosts",
            View::Ifaces => "interfaces",
        }
    }

    pub fn step(self, d: i64) -> View {
        let i = Self::ALL.iter().position(|v| *v == self).unwrap_or(0) as i64;
        Self::ALL[(i + d).rem_euclid(Self::ALL.len() as i64) as usize]
    }

    pub fn is_agg(self) -> bool {
        matches!(self, View::Procs | View::Services | View::Countries | View::Networks)
    }
}

/// One row of the services / countries / networks views.
pub struct Agg {
    pub key: String,
    pub label: String,
    pub detail: String,
    /// Filter expression that narrows the hosts view to this row (Enter).
    pub filter: Option<String>,
    pub hosts: usize,
    pub nflows: usize,
    pub sent: [f64; 3],
    pub recv: [f64; 3],
    pub tot_sent: u64,
    pub tot_recv: u64,
    pub spark: [u64; SPARK],
}

/// The aggregation key, label, detail and filter of a flow in `view`.
fn agg_key(view: View, k: &FlowKey, st: &Stats, geo: &Geo, services: &Services, procs: &Procs) -> (String, String, String, Option<String>) {
    let internal = || is_internal(&k.remote, &st.filter.internal_nets);
    match view {
        View::Procs => match procs.lookup(k) {
            // Grouped by name (all curl runs together); the detail lists the pids.
            Some(p) => (p.name.clone(), p.name.clone(), format!("{} · {}", p.user, p.pid), Some(format!("proc '{}'", p.name))),
            None => ("~none".into(), "—".into(), "no local process (forwarded, ended, or not visible)".into(), None),
        },
        View::Services => {
            let proto = util::proto_name(k.proto);
            if !has_ports(k.proto) {
                return (proto.clone(), proto, String::new(), Some(format!("proto {}", k.proto)));
            }
            // The service is the well-known side: the remote port for our
            // outgoing connections, the local one for servers we run.
            let named = |p: u16| services.name(p, k.proto).map(str::to_string);
            let (port, name) = match (named(k.rport), named(k.lport)) {
                (Some(n), _) => (k.rport, n),
                (None, Some(n)) => (k.lport, n),
                (None, None) => {
                    let p = k.rport.min(k.lport);
                    (p, p.to_string())
                }
            };
            (format!("{proto}/{port}"), name, format!("{proto}/{port}"), Some(format!("{proto} port {port}")))
        }
        View::Countries => match geo.lookup(&k.remote).and_then(|g| g.cc.map(|cc| (cc, g.country))) {
            Some((cc, name)) => (cc.clone(), cc.clone(), name.unwrap_or_default(), Some(format!("cc {cc}"))),
            None if internal() => ("~int".into(), "internal".into(), "own and private networks".into(), Some("internal".into())),
            None => ("~unk".into(), "?".into(), "no GeoIP data".into(), None),
        },
        View::Networks => match geo.lookup(&k.remote).and_then(|g| g.asn.map(|a| (a, g.as_org, g.cc))) {
            Some((asn, org, cc)) => {
                let detail = [cc.unwrap_or_default(), org.unwrap_or_default()].join(" ").trim().to_string();
                (format!("{asn}"), format!("AS{asn}"), detail, Some(format!("asn {asn}")))
            }
            None if internal() => ("~int".into(), "internal".into(), "own and private networks".into(), Some("internal".into())),
            None => ("~unk".into(), "?".into(), "no ASN data".into(), None),
        },
        _ => unreachable!(),
    }
}

/// Aggregate the filtered flows for an aggregate view.
pub fn build(view: View, st: &Stats, now: u64, windows: &[u64; 3], geo: &Geo, services: &Services, procs: &Procs) -> Vec<Agg> {
    let mut map: HashMap<String, (Agg, HashSet<IpAddr>)> = HashMap::new();
    for (k, f) in st.flows.iter() {
        if !st.filter.matches(k) {
            continue;
        }
        let (key, label, detail, filter) = agg_key(view, k, st, geo, services, procs);
        let (a, hosts) = map.entry(key.clone()).or_insert_with(|| {
            (
                Agg {
                    key,
                    label,
                    detail,
                    filter,
                    hosts: 0,
                    nflows: 0,
                    sent: [0.0; 3],
                    recv: [0.0; 3],
                    tot_sent: 0,
                    tot_recv: 0,
                    spark: [0; SPARK],
                },
                HashSet::new(),
            )
        });
        hosts.insert(k.remote);
        if view == View::Procs && a.key != "~none" {
            // detail is "user · pid pid …"; add this flow's pid if new.
            if let Some(p) = procs.lookup(k) {
                let pid = p.pid.to_string();
                if !a.detail.split(' ').any(|w| w == pid) {
                    a.detail.push(' ');
                    a.detail.push_str(&pid);
                }
            }
        }
        a.nflows += 1;
        for (i, n) in windows.iter().enumerate() {
            let (s, r) = f.hist.avg(now, *n);
            a.sent[i] += s;
            a.recv[i] += r;
        }
        a.tot_sent += f.hist.total_sent;
        a.tot_recv += f.hist.total_recv;
        for j in 0..SPARK {
            let back = (j + 1) as u64;
            if back <= now {
                let (s, r) = f.hist.get(now - back);
                a.spark[SPARK - 1 - j] += s + r;
            }
        }
    }
    map.into_values()
        .map(|(mut a, h)| {
            a.hosts = h.len();
            a
        })
        .collect()
}

/// A host seen for the first time (since start, reset or interface switch).
pub struct LogEntry {
    pub time: String,
    pub ip: IpAddr,
    /// First flow's service, e.g. "tcp/443 https".
    pub service: String,
    pub geo: String,
    /// Process of the first flow, if known yet.
    pub proc: String,
}

pub const LOG_MAX: usize = 2000;

#[derive(Default)]
pub struct Log {
    seen: HashSet<IpAddr>,
    /// Newest first.
    pub entries: VecDeque<LogEntry>,
}

impl Log {
    /// Record remote hosts of filtered flows that weren't seen before.
    pub fn update(&mut self, st: &Stats, geo: &Geo, services: &Services, procs: &Procs) {
        let mut new: Vec<&FlowKey> = st.flows.keys().filter(|k| !self.seen.contains(&k.remote) && st.filter.matches(k)).collect();
        new.sort_by_key(|k| (k.remote, k.rport));
        for k in new {
            if !self.seen.insert(k.remote) {
                continue;
            }
            let proto = util::proto_name(k.proto);
            let service = if has_ports(k.proto) {
                let name = services.name(k.rport, k.proto).or(services.name(k.lport, k.proto));
                let port = if services.name(k.rport, k.proto).is_some() || services.name(k.lport, k.proto).is_none() {
                    k.rport
                } else {
                    k.lport
                };
                format!("{proto}/{port} {}", name.unwrap_or(""))
            } else {
                proto
            };
            self.entries.push_front(LogEntry {
                time: util::clock(),
                ip: k.remote,
                service: service.trim().to_string(),
                geo: geo.lookup(&k.remote).map(|g| g.short()).unwrap_or_default(),
                proc: procs.lookup(k).map(|p| p.name.clone()).unwrap_or_default(),
            });
        }
        self.entries.truncate(LOG_MAX);
    }

    pub fn clear(&mut self) {
        self.seen.clear();
        self.entries.clear();
    }
}

/// Per-interface rate history from /proc/net/dev, sampled every update.
pub struct IfHist {
    prev: (HashMap<String, (u64, u64)>, Instant),
    /// name -> (rx, tx) bytes/s, oldest first
    pub hist: HashMap<String, VecDeque<(f64, f64)>>,
}

pub const IF_HIST: usize = 120;

impl IfHist {
    pub fn new() -> IfHist {
        IfHist { prev: (util::read_netdev(), Instant::now()), hist: HashMap::new() }
    }

    pub fn sample(&mut self) {
        let el = self.prev.1.elapsed().as_secs_f64();
        if el < 0.2 {
            return;
        }
        let now = util::read_netdev();
        for (name, (rx, tx)) in &now {
            if let Some((prx, ptx)) = self.prev.0.get(name) {
                let h = self.hist.entry(name.clone()).or_default();
                h.push_back((rx.saturating_sub(*prx) as f64 / el, tx.saturating_sub(*ptx) as f64 / el));
                while h.len() > IF_HIST {
                    h.pop_front();
                }
            }
        }
        self.hist.retain(|n, _| now.contains_key(n));
        self.prev = (now, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_cycle() {
        assert_eq!(View::Hosts.step(1), View::Procs);
        assert_eq!(View::Hosts.step(-1), View::Ifaces);
        let mut v = View::Hosts;
        for _ in 0..View::ALL.len() {
            v = v.step(1);
        }
        assert_eq!(v, View::Hosts);
        assert!(View::Networks.is_agg() && !View::Log.is_agg());
    }
}
