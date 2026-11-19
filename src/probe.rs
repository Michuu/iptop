//! Host panel probes: continuous ping and a traceroute, run as child
//! processes (system `ping` and `traceroute`/`tracepath`).

use std::io::{BufRead, BufReader};
use std::net::IpAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Hop {
    pub ttl: u8,
    pub ip: Option<IpAddr>,
    pub rtt: Option<f64>,
}

#[derive(Default)]
pub struct ProbeState {
    /// (icmp_seq, rtt ms)
    pub replies: Vec<(u64, f64)>,
    pub max_seq: u64,
    pub ping_err: Option<String>,
    pub hops: Vec<Hop>,
    pub trace_tool: String,
    pub trace_done: bool,
    pub trace_err: Option<String>,
}

impl ProbeState {
    pub fn loss(&self) -> Option<f64> {
        // The newest seq may still be in flight; ignore it.
        let sent = self.max_seq.saturating_sub(1);
        if sent == 0 {
            return None;
        }
        let got = self.replies.iter().filter(|(s, _)| *s <= sent).count() as f64;
        Some((1.0 - got / sent as f64).max(0.0) * 100.0)
    }

    pub fn rtt_stats(&self) -> Option<(f64, f64, f64, f64)> {
        let last = self.replies.last()?.1;
        let mut min = f64::MAX;
        let mut max: f64 = 0.0;
        let mut sum = 0.0;
        for (_, r) in &self.replies {
            min = min.min(*r);
            max = max.max(*r);
            sum += r;
        }
        Some((last, min, sum / self.replies.len() as f64, max))
    }
}

pub struct Probe {
    pub ip: IpAddr,
    pub state: Arc<Mutex<ProbeState>>,
    children: Arc<Mutex<Vec<Child>>>,
}

impl Probe {
    pub fn start(ip: IpAddr) -> Probe {
        let state = Arc::new(Mutex::new(ProbeState::default()));
        let children = Arc::new(Mutex::new(Vec::new()));
        let p = Probe { ip, state, children };
        p.spawn_ping();
        p.spawn_trace();
        p
    }

    fn spawn_ping(&self) {
        let ip = self.ip.to_string();
        let mut cmd = Command::new("ping");
        cmd.args(["-n", "-i", "1", "-W", "2", &ip]);
        let state = self.state.clone();
        match cmd.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn() {
            Err(e) => state.lock().unwrap().ping_err = Some(format!("ping: {e}")),
            Ok(mut child) => {
                let out = child.stdout.take();
                self.children.lock().unwrap().push(child);
                std::thread::spawn(move || {
                    let Some(out) = out else { return };
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if let Some((seq, rtt)) = parse_ping(&line) {
                            let mut s = state.lock().unwrap();
                            s.max_seq = s.max_seq.max(seq);
                            if let Some(r) = rtt {
                                s.replies.push((seq, r));
                            }
                        }
                    }
                });
                // Advance max_seq even if no replies come back, so loss shows.
                let state = self.state.clone();
                let children = self.children.clone();
                std::thread::spawn(move || {
                    let start = std::time::Instant::now();
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(1000));
                        if children.lock().unwrap().is_empty() {
                            break;
                        }
                        let mut s = state.lock().unwrap();
                        let expected = start.elapsed().as_secs();
                        if expected > s.max_seq + 2 {
                            s.max_seq = expected - 1;
                        }
                    }
                });
            }
        }
    }

    fn spawn_trace(&self) {
        let ip = self.ip.to_string();
        let state = self.state.clone();
        let root = unsafe { libc::geteuid() } == 0;
        let mut tries: Vec<(&str, &str, Vec<&str>)> = Vec::new();
        if root {
            // ICMP probes get through far more firewalls than UDP ones.
            tries.push(("traceroute", "traceroute -I", vec!["-n", "-I", "-q", "1", "-w", "1", "-m", "30", &ip]));
        }
        tries.push(("traceroute", "traceroute", vec!["-n", "-q", "1", "-w", "1", "-m", "30", &ip]));
        tries.push(("tracepath", "tracepath", vec!["-n", &ip]));
        for (tool, label, args) in tries {
            let child = Command::new(tool)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn();
            let Ok(mut child) = child else { continue };
            state.lock().unwrap().trace_tool = label.to_string();
            let out = child.stdout.take();
            self.children.lock().unwrap().push(child);
            std::thread::spawn(move || {
                if let Some(out) = out {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if let Some(h) = parse_hop(&line) {
                            let mut s = state.lock().unwrap();
                            match s.hops.iter_mut().find(|x| x.ttl == h.ttl) {
                                Some(x) => {
                                    if x.ip.is_none() {
                                        *x = h;
                                    }
                                }
                                None => s.hops.push(h),
                            }
                        }
                    }
                }
                state.lock().unwrap().trace_done = true;
            });
            return;
        }
        let mut s = state.lock().unwrap();
        s.trace_err = Some("no traceroute/tracepath found (apt install traceroute)".into());
        s.trace_done = true;
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let mut ch = self.children.lock().unwrap();
        for c in ch.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        ch.clear();
    }
}

