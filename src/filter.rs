//! Flow filtering: a "scope" (all / external / internal) plus a small
//! BPF-flavoured expression language evaluated on flows.
//!
//! Grammar:
//!   expr    := and ( ("or" | "||") and )*
//!   and     := unary ( ["and" | "&&"] unary )*
//!   unary   := ("not" | "!") unary | "(" expr ")" | prim
//!   prim    := host CIDR | net CIDR | src|local [host|net] CIDR | dst|remote [host|net] CIDR
//!            | port N | src port N | dst port N | sport N | dport N
//!            | tcp | udp | icmp | sctp | proto N | ip | ip6
//!            | external | internal | CIDR | N (=port)

use crate::geo::Geo;
use crate::procs::Procs;
use crate::stats::FlowKey;
use std::net::IpAddr;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Result<Cidr, String> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.parse().map_err(|_| format!("bad address '{a}'"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            None => max,
            Some(p) => {
                if let Ok(n) = p.parse::<u8>() {
                    n
                } else if let Ok(IpAddr::V4(m)) = p.parse::<IpAddr>() {
                    u32::from(m).count_ones() as u8
                } else {
                    return Err(format!("bad prefix '{p}'"));
                }
            }
        };
        if prefix > max {
            return Err(format!("prefix /{prefix} too long"));
        }
        Ok(Cidr { addr, prefix })
    }

    pub fn host(ip: IpAddr) -> Cidr {
        Cidr {
            addr: ip,
            prefix: if ip.is_ipv4() { 32 } else { 128 },
        }
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        if self.prefix == 0 {
            return self.addr.is_ipv4() == ip.is_ipv4();
        }
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let m = u32::MAX << (32 - self.prefix as u32);
                (u32::from(n) & m) == (u32::from(*a) & m)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let m = u128::MAX << (128 - self.prefix as u32);
                (u128::from(n) & m) == (u128::from(*a) & m)
            }
            _ => false,
        }
    }
}

