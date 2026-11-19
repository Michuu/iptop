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

    pub fn is_empty(&self) -> bool {
        self.cc.is_none() && self.city.is_none() && self.asn.is_none() && self.as_org.is_none()
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
        if let Ok(u) = std::env::var("SUDO_USER") {
            homes.push(PathBuf::from(format!("/home/{u}")));
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
            let Ok(rec) = r.lookup::<Rec>(*ip) else { continue };
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
