//! Which process owns a flow: sockets from /proc/<pid>/net/{tcp,udp}{,6}
//! (one read per network namespace, so containers count too) matched to
//! processes through the `socket:[inode]` links in /proc/<pid>/fd.
//!
//! A background thread rebuilds the socket table every couple of seconds.
//! Once a flow has been matched it keeps its process ("sticky"), so short
//! connections stay attributed after their socket is gone.

use crate::stats::FlowKey;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub user: String,
}

/// (proto, local port, remote ip, remote port) of a connected socket.
type ConnKey = (u8, u16, IpAddr, u16);

#[derive(Default)]
struct Table {
    conn: HashMap<ConnKey, Arc<ProcInfo>>,
    /// Listening / unconnected sockets: (proto, local port).
    bound: HashMap<(u8, u16), Arc<ProcInfo>>,
}

pub struct Procs {
    table: Mutex<Table>,
    sticky: Mutex<HashMap<FlowKey, Arc<ProcInfo>>>,
    /// Some process fds couldn't be read (not root): attribution is partial.
    pub partial: Mutex<bool>,
}

const REFRESH: Duration = Duration::from_millis(1500);

impl Procs {
    pub fn start() -> Arc<Procs> {
        let p = Arc::new(Procs { table: Mutex::default(), sticky: Mutex::default(), partial: Mutex::new(false) });
        let weak = Arc::downgrade(&p);
        std::thread::Builder::new()
            .name("procs".into())
            .spawn(move || {
                while let Some(p) = weak.upgrade() {
                    let (t, partial) = scan();
                    *p.table.lock().unwrap() = t;
                    *p.partial.lock().unwrap() = partial;
                    drop(p);
                    std::thread::sleep(REFRESH);
                }
            })
            .ok();
        p
    }

    /// The process of a flow, if known.
    pub fn lookup(&self, k: &FlowKey) -> Option<Arc<ProcInfo>> {
        if let Some(p) = self.sticky.lock().unwrap().get(k) {
            return Some(p.clone());
        }
        let found = {
            let t = self.table.lock().unwrap();
            t.conn.get(&(k.proto, k.lport, k.remote, k.rport)).or_else(|| t.bound.get(&(k.proto, k.lport))).cloned()
        }?;
        self.sticky.lock().unwrap().insert(*k, found.clone());
        Some(found)
    }

    /// Forget flows that no longer exist.
    pub fn prune(&self, live: impl Fn(&FlowKey) -> bool) {
        self.sticky.lock().unwrap().retain(|k, _| live(k));
    }
}

fn scan() -> (Table, bool) {
    let mut t = Table::default();
    let mut partial = false;
    // inode -> owner, and one pid per network namespace to read its sockets.
    let mut owners: HashMap<u64, Arc<ProcInfo>> = HashMap::new();
    let mut netns: HashMap<String, u32> = HashMap::new();
    let mut users: HashMap<u32, String> = HashMap::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return (t, true) };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let base = e.path();
        let Ok(fds) = std::fs::read_dir(base.join("fd")) else {
            partial = true;
            continue;
        };
        if let Ok(ns) = std::fs::read_link(base.join("ns/net")) {
            netns.entry(ns.to_string_lossy().to_string()).or_insert(pid);
        }
        let mut info: Option<Arc<ProcInfo>> = None;
        for fd in fds.flatten() {
            let Ok(l) = std::fs::read_link(fd.path()) else { continue };
            let l = l.to_string_lossy();
            let Some(inode) = l.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse().ok())
            else {
                continue;
            };
            let info = info.get_or_insert_with(|| {
                let name = std::fs::read_to_string(base.join("comm")).unwrap_or_default().trim().to_string();
                let uid = uid_of(&base.join("status"));
                let user = users.entry(uid).or_insert_with(|| crate::util::user_name(uid)).clone();
                Arc::new(ProcInfo { pid, name, user })
            });
            owners.entry(inode).or_insert_with(|| info.clone());
        }
    }
    for pid in netns.values() {
        for (file, proto) in [("tcp", 6u8), ("tcp6", 6), ("udp", 17), ("udp6", 17)] {
            let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/net/{file}")) else { continue };
            for s in text.lines().skip(1).filter_map(parse_line) {
                let Some(p) = owners.get(&s.inode) else { continue };
                // TCP LISTEN (0x0A) and unconnected UDP have no remote end.
                if s.rport == 0 || (proto == 6 && s.state == 0x0A) {
                    t.bound.entry((proto, s.lport)).or_insert_with(|| p.clone());
                } else {
                    t.conn.insert((proto, s.lport, s.remote, s.rport), p.clone());
                }
            }
        }
    }
    (t, partial)
}

fn uid_of(status: &std::path::Path) -> u32 {
    std::fs::read_to_string(status)
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Uid:").and_then(|v| v.split_whitespace().next()?.parse().ok())))
        .unwrap_or(0)
}

struct Sock {
    lport: u16,
    remote: IpAddr,
    rport: u16,
    state: u8,
    inode: u64,
}

/// One line of /proc/net/{tcp,udp}{,6}.
fn parse_line(l: &str) -> Option<Sock> {
    let f: Vec<&str> = l.split_whitespace().collect();
    if f.len() < 10 {
        return None;
    }
    let (_, lport) = parse_addr(f[1])?;
    let (remote, rport) = parse_addr(f[2])?;
    let state = u8::from_str_radix(f[3], 16).ok()?;
    let inode = f[9].parse().ok()?;
    if inode == 0 {
        return None;
    }
    Some(Sock { lport, remote, rport, state, inode })
}

/// "0100007F:0035" or a 32-hex-digit IPv6 address; each 32-bit word is in
/// host (little-endian) byte order. IPv4-mapped IPv6 becomes IPv4.
fn parse_addr(s: &str) -> Option<(IpAddr, u16)> {
    let (a, p) = s.split_once(':')?;
    let port = u16::from_str_radix(p, 16).ok()?;
    let word = |i: usize| u32::from_str_radix(&a[i * 8..i * 8 + 8], 16).ok().map(|w| w.to_le_bytes());
    let ip = match a.len() {
        8 => IpAddr::V4(Ipv4Addr::from(word(0)?)),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            let v6 = Ipv6Addr::from(b);
            v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4)
        }
        _ => return None,
    };
    Some((ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addrs() {
        assert_eq!(parse_addr("0100007F:0035"), Some(("127.0.0.1".parse().unwrap(), 53)));
        assert_eq!(parse_addr("00000000000000000000000001000000:0016"), Some(("::1".parse().unwrap(), 22)));
        assert_eq!(parse_addr("0000000000000000FFFF00000100007F:01BB"), Some(("127.0.0.1".parse().unwrap(), 443)));
        let l = "   1: 0100007F:0035 08080808:01BB 01 00000000:00000000 00:00000000 00000000   101        0 12345 1";
        let s = parse_line(l).unwrap();
        assert_eq!((s.lport, s.remote, s.rport, s.state, s.inode), (53, "8.8.8.8".parse().unwrap(), 443, 1, 12345));
    }

    #[test]
    fn own_sockets() {
        // A socket of ours shows up with our pid after a scan.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (t, _) = scan();
        let p = t.bound.get(&(6, port)).expect("listener found");
        assert_eq!(p.pid, std::process::id());
    }
}
