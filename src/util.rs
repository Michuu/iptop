//! Formatting, interfaces, /etc/services.

use std::collections::HashMap;
use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const BYTE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
const BIT_UNITS: [&str; 5] = ["b", "Kb", "Mb", "Gb", "Tb"];

fn scaled(mut v: f64, units: &[&str; 5], base: f64) -> String {
    if v < 0.5 {
        return format!("0{}", units[0]);
    }
    let mut i = 0;
    while v >= base && i < 4 {
        v /= base;
        i += 1;
    }
    if i == 0 {
        format!("{:.0}{}", v, units[0])
    } else if v < 10.0 {
        format!("{:.2}{}", v, units[i])
    } else if v < 100.0 {
        format!("{:.1}{}", v, units[i])
    } else {
        format!("{:.0}{}", v, units[i])
    }
}

/// Rate given in bytes/s, shown in bits (iftop default) or bytes.
pub fn fmt_rate(bytes_per_s: f64, bytes: bool) -> String {
    if bytes {
        scaled(bytes_per_s, &BYTE_UNITS, 1024.0)
    } else {
        scaled(bytes_per_s * 8.0, &BIT_UNITS, 1000.0)
    }
}

pub fn fmt_bytes(b: u64) -> String {
    scaled(b as f64, &BYTE_UNITS, 1024.0)
}

pub fn fmt_dur(secs: usize) -> String {
    if secs >= 120 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

pub fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

pub fn has_ports(proto: u8) -> bool {
    matches!(proto, 6 | 17 | 33 | 132 | 136)
}

pub fn proto_name(p: u8) -> String {
    match p {
        1 => "icmp".into(),
        2 => "igmp".into(),
        6 => "tcp".into(),
        17 => "udp".into(),
        41 => "6in4".into(),
        47 => "gre".into(),
        50 => "esp".into(),
        51 => "ah".into(),
        58 => "icmp6".into(),
        89 => "ospf".into(),
        112 => "vrrp".into(),
        132 => "sctp".into(),
        n => format!("p{n}"),
    }
}

pub fn clock() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

/// Truncate to `w` display columns (char-based; fine for host names).
pub fn fit(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(w - 1).collect();
        t.push('…');
        t
    }
}

pub struct Services(HashMap<(u16, u8), String>);

impl Services {
    pub fn load() -> Self {
        let mut m = HashMap::new();
        let text = std::fs::read_to_string("/etc/services").unwrap_or_default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let mut it = line.split_whitespace();
            let (Some(name), Some(pp)) = (it.next(), it.next()) else { continue };
            let Some((port, proto)) = pp.split_once('/') else { continue };
            let Ok(port) = port.parse::<u16>() else { continue };
            let proto = match proto {
                "tcp" => 6,
                "udp" => 17,
                "sctp" => 132,
                _ => continue,
            };
            m.entry((port, proto)).or_insert_with(|| name.to_string());
        }
        Services(m)
    }

    pub fn name(&self, port: u16, proto: u8) -> Option<&str> {
        self.0.get(&(port, proto)).map(|s| s.as_str())
    }
}

pub fn list_ifaces() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/sys/class/net")
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub fn operstate(name: &str) -> String {
    std::fs::read_to_string(format!("/sys/class/net/{name}/operstate"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// iface -> (rx_bytes, tx_bytes)
pub fn read_netdev() -> HashMap<String, (u64, u64)> {
    let mut m = HashMap::new();
    let text = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let f: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if f.len() >= 9 {
            m.insert(name.trim().to_string(), (f[0], f[8]));
        }
    }
    m
}

/// Interface carrying the default route, else first "up" non-loopback one.
pub fn default_iface() -> String {
    let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    for line in route.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() > 1 && f[1] == "00000000" {
            return f[0].to_string();
        }
    }
    list_ifaces()
        .into_iter()
        .find(|i| i != "lo" && operstate(i) == "up")
        .unwrap_or_else(|| "any".to_string())
}

/// iface -> addresses
pub fn iface_addrs() -> HashMap<String, Vec<IpAddr>> {
    let mut m: HashMap<String, Vec<IpAddr>> = HashMap::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return m;
        }
        let mut p = ifap;
        while !p.is_null() {
            let ifa = &*p;
            if !ifa.ifa_addr.is_null() {
                let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().to_string();
                let fam = (*ifa.ifa_addr).sa_family as i32;
                let ip = if fam == libc::AF_INET {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr))))
                } else if fam == libc::AF_INET6 {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    Some(IpAddr::V6(Ipv6Addr::from(sa.sin6_addr.s6_addr)))
                } else {
                    None
                };
                if let Some(ip) = ip {
                    m.entry(name).or_default().push(ip);
                }
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    m
}

/// Home directory of `user` from the passwd database (works with musl too).
pub fn user_home(user: &str) -> Option<std::path::PathBuf> {
    let c = std::ffi::CString::new(user).ok()?;
    unsafe {
        let pw = libc::getpwnam(c.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned().into())
    }
}

/// Login name of a uid (the number if unknown).
pub fn user_name(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() || (*pw).pw_name.is_null() {
            return uid.to_string();
        }
        CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for ch in data.chunks(3) {
        let b = [ch[0], *ch.get(1).unwrap_or(&0), *ch.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= ch.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Put `text` on the clipboard: OSC 52 (works over ssh in most terminals,
/// not in GNOME's VTE-based ones), plus wl-copy / xclip / xsel when a local
/// desktop session is reachable. Returns how it was sent.
pub fn copy_to_clipboard(text: &str) -> &'static str {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
    let tools: &[(&str, &[&str], &str)] = &[
        ("WAYLAND_DISPLAY", &["wl-copy"], "wl-copy"),
        ("DISPLAY", &["xclip", "-selection", "clipboard"], "xclip"),
        ("DISPLAY", &["xsel", "-b", "-i"], "xsel"),
    ];
    for (env, cmd, name) in tools {
        if std::env::var_os(env).is_none() {
            continue;
        }
        let child = Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(text.as_bytes());
            }
            if c.wait().is_ok_and(|s| s.success()) {
                return name;
            }
        }
    }
    "terminal (OSC 52)"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_lookup() {
        assert_eq!(user_home("root"), Some(std::path::PathBuf::from("/root")));
        assert_eq!(user_home("no-such-user-iptop"), None);
    }

    #[test]
    fn b64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"8.8.8.8"), "OC44LjguOA==");
    }

    #[test]
    fn rates() {
        assert_eq!(fmt_rate(0.0, false), "0b");
        assert_eq!(fmt_rate(125.0, false), "1.00Kb");
        assert_eq!(fmt_rate(12_500_000.0, false), "100Mb");
        assert_eq!(fmt_bytes(1536), "1.50KB");
    }
}
