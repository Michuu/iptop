//! GeoIP + ASN lookups from MaxMind-format .mmdb files (GeoLite2 City/ASN,
//! DB-IP lite, or custom-patched databases with the same schema).

use serde::Deserialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Deserialize)]
struct Names {
    en: Option<String>,
}

#[derive(Deserialize)]
struct Named {
    names: Option<Names>,
    iso_code: Option<String>,
}

#[derive(Deserialize)]
struct Rec {
    country: Option<Named>,
    registered_country: Option<Named>,
    city: Option<Named>,
    autonomous_system_number: Option<u32>,
    autonomous_system_organization: Option<String>,
}

#[derive(Clone, Default, Debug)]
pub struct GeoInfo {
    pub cc: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub asn: Option<u32>,
    pub as_org: Option<String>,
    /// Network block of the lookup, e.g. "142.250.0.0/15" (from the ASN db if present).
    pub net: Option<String>,
}

impl GeoInfo {
    /// "PL AS8501 PSNC" style, for table columns.
    pub fn short(&self) -> String {
        let mut parts = Vec::new();
        if let Some(cc) = &self.cc {
            parts.push(cc.clone());
        }
        if let Some(a) = self.asn {
            parts.push(format!("AS{a}"));
        }
        if let Some(o) = &self.as_org {
            parts.push(o.clone());
        }
        parts.join(" ")
    }

    /// "Warsaw, Poland (PL)".
    pub fn place(&self) -> String {
        let mut s = String::new();
        if let Some(c) = &self.city {
            s.push_str(c);
        }
        if let Some(c) = &self.country {
            if !s.is_empty() {
                s.push_str(", ");
            }
            s.push_str(c);
        }
        if let Some(cc) = &self.cc {
            s.push_str(&format!(" ({cc})"));
        }
        s.trim().to_string()
    }

    /// "AS8501 PSNC".
    pub fn as_text(&self) -> String {
        match (self.asn, &self.as_org) {
            (Some(a), Some(o)) => format!("AS{a} {o}"),
            (Some(a), None) => format!("AS{a}"),
            (None, Some(o)) => o.clone(),
            (None, None) => String::new(),
        }
    }

    /// "Warsaw, PL" (falls back to the country name, then the code).
    pub fn city_cc(&self) -> String {
        match (&self.city, &self.cc, &self.country) {
            (Some(c), Some(cc), _) => format!("{c}, {cc}"),
            (Some(c), None, _) => c.clone(),
            (None, _, Some(n)) => n.clone(),
            (None, Some(cc), None) => cc.clone(),
            (None, None, None) => String::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.cc.is_none() && self.city.is_none() && self.asn.is_none() && self.as_org.is_none()
    }
}

/// How the geo column renders a `GeoInfo` (cycled with `G`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeoFmt {
    Off,
    /// "PL AS8501 PSNC"
    Short,
    /// "PL PSNC"
    Org,
    /// "PL AS8501"
    Asn,
    /// "Warsaw, PL"
    Place,
    /// "Warsaw, PL · AS8501 PSNC"
    Full,
}

impl GeoFmt {
    pub const ALL: [GeoFmt; 6] = [GeoFmt::Short, GeoFmt::Org, GeoFmt::Asn, GeoFmt::Place, GeoFmt::Full, GeoFmt::Off];

    pub fn parse(s: &str) -> Option<GeoFmt> {
        Some(match s {
            "off" | "none" => GeoFmt::Off,
            "short" => GeoFmt::Short,
            "org" => GeoFmt::Org,
            "asn" => GeoFmt::Asn,
            "place" | "city" => GeoFmt::Place,
            "full" => GeoFmt::Full,
            _ => return None,
        })
    }

    pub fn next(self) -> GeoFmt {
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn name(self) -> &'static str {
        match self {
            GeoFmt::Off => "off",
            GeoFmt::Short => "country + AS + org",
            GeoFmt::Org => "country + org",
            GeoFmt::Asn => "country + AS number",
            GeoFmt::Place => "city, country",
            GeoFmt::Full => "city, country + AS + org",
        }
    }

    /// Column header.
    pub fn header(self) -> &'static str {
        match self {
            GeoFmt::Off => "",
            GeoFmt::Short => "geo / asn",
            GeoFmt::Org => "geo / org",
            GeoFmt::Asn => "geo / asn",
            GeoFmt::Place => "location",
            GeoFmt::Full => "location / asn",
        }
    }

    pub fn render(self, g: &GeoInfo) -> String {
        let cc = g.cc.clone();
        let join = |v: Vec<Option<String>>| v.into_iter().flatten().collect::<Vec<_>>().join(" ");
        match self {
            GeoFmt::Off => String::new(),
            GeoFmt::Short => g.short(),
            GeoFmt::Org => join(vec![cc, g.as_org.clone().or(g.asn.map(|a| format!("AS{a}")))]),
            GeoFmt::Asn => join(vec![cc, g.asn.map(|a| format!("AS{a}"))]),
            GeoFmt::Place => g.city_cc(),
            GeoFmt::Full => {
                let p = if g.city.is_some() { g.city_cc() } else { g.cc.clone().unwrap_or_default() };
                let a = g.as_text();
                match (p.is_empty(), a.is_empty()) {
                    (false, false) => format!("{p} · {a}"),
                    (true, _) => a,
                    (false, true) => p,
                }
            }
        }
    }
}

pub struct Geo {
    readers: Vec<(String, maxminddb::Reader<Vec<u8>>)>,
    cache: Mutex<HashMap<IpAddr, Option<GeoInfo>>>,
}