/// Private, loopback, link-local, CGNAT, multicast, broadcast… or in one of
/// the user's extra internal networks.
pub fn is_internal(ip: &IpAddr, extra: &[Cidr]) -> bool {
    if extra.iter().any(|c| c.contains(ip)) {
        return true;
    }
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_unspecified()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(a) => {
            let s0 = a.segments()[0];
            a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s0 & 0xfe00) == 0xfc00
                || (s0 & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    All,
    /// At least one side is external (hides traffic between your own networks).
    External,
    /// Both sides internal.
    Internal,
}

impl Scope {
    pub fn next(self) -> Scope {
        match self {
            Scope::All => Scope::External,
            Scope::External => Scope::Internal,
            Scope::Internal => Scope::All,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Scope::All => "all hosts",
            Scope::External => "external only",
            Scope::Internal => "internal only",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Host(Cidr),
    Local(Cidr),
    Remote(Cidr),
    Port(u16),
    LPort(u16),
    RPort(u16),
    Proto(u8),
    Icmp,
    V4,
    V6,
    External,
    Internal,
    /// GeoIP terms; `local` picks the local side, default is the remote peer.
    Asn { local: bool, asn: u32 },
    Cc { local: bool, cc: String },
    /// Case-insensitive substring of the AS organisation.
    Org { local: bool, org: String },
    /// Case-insensitive substring of the owning process's name.
    Proc(String),
    Pid(u32),
}

/// What filter terms may look things up in.
#[derive(Default, Clone, Copy)]
pub struct Ctx<'a> {
    pub geo: Option<&'a Geo>,
    pub procs: Option<&'a Procs>,
}

impl Expr {
    pub fn eval(&self, k: &FlowKey, nets: &[Cidr], cx: &Ctx) -> bool {
        let side = |local: bool| if local { k.local } else { k.remote };
        let info = |local: bool| cx.geo.and_then(|g| g.lookup(&side(local)));
        let proc = || cx.procs.and_then(|p| p.lookup(k));
        match self {
            Expr::And(a, b) => a.eval(k, nets, cx) && b.eval(k, nets, cx),
            Expr::Or(a, b) => a.eval(k, nets, cx) || b.eval(k, nets, cx),
            Expr::Not(a) => !a.eval(k, nets, cx),
            Expr::Proc(name) => proc().is_some_and(|p| p.name.to_lowercase().contains(name.as_str())),
            Expr::Pid(pid) => proc().is_some_and(|p| p.pid == *pid),
            Expr::Asn { local, asn } => info(*local).is_some_and(|g| g.asn == Some(*asn)),
            Expr::Cc { local, cc } => {
                info(*local).is_some_and(|g| g.cc.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(cc)))
            }
            Expr::Org { local, org } => info(*local)
                .is_some_and(|g| g.as_org.as_deref().is_some_and(|o| o.to_lowercase().contains(org.as_str()))),
            Expr::Host(c) => c.contains(&k.local) || c.contains(&k.remote),
            Expr::Local(c) => c.contains(&k.local),
            Expr::Remote(c) => c.contains(&k.remote),
            Expr::Port(p) => k.lport == *p || k.rport == *p,
            Expr::LPort(p) => k.lport == *p,
            Expr::RPort(p) => k.rport == *p,
            Expr::Proto(p) => k.proto == *p,
            Expr::Icmp => k.proto == 1 || k.proto == 58,
            Expr::V4 => k.remote.is_ipv4(),
            Expr::V6 => k.remote.is_ipv6(),
            Expr::External => !(is_internal(&k.local, nets) && is_internal(&k.remote, nets)),
            Expr::Internal => is_internal(&k.local, nets) && is_internal(&k.remote, nets),
        }
    }

    /// True if evaluating needs GeoIP data.
    pub fn uses_geo(&self) -> bool {
        match self {
            Expr::And(a, b) | Expr::Or(a, b) => a.uses_geo() || b.uses_geo(),
            Expr::Not(a) => a.uses_geo(),
            Expr::Asn { .. } | Expr::Cc { .. } | Expr::Org { .. } => true,
            _ => false,
        }
    }

    pub fn parse(text: &str) -> Result<Option<Expr>, String> {
        let toks = tokenize(text)?;
        if toks.is_empty() {
            return Ok(None);
        }
        let mut p = Parser { t: &toks, i: 0 };
        let e = p.or()?;
        if let Some(t) = p.peek() {
            return Err(format!("unexpected '{t}'"));
        }
        Ok(Some(e))
    }
}

/// Words, parentheses, `! && ||`, and "quoted strings" (one token each).
fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let c: Vec<char> = text.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        let pair = |i: usize, a: char| i + 1 < c.len() && c[i] == a && c[i + 1] == a;
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '(' || ch == ')' || ch == '!' {
            toks.push(ch.to_string());
            i += 1;
        } else if pair(i, '&') || pair(i, '|') {
            toks.push(format!("{ch}{ch}"));
            i += 2;
        } else if ch == '"' || ch == '\'' {
            let end = c[i + 1..].iter().position(|&x| x == ch).ok_or("unterminated quote")?;
            toks.push(c[i + 1..i + 1 + end].iter().collect::<String>().to_lowercase());
            i += end + 2;
        } else {
            let start = i;
            while i < c.len()
                && !c[i].is_whitespace()
                && !matches!(c[i], '(' | ')' | '"' | '\'')
                && !pair(i, '&')
                && !pair(i, '|')
            {
                i += 1;
            }
            toks.push(c[start..i].iter().collect::<String>().to_lowercase());
        }
    }
    Ok(toks)
}