/// "64 bytes from 1.2.3.4: icmp_seq=3 ttl=57 time=12.3 ms" -> (3, Some(12.3))
/// "From 1.2.3.4 icmp_seq=3 Destination Host Unreachable" -> (3, None)
pub fn parse_ping(line: &str) -> Option<(u64, Option<f64>)> {
    let seq = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("icmp_seq="))
        .and_then(|s| s.parse().ok())?;
    let rtt = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("time="))
        .and_then(|s| s.trim_end_matches("ms").parse().ok());
    Some((seq, rtt))
}

/// Handles both traceroute (" 3  10.0.0.1  1.234 ms") and
/// tracepath (" 3:  10.0.0.1   1.234ms") output.
pub fn parse_hop(line: &str) -> Option<Hop> {
    let mut it = line.split_whitespace();
    let first = it.next()?;
    let digits: String = first.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || first.contains("LOCALHOST") || line.contains("[LOCALHOST]") {
        return None;
    }
    let ttl: u8 = digits.parse().ok()?;
    let rest: Vec<&str> = it.collect();
    let ip = rest.iter().find_map(|w| w.trim_matches(|c| c == '(' || c == ')').parse::<IpAddr>().ok());
    let mut rtt = None;
    for (i, w) in rest.iter().enumerate() {
        if let Some(v) = w.strip_suffix("ms") {
            if let Ok(x) = v.parse::<f64>() {
                rtt = Some(x);
                break;
            }
        }
        if *w == "ms" && i > 0 {
            if let Ok(x) = rest[i - 1].parse::<f64>() {
                rtt = Some(x);
                break;
            }
        }
    }
    Some(Hop { ttl, ip, rtt })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ping_lines() {
        assert_eq!(
            parse_ping("64 bytes from 1.2.3.4: icmp_seq=3 ttl=57 time=12.3 ms"),
            Some((3, Some(12.3)))
        );
        assert_eq!(parse_ping("From 1.2.3.4 icmp_seq=4 Destination Host Unreachable"), Some((4, None)));
        assert_eq!(parse_ping("PING 1.2.3.4 (1.2.3.4) 56(84) bytes of data."), None);
    }
    #[test]
    fn hop_lines() {
        let h = parse_hop(" 3  10.0.0.1  1.234 ms").unwrap();
        assert_eq!((h.ttl, h.ip, h.rtt), (3, Some("10.0.0.1".parse().unwrap()), Some(1.234)));
        let h = parse_hop(" 4:  192.168.1.1          0.512ms ").unwrap();
        assert_eq!((h.ttl, h.rtt), (4, Some(0.512)));
        let h = parse_hop(" 5  *").unwrap();
        assert!(h.ip.is_none());
        assert!(parse_hop("traceroute to 8.8.8.8 (8.8.8.8), 30 hops max").is_none());
        assert!(parse_hop(" 1?: [LOCALHOST]                      pmtu 1500").is_none());
    }
}