impl Geo {
    pub fn load(paths: &[PathBuf]) -> Geo {
        let mut readers = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for p in paths {
            let mut files = Vec::new();
            if p.is_file() {
                files.push(p.clone());
            } else if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    let f = e.path();
                    if f.extension().is_some_and(|x| x == "mmdb") {
                        files.push(f);
                    }
                }
            }
            files.sort();
            for f in files {
                let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
                // Prefer the first database of each name (search order matters).
                if !seen.insert(name.clone()) {
                    continue;
                }
                if let Ok(r) = maxminddb::Reader::open_readfile(&f) {
                    readers.push((f.display().to_string(), r));
                }
            }
        }
        Geo { readers, cache: Mutex::new(HashMap::new()) }
    }

    /// Default search path. `~/geoip-*/mmdb` (patched with local nets) wins
    /// over `~/geoip-*`, then the usual system locations.
    pub fn default_paths() -> Vec<PathBuf> {
        let mut homes = Vec::new();
        // Under sudo, also search the invoking user's real home (it is not
        // always /home/<user>, e.g. qlab -> /home/lab).
        if let Ok(u) = std::env::var("SUDO_USER") {
            if let Some(h) = crate::util::user_home(&u) {
                homes.push(h);
            }
        }
        if let Ok(h) = std::env::var("HOME") {
            homes.push(PathBuf::from(h));
        }
        let mut out = Vec::new();
        for h in homes {
            out.push(h.join(".local/share/iptop"));
            if let Ok(rd) = std::fs::read_dir(&h) {
                let mut dirs: Vec<PathBuf> = rd
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("geoip"))
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                dirs.sort();
                for d in &dirs {
                    out.push(d.join("mmdb"));
                }
                out.extend(dirs);
            }
        }
        for s in ["/usr/share/GeoIP", "/var/lib/GeoIP", "/usr/local/share/GeoIP"] {
            out.push(PathBuf::from(s));
        }
        out
    }

    pub fn available(&self) -> bool {
        !self.readers.is_empty()
    }

    pub fn sources(&self) -> Vec<String> {
        self.readers.iter().map(|(n, _)| n.clone()).collect()
    }

    pub fn lookup(&self, ip: &IpAddr) -> Option<GeoInfo> {
        if self.readers.is_empty() {
            return None;
        }
        if let Some(c) = self.cache.lock().unwrap().get(ip) {
            return c.clone();
        }
        let mut g = GeoInfo::default();
        for (_, r) in &self.readers {
            let Ok((rec, plen)) = r.lookup_prefix::<Rec>(*ip) else { continue };
            // The ASN database's block is the more meaningful one (≈ the routed prefix).
            if g.net.is_none() || (g.asn.is_none() && rec.autonomous_system_number.is_some()) {
                g.net = Some(network(ip, plen));
            }
            let country = rec.country.or(rec.registered_country);
            if let Some(c) = country {
                if g.cc.is_none() {
                    g.cc = c.iso_code;
                }
                if g.country.is_none() {
                    g.country = c.names.and_then(|n| n.en);
                }
            }
            if g.city.is_none() {
                g.city = rec.city.and_then(|c| c.names).and_then(|n| n.en);
            }
            if g.asn.is_none() {
                g.asn = rec.autonomous_system_number;
            }
            if g.as_org.is_none() {
                g.as_org = rec.autonomous_system_organization;
            }
        }
        let res = if g.is_empty() { None } else { Some(g) };
        self.cache.lock().unwrap().insert(*ip, res.clone());
        res
    }
}

/// "a.b.c.d/len" for the block of `len` bits containing `ip`.
fn network(ip: &IpAddr, len: usize) -> String {
    match ip {
        IpAddr::V4(a) => {
            let len = len.min(32);
            let m = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            format!("{}/{len}", std::net::Ipv4Addr::from(u32::from(*a) & m))
        }
        IpAddr::V6(a) => {
            let len = len.min(128);
            let m = if len == 0 { 0 } else { u128::MAX << (128 - len) };
            format!("{}/{len}", std::net::Ipv6Addr::from(u128::from(*a) & m))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks() {
        assert_eq!(network(&"142.250.74.14".parse().unwrap(), 15), "142.250.0.0/15");
        assert_eq!(network(&"8.8.8.8".parse().unwrap(), 32), "8.8.8.8/32");
        assert_eq!(network(&"2001:db8:1234::1".parse().unwrap(), 32), "2001:db8::/32");
    }

    #[test]
    fn formats() {
        let g = GeoInfo {
            cc: Some("PL".into()),
            country: Some("Poland".into()),
            city: Some("Warsaw".into()),
            asn: Some(8501),
            as_org: Some("PSNC".into()),
            net: None,
        };
        assert_eq!(GeoFmt::Short.render(&g), "PL AS8501 PSNC");
        assert_eq!(GeoFmt::Org.render(&g), "PL PSNC");
        assert_eq!(GeoFmt::Asn.render(&g), "PL AS8501");
        assert_eq!(GeoFmt::Place.render(&g), "Warsaw, PL");
        assert_eq!(GeoFmt::Full.render(&g), "Warsaw, PL · AS8501 PSNC");
        let a = GeoInfo { asn: Some(15169), as_org: Some("Google LLC".into()), ..Default::default() };
        assert_eq!(GeoFmt::Full.render(&a), "AS15169 Google LLC");
        assert_eq!(GeoFmt::Place.render(&a), "");
        let mut f = GeoFmt::Short;
        for _ in 0..GeoFmt::ALL.len() {
            f = f.next();
        }
        assert_eq!(f, GeoFmt::Short);
    }
}