struct Parser<'a> {
    t: &'a [String],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.t.get(self.i).map(|s| s.as_str())
    }
    fn next(&mut self) -> Option<&'a str> {
        let r = self.peek();
        self.i += 1;
        r
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while matches!(self.peek(), Some("or") | Some("||")) {
            self.i += 1;
            let r = self.and()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.unary()?;
        loop {
            match self.peek() {
                Some("and") | Some("&&") => self.i += 1,
                Some("or") | Some("||") | Some(")") | None => break,
                _ => {}
            }
            let r = self.unary()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some("not") | Some("!") => Ok(Expr::Not(Box::new(self.unary()?))),
            Some("(") => {
                let e = self.or()?;
                match self.next() {
                    Some(")") => Ok(e),
                    _ => Err("missing ')'".into()),
                }
            }
            Some(w) => self.prim(w),
            None => Err("unexpected end of filter".into()),
        }
    }
    fn cidr(&mut self) -> Result<Cidr, String> {
        let w = self.next().ok_or("expected address")?;
        Cidr::parse(w)
    }
    fn geo_term(&mut self, w: &str, local: bool) -> Result<Expr, String> {
        let v = self.next().ok_or(format!("expected a value after '{w}'"))?;
        Ok(match w {
            "asn" | "as" => {
                let n = v.strip_prefix("as").unwrap_or(v);
                Expr::Asn { local, asn: n.parse().map_err(|_| format!("bad AS number '{v}'"))? }
            }
            "cc" | "country" => Expr::Cc { local, cc: v.to_string() },
            _ => Expr::Org { local, org: v.to_string() },
        })
    }
    fn port(&mut self) -> Result<u16, String> {
        let w = self.next().ok_or("expected port")?;
        w.parse().map_err(|_| format!("bad port '{w}'"))
    }
    fn prim(&mut self, w: &'a str) -> Result<Expr, String> {
        Ok(match w {
            "host" | "net" => Expr::Host(self.cidr()?),
            "src" | "local" | "dst" | "remote" => {
                let local = matches!(w, "src" | "local");
                match self.peek() {
                    Some("port") => {
                        self.i += 1;
                        let p = self.port()?;
                        if local { Expr::LPort(p) } else { Expr::RPort(p) }
                    }
                    Some(g @ ("asn" | "as" | "cc" | "country" | "org")) => {
                        self.i += 1;
                        self.geo_term(g, local)?
                    }
                    Some("host") | Some("net") => {
                        self.i += 1;
                        let c = self.cidr()?;
                        if local { Expr::Local(c) } else { Expr::Remote(c) }
                    }
                    _ => {
                        let c = self.cidr()?;
                        if local { Expr::Local(c) } else { Expr::Remote(c) }
                    }
                }
            }
            "asn" | "as" | "cc" | "country" | "org" => self.geo_term(w, false)?,
            "proc" | "process" => {
                let v = self.next().ok_or("expected a process name after 'proc'")?;
                Expr::Proc(v.to_lowercase())
            }
            "pid" => {
                let v = self.next().ok_or("expected a pid")?;
                Expr::Pid(v.parse().map_err(|_| format!("bad pid '{v}'"))?)
            }
            "port" => Expr::Port(self.port()?),
            "sport" | "lport" => Expr::LPort(self.port()?),
            "dport" | "rport" => Expr::RPort(self.port()?),
            "tcp" => Expr::Proto(6),
            "udp" => Expr::Proto(17),
            "sctp" => Expr::Proto(132),
            "gre" => Expr::Proto(47),
            "esp" => Expr::Proto(50),
            "icmp" | "icmp6" => Expr::Icmp,
            "proto" => {
                let w = self.next().ok_or("expected protocol number")?;
                Expr::Proto(w.parse().map_err(|_| format!("bad protocol '{w}'"))?)
            }
            "ip" | "ip4" | "ipv4" => Expr::V4,
            "ip6" | "ipv6" => Expr::V6,
            "external" | "ext" | "public" => Expr::External,
            "internal" | "int" | "private" | "lan" => Expr::Internal,
            _ => {
                if let Ok(p) = w.parse::<u16>() {
                    Expr::Port(p)
                } else if let Ok(c) = Cidr::parse(w) {
                    Expr::Host(c)
                } else {
                    return Err(format!("unknown term '{w}'"));
                }
            }
        })
    }
}

