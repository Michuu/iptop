mod app;
mod capture;
mod config;
mod filter;
mod geo;
mod probe;
mod resolver;
mod stats;
mod ui;
mod util;

use clap::Parser;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;

/// iptop: a modern, btop-flavoured take on iftop.
///
/// Most iftop keys work (press h inside). Needs root or
/// `setcap cap_net_raw,cap_net_admin=eip` on the binary.
#[derive(Parser, Debug)]
#[command(name = "iptop", version, about, long_about = None, args_override_self = true)]
pub struct Args {
    /// Interface to listen on ("any" for all). Default: the default-route interface
    #[arg(short = 'i', long)]
    pub interface: Option<String>,
    /// Don't do hostname lookups
    #[arg(short = 'n', long)]
    pub no_dns: bool,
    /// Don't convert port numbers to services
    #[arg(short = 'N', long)]
    pub no_port_names: bool,
    /// Run in promiscuous mode
    #[arg(short = 'p', long)]
    pub promiscuous: bool,
    /// Show ports as well as hosts
    #[arg(short = 'P', long)]
    pub ports: bool,
    /// Don't display bar graphs of traffic
    #[arg(short = 'b', long)]
    pub no_bars: bool,
    /// Display bandwidth in bytes
    #[arg(short = 'B', long)]
    pub bytes: bool,
    /// Local network(s) for direction of promiscuous traffic (repeatable), e.g. 192.168.1.0/24
    #[arg(short = 'F', long = "net")]
    pub nets: Vec<String>,
    /// Filter expression, e.g. "tcp and not port 22", "net 10.0.0.0/8 or udp"
    #[arg(short = 'f', long)]
    pub filter: Option<String>,
    /// Show only flows with an external peer
    #[arg(short = 'x', long)]
    pub external: bool,
    /// Extra networks/hosts to treat as internal (repeatable), e.g. -I 192.168.10.0/24 -I 198.51.100.7
    #[arg(short = 'I', long = "internal-net")]
    pub internal_nets: Vec<String>,
    /// Sort by: 2s, 10s, 40s, source, destination
    #[arg(short = 'o', long, default_value = "2s")]
    pub order: String,
    /// Line display: one, two, sent, recv
    #[arg(long, default_value = "one")]
    pub lines: String,
    /// Update interval in milliseconds (keys + / -), like btop
    #[arg(short = 'u', long, default_value_t = 1000)]
    pub interval: u64,
    /// The three averaging windows in seconds (key w cycles presets)
    #[arg(short = 'w', long, value_name = "A,B,C")]
    pub windows: Option<String>,
    /// GeoIP .mmdb file or directory (default: ~/geoip-*/mmdb, ~/geoip-*, /usr/share/GeoIP, ...)
    #[arg(long)]
    pub geoip: Option<String>,
    /// Geo column: short, org, asn, place, full, off (key G cycles)
    #[arg(long, value_name = "FMT", default_value = "short")]
    pub geo_format: String,
    /// Config file with default options (default: ~/.config/iptop/config)
    #[arg(long, value_name = "PATH")]
    pub config: Option<String>,
    /// Ignore the config file
    #[arg(long)]
    pub no_config: bool,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    // A capability wrapper (e.g. NixOS security.wrappers) hands us
    // cap_net_raw/cap_net_admin as ambient capabilities, which every child
    // would inherit. Drop them so the PATH-resolved ping/traceroute we spawn
    // run unprivileged; our own permitted/effective set is unaffected.
    unsafe {
        libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0);
    }
    let mut app = app::App::new(&args)?;
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = app.run(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

/// Command line with the config file's options inserted in front, so that
/// options given on the command line win.
fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    let (from_cfg, path) = config::load(&argv);
    let mut all = vec![argv[0].clone()];
    all.extend(from_cfg);
    all.extend(argv[1..].iter().cloned());
    match Args::try_parse_from(&all) {
        Ok(a) => a,
        Err(e) => {
            use clap::error::ErrorKind;
            if let Some(p) = path.filter(|_| !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion)) {
                eprintln!("note: options were also read from {}", p.display());
            }
            e.exit()
        }
    }
}
