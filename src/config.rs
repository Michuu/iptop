//! ~/.config/iptop/config: default options, one per line.
//!
//! ```text
//! # comments start with #
//! external                      # same as --external
//! internal-net = 192.168.10.0/24  # same as --internal-net=192.168.10.0/24
//! geo-format = full
//! filter = not port 22          # the value is the rest of the line
//! -I 10.0.0.0/8                 # lines starting with - are split like a shell (no quoting)
//! ```

use std::path::PathBuf;

/// Candidate config files, first existing wins.
fn default_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        v.push(PathBuf::from(x).join("iptop/config"));
    }
    // Under sudo, the invoking user's config comes first.
    if let Ok(u) = std::env::var("SUDO_USER") {
        if let Some(h) = crate::util::user_home(&u) {
            v.push(h.join(".config/iptop/config"));
        }
    }
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h).join(".config/iptop/config"));
    }
    v
}

/// Turn config text into command-line arguments.
pub fn to_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        // Strip a comment: '#' at the start or after whitespace.
        let mut cut = line.len();
        let b = line.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c == b'#' && (i == 0 || b[i - 1].is_ascii_whitespace()) {
                cut = i;
                break;
            }
        }
        let line = line[..cut].trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('-') {
            out.extend(line.split_whitespace().map(String::from));
        } else if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
                .unwrap_or(v);
            out.push(format!("--{}={v}", k.trim()));
        } else {
            out.push(format!("--{line}"));
        }
    }
    out
}

/// Options from the config file (unless --no-config), and the file used.
pub fn load(argv: &[String]) -> (Vec<String>, Option<PathBuf>) {
    if argv.iter().any(|a| a == "--no-config") {
        return (Vec::new(), None);
    }
    let explicit = argv.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--config=").map(String::from).or_else(|| (a == "--config").then(|| argv.get(i + 1).cloned()).flatten())
    });
    let path = match explicit {
        Some(p) => Some(PathBuf::from(p)),
        None => default_paths().into_iter().find(|p| p.is_file()),
    };
    let Some(path) = path else { return (Vec::new(), None) };
    match std::fs::read_to_string(&path) {
        Ok(t) => (to_args(&t), Some(path)),
        Err(e) => {
            eprintln!("iptop: cannot read {}: {e}", path.display());
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let t = "# example\nexternal\ninternal-net = 192.168.10.0/24  # office\n\n-I 10.0.0.0/8 -n\nfilter = \"not port 22\"\ngeo-format=full\nfilter2 = a#b\n";
        assert_eq!(
            to_args(t),
            [
                "--external",
                "--internal-net=192.168.10.0/24",
                "-I",
                "10.0.0.0/8",
                "-n",
                "--filter=not port 22",
                "--geo-format=full",
                "--filter2=a#b"
            ]
        );
    }

    #[test]
    fn cli_wins() {
        use clap::Parser;
        let a = crate::Args::try_parse_from([
            "iptop", "--geo-format=full", "-I", "10.0.0.0/8", "-u", "500", // from config
            "-I", "1.2.3.4", "--geo-format", "org", // from the command line
        ])
        .unwrap();
        assert_eq!(a.geo_format, "org");
        assert_eq!(a.interval, Some(500));
        assert_eq!(a.internal_nets, ["10.0.0.0/8", "1.2.3.4"]);
    }
}