#[derive(Clone, Default)]
pub struct Filter {
    pub expr: Option<Expr>,
    pub text: String,
    pub scope: Scope,
    pub internal_nets: Vec<Cidr>,
    /// For asn / cc / org terms.
    pub geo: Option<Arc<Geo>>,
    /// For proc / pid terms.
    pub procs: Option<Arc<Procs>>,
}

impl Filter {
    pub fn matches(&self, k: &FlowKey) -> bool {
        let nets = &self.internal_nets;
        let ok = match self.scope {
            Scope::All => true,
            Scope::External => !(is_internal(&k.local, nets) && is_internal(&k.remote, nets)),
            Scope::Internal => is_internal(&k.local, nets) && is_internal(&k.remote, nets),
        };
        ok && self.expr.as_ref().map_or(true, |e| e.eval(k, nets, &Ctx { geo: self.geo.as_deref(), procs: self.procs.as_deref() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(l: &str, lp: u16, r: &str, rp: u16, proto: u8) -> FlowKey {
        FlowKey { proto, local: l.parse().unwrap(), lport: lp, remote: r.parse().unwrap(), rport: rp }
    }

    #[test]
    fn cidr() {
        let c = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(c.contains(&"10.2.3.4".parse().unwrap()));
        assert!(!c.contains(&"11.2.3.4".parse().unwrap()));
        let c = Cidr::parse("192.168.1.0/255.255.255.0").unwrap();
        assert_eq!(c.prefix, 24);
        assert!(Cidr::parse("fe80::/10").unwrap().contains(&"fe80::1".parse().unwrap()));
    }

    #[test]
    fn exprs() {
        let k = key("198.51.100.7", 22, "8.8.8.8", 50000, 6);
        let t = |s: &str| Expr::parse(s).unwrap().unwrap().eval(&k, &[], &Ctx::default());
        assert!(t("tcp port 22"));
        assert!(t("tcp and not udp"));
        assert!(!t("udp or port 80"));
        assert!(t("src port 22 and dst net 8.8.0.0/16"));
        assert!(t("!(port 80 || port 443)"));
        assert!(t("external"));
        assert!(Expr::parse("host").is_err());
        // No GeoIP loaded: geo terms never match.
        assert!(!t("asn 15169"));
        assert!(t("not cc us"));
    }

    #[test]
    fn tokens() {
        assert_eq!(tokenize("!(tcp&&port 22)||udp").unwrap(), ["!", "(", "tcp", "&&", "port", "22", ")", "||", "udp"]);
        assert_eq!(tokenize(r#"org "University of Warsaw" and cc PL"#).unwrap(), ["org", "university of warsaw", "and", "cc", "pl"]);
        assert!(tokenize("org \"oops").is_err());
        let e = Expr::parse("dst asn AS15169 or src org 'acme' or cc pl").unwrap().unwrap();
        assert!(e.uses_geo());
        assert!(matches!(Expr::parse("as15169"), Err(_)));
        assert!(matches!(Expr::parse("asn as15169").unwrap(), Some(Expr::Asn { local: false, asn: 15169 })));
        assert!(matches!(Expr::parse("proc Firefox").unwrap(), Some(Expr::Proc(n)) if n == "firefox"));
        assert!(matches!(Expr::parse("pid 42").unwrap(), Some(Expr::Pid(42))));
        assert!(Expr::parse("pid x").is_err());
        assert!(matches!(Expr::parse("src cc pl").unwrap(), Some(Expr::Cc { local: true, .. })));
    }

    #[test]
    fn external_scope() {
        let nets = vec![Cidr::parse("192.168.10.0/24").unwrap(), Cidr::parse("198.51.100.7").unwrap()];
        let f = Filter { scope: Scope::External, internal_nets: nets, ..Default::default() };
        assert!(!f.matches(&key("198.51.100.7", 22, "192.168.10.5", 1, 6)));
        assert!(!f.matches(&key("198.51.100.7", 22, "224.0.0.1", 1, 17)));
        assert!(f.matches(&key("198.51.100.7", 22, "1.1.1.1", 1, 6)));
    }
}
