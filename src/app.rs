//! Application state, aggregation of flows into display rows, key handling.

use crate::capture::Capture;
use crate::filter::{Cidr, Expr, Filter, Scope};
use crate::geo::{Geo, GeoFmt};
use crate::probe::Probe;
use crate::procs::Procs;
use crate::resolver::Resolver;
use crate::stats::{FlowKey, SharedStats, Stats, FLOW_HIST, TOTAL_HIST};
use crate::util::{self, has_ports, Services};
use crate::views::{self, Agg, IfHist, Log, View};
use crate::Args;
use anyhow::Result;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SPARK: usize = 32;
/// Time scales (`+` / `-`): name, update interval (ms) and the three averaging
/// windows (s). The update interval equals the shortest window, so every
/// update shows a fresh, complete average and no traffic is skipped; the graph
/// draws one sample per update, so slower scales also cover a longer time.
pub const SCALES: [(&str, u64, [u64; 3]); 4] = [
    ("fast", 1000, [1, 5, 20]),
    ("normal", 2000, [2, 10, 40]),
    ("slow", 5000, [5, 30, 120]),
    ("slower", 10000, [10, 60, 300]),
];
/// Rows keep their order this long after the last navigation input.
const HOLD: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineMode {
    Two,
    One,
    Sent,
    Recv,
}

impl LineMode {
    pub fn name(self) -> &'static str {
        match self {
            LineMode::Two => "two-line",
            LineMode::One => "one-line",
            LineMode::Sent => "sent only",
            LineMode::Recv => "recv only",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    /// Rate over averaging window 0/1/2 (sent + received, or one direction in sent/recv line mode).
    Col(usize),
    /// Sent (upload) rate over the shortest window.
    Sent,
    /// Received (download) rate over the shortest window.
    Recv,
    /// Cumulative bytes.
    Total,
    Flows,
    /// Local host name.
    Src,
    /// Remote host name.
    Dst,
    /// Remote IP address, numerically.
    Ip,
    Asn,
    Country,
    Process,
}

impl Sort {
    pub const MENU: [Sort; 13] = [
        Sort::Col(0),
        Sort::Col(1),
        Sort::Col(2),
        Sort::Sent,
        Sort::Recv,
        Sort::Total,
        Sort::Flows,
        Sort::Src,
        Sort::Dst,
        Sort::Ip,
        Sort::Asn,
        Sort::Country,
        Sort::Process,
    ];

    pub fn parse(s: &str) -> Option<Sort> {
        Some(match s {
            "1" | "w1" | "2s" => Sort::Col(0),
            "2" | "w2" | "10s" => Sort::Col(1),
            "3" | "w3" | "40s" => Sort::Col(2),
            "sent" | "up" | "tx" => Sort::Sent,
            "recv" | "down" | "rx" => Sort::Recv,
            "total" => Sort::Total,
            "flows" => Sort::Flows,
            "source" | "src" | "local" => Sort::Src,
            "destination" | "dst" | "remote" | "name" => Sort::Dst,
            "ip" => Sort::Ip,
            "asn" => Sort::Asn,
            "country" | "cc" | "geo" => Sort::Country,
            "process" | "proc" => Sort::Process,
            _ => return None,
        })
    }

    /// Biggest first (rates, bytes) rather than A→Z.
    pub fn descending(self) -> bool {
        matches!(self, Sort::Col(_) | Sort::Sent | Sort::Recv | Sort::Total | Sort::Flows)
    }

    pub fn label(self, windows: &[u64; 3]) -> String {
        match self {
            Sort::Col(i) => format!("{}s rate", windows[i]),
            Sort::Sent => format!("↑ sent ({}s)", windows[0]),
            Sort::Recv => format!("↓ received ({}s)", windows[0]),
            Sort::Total => "total bytes".into(),
            Sort::Flows => "flows".into(),
            Sort::Src => "local name".into(),
            Sort::Dst => "remote name".into(),
            Sort::Ip => "remote IP".into(),
            Sort::Asn => "ASN".into(),
            Sort::Country => "country".into(),
            Sort::Process => "process".into(),
        }
    }
}

/// `None` sorts last whatever the direction.
fn none_last<T: Ord>(a: &Option<T>, b: &Option<T>, rev: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Some(x), Some(y)) => {
            let o = x.cmp(y);
            if rev { o.reverse() } else { o }
        }
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => Equal,
    }
}

/// World map (`m`): off, a panel beside the graph, or a big one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapMode {
    Off,
    Side,
    Big,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Search,
    Filter,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    None,
    Help,
    Ifaces,
    Sort,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RowKey {
    pub local: Option<IpAddr>,
    pub lport: Option<u16>,
    pub remote: Option<IpAddr>,
    pub rport: Option<u16>,
    pub proto: Option<u8>,
}

pub struct Row {
    pub key: RowKey,
    pub local: String,
    pub remote: String,
    pub geo: String,
    pub asn: Option<u32>,
    pub cc: Option<String>,
    pub loc: Option<(f32, f32)>,
    /// Busiest owning process ("firefox", or "firefox +2" if several).
    pub proc: String,
    pub sent: [f64; 3],
    pub recv: [f64; 3],
    pub tot_sent: u64,
    pub tot_recv: u64,
    pub spark: [u64; SPARK],
    pub nflows: usize,
}

impl Row {
    fn new(key: RowKey) -> Self {
        Row {
            key,
            local: String::new(),
            remote: String::new(),
            geo: String::new(),
            asn: None,
            cc: None,
            loc: None,
            proc: String::new(),
            sent: [0.0; 3],
            recv: [0.0; 3],
            tot_sent: 0,
            tot_recv: 0,
            spark: [0; SPARK],
            nflows: 0,
        }
    }
}

pub struct IfaceEntry {
    pub name: String,
    pub state: String,
    pub addrs: Vec<IpAddr>,
    pub rx: f64,
    pub tx: f64,
}

/// State of the per-host panel (Enter / click on a row).
pub struct HostView {
    pub ip: IpAddr,
    /// The panel shows the row's local side (aggregated rows may force a side).
    pub local: bool,
    pub probe: Probe,
    /// flows touching this host: (key, sent 2s, recv 2s, total bytes)
    pub flows: Vec<(FlowKey, f64, f64, u64)>,
    pub rx: Vec<f64>,
    pub tx: Vec<f64>,
    pub rate: (f64, f64),
    pub totals: (u64, u64),
}

pub struct App {
    pub stats: SharedStats,
    capture: Option<Capture>,
    pub iface: String,
    pub promisc: bool,
    user_nets: Vec<Cidr>,
    pub resolver: Resolver,
    pub geo: Arc<Geo>,
    pub procs: Arc<Procs>,
    services: Services,

    // iftop-style toggles
    pub dns: bool,
    pub port_names: bool,
    pub show_bars: bool,
    pub bar_interval: usize,
    pub hide_local: bool,
    pub hide_remote: bool,
    pub show_lport: bool,
    pub show_rport: bool,
    pub paused: bool,
    pub frozen: bool,
    pub sort: Sort,
    /// Reverse the sort's natural direction.
    pub sort_rev: bool,
    /// Follow mode: the cursor sticks to this row (Space); otherwise it stays
    /// on its row number and the table shows whoever ranks there.
    pub follow: Option<RowKey>,
    /// `follow` was set by opening the host panel, not by Space.
    follow_auto: bool,
    /// Keep the row order until then (set by scrolling / clicking).
    pub hold_until: Option<Instant>,
    /// An input changed something that needs a recompute (not just navigation).
    dirty: bool,
    /// Index into SCALES, None for a custom -u / -w combination.
    pub scale_idx: Option<usize>,
    /// Seconds per graph sample (one sample per update).
    pub graph_step: u64,
    pub sort_sel: usize,
    pub line_mode: LineMode,
    pub show_totals: bool,
    pub log_scale: bool,
    pub bytes: bool,
    // extras
    pub show_graph: bool,
    pub map: MapMode,
    /// Map in braille dots (finer) rather than half-block pixels.
    pub map_dots: bool,
    /// What the table shows (Tab).
    pub view: View,
    pub aggs: Vec<Agg>,
    agg_order: Vec<String>,
    pub log: Log,
    pub if_hist: IfHist,
    pub if_list: Vec<IfaceEntry>,
    /// Cursor and scroll of the non-host views.
    pub vsel: usize,
    pub voff: usize,
    /// Table title: its row and the clickable view tabs (x0, x1, view).
    pub tab_hits: (u16, Vec<(u16, u16, View)>),
    pub geo_fmt: GeoFmt,
    /// Host panel targets the local side of the selected row instead of the remote one.
    pub host_local: bool,
    /// Averaging windows (seconds) of the three rate columns.
    pub windows: [u64; 3],
    /// How often rows/graphs are recomputed.
    pub interval: Duration,
    peak_slot: u64,
    pub search: String,
    pub input: Option<InputKind>,
    pub input_buf: String,
    pub popup: Popup,
    pub host: Option<HostView>,
    status: Option<(String, Instant)>,

    // computed
    pub rows: Vec<Row>,
    order: Vec<RowKey>,
    pub selected: usize,
    pub offset: usize,
    pub scale: f64,
    pub graph_rx: Vec<f64>,
    pub graph_tx: Vec<f64>,
    pub rates: [(f64, f64); 3],
    pub peak: (f64, f64),
    pub totals: (u64, u64),
    pub pps: f64,
    pub nflows: usize,
    pub capture_error: Option<String>,
    last_pkts: (u64, Instant),
    pub ifaces: Vec<IfaceEntry>,
    pub iface_sel: usize,
    /// First visible row of the interface picker (kept in view by the UI).
    pub iface_off: usize,
    /// screen row y -> interface index, and the popup's area (for mouse)
    pub popup_hits: Vec<(u16, usize)>,
    /// Table header: its row and the clickable column spans (x0, x1, sort).
    pub header_hits: (u16, Vec<(u16, u16, Sort)>),
    pub popup_area: Option<(u16, u16, u16, u16)>,
    ifstat_prev: (HashMap<String, (u64, u64)>, Instant),
    /// screen row y -> row index (for mouse), filled by the UI
    pub row_hits: Vec<(u16, usize)>,
    last_click: Option<(usize, Instant)>,
}

impl App {
    pub fn new(args: &Args) -> Result<App> {
        let mut internal_nets = Vec::new();
        for n in &args.internal_nets {
            internal_nets.push(Cidr::parse(n).map_err(anyhow::Error::msg)?);
        }
        let mut user_nets = Vec::new();
        for n in &args.nets {
            user_nets.push(Cidr::parse(n).map_err(anyhow::Error::msg)?);
        }
        let text = args.filter.clone().unwrap_or_default();
        let expr = Expr::parse(&text).map_err(|e| anyhow::anyhow!("filter: {e}"))?;
        let filter = Filter {
            expr,
            text,
            scope: if args.external { Scope::External } else { Scope::All },
            internal_nets,
            geo: None,
            procs: None,
        };
        let iface = args.interface.clone().unwrap_or_else(util::default_iface);
        let geo_fmt = GeoFmt::parse(&args.geo_format)
            .ok_or_else(|| anyhow::anyhow!("--geo-format: expected short, org, asn, place, full or off"))?;

        let geo_paths = match &args.geoip {
            Some(p) => vec![p.into()],
            None => Geo::default_paths(),
        };
        let geo = Arc::new(Geo::load(&geo_paths));
        let procs = Procs::start();
        let filter = Filter { geo: geo.available().then(|| geo.clone()), procs: Some(procs.clone()), ..filter };
        let stats: SharedStats = Arc::new(Mutex::new(Stats::new(filter)));

        let sort = Sort::parse(&args.order).ok_or_else(|| {
            anyhow::anyhow!("--order: expected 1, 2, 3, sent, recv, total, flows, source, destination, ip, asn or country")
        })?;
        if let Some(t) = &args.theme {
            let i = crate::theme::find(t)
                .ok_or_else(|| anyhow::anyhow!("--theme: expected one of {}", crate::theme::names()))?;
            crate::theme::set(i);
        }
        let map = match args.map.as_str() {
            "side" | "on" | "small" => MapMode::Side,
            "big" => MapMode::Big,
            "off" | "none" => MapMode::Off,
            _ => anyhow::bail!("--map: expected side, big or off"),
        };
        let map = if geo.available() { map } else { MapMode::Off };
        let line_mode = match args.lines.as_str() {
            "two" => LineMode::Two,
            "sent" => LineMode::Sent,
            "recv" => LineMode::Recv,
            _ => LineMode::One,
        };

        let base = match &args.scale {
            Some(name) => SCALES
                .iter()
                .position(|s| s.0 == name.as_str())
                .ok_or_else(|| anyhow::anyhow!("--scale: expected fast, normal, slow or slower"))?,
            None => 0,
        };
        let windows = match &args.windows {
            Some(w) => {
                let v: Vec<u64> = w.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() != 3 || v.iter().any(|&x| x == 0 || x as usize > FLOW_HIST - 4) {
                    anyhow::bail!("--windows needs three values in 1..={}, e.g. 2,10,40", FLOW_HIST - 4);
                }
                [v[0], v[1], v[2]]
            }
            None => SCALES[base].2,
        };
        let interval_ms = args.interval.unwrap_or(SCALES[base].1).clamp(50, 60_000);
        let interval = Duration::from_millis(interval_ms);
        let scale_idx = SCALES.iter().position(|s| s.1 == interval_ms && s.2 == windows);

        let mut app = App {
            stats,
            capture: None,
            iface: iface.clone(),
            promisc: args.promiscuous,
            user_nets,
            resolver: Resolver::new(4),
            geo_fmt: if geo.available() { geo_fmt } else { GeoFmt::Off },
            host_local: false,
            geo,
            procs,
            services: Services::load(),
            dns: !args.no_dns,
            port_names: !args.no_port_names,
            show_bars: !args.no_bars,
            bar_interval: 0,
            hide_local: false,
            hide_remote: false,
            show_lport: args.ports,
            show_rport: args.ports,
            paused: false,
            frozen: false,
            sort,
            sort_rev: args.reverse,
            follow: None,
            follow_auto: false,
            hold_until: None,
            dirty: false,
            scale_idx,
            graph_step: (interval_ms / 1000).max(1),
            sort_sel: 0,
            line_mode,
            show_totals: true,
            log_scale: true,
            bytes: args.bytes,
            show_graph: true,
            // Needs a City database for host positions.
            map,
            map_dots: true,
            view: View::Hosts,
            aggs: Vec::new(),
            agg_order: Vec::new(),
            log: Log::default(),
            if_hist: IfHist::new(),
            if_list: Vec::new(),
            vsel: 0,
            voff: 0,
            tab_hits: (0, Vec::new()),
            windows,
            interval,
            peak_slot: 0,
            search: String::new(),
            input: None,
            input_buf: String::new(),
            popup: Popup::None,
            host: None,
            status: None,
            rows: Vec::new(),
            order: Vec::new(),
            selected: 0,
            offset: 0,
            scale: 125.0,
            graph_rx: Vec::new(),
            graph_tx: Vec::new(),
            rates: [(0.0, 0.0); 3],
            peak: (0.0, 0.0),
            totals: (0, 0),
            pps: 0.0,
            nflows: 0,
            capture_error: None,
            last_pkts: (0, Instant::now()),
            ifaces: Vec::new(),
            iface_sel: 0,
            iface_off: 0,
            popup_hits: Vec::new(),
            header_hits: (0, Vec::new()),
            popup_area: None,
            ifstat_prev: (HashMap::new(), Instant::now()),
            row_hits: Vec::new(),
            last_click: None,
        };
        // Fail early (before the TUI starts) if we cannot capture at all.
        app.capture = Some(Capture::start(&iface, app.promisc, app.local_nets(), app.stats.clone())?);
        if app.geo.available() {
            app.flash(format!("geoip: {}", app.geo.sources().join(", ")));
        }
        Ok(app)
    }

    fn local_nets(&self) -> Vec<Cidr> {
        let mut v = self.user_nets.clone();
        for ips in util::iface_addrs().values() {
            v.extend(ips.iter().map(|ip| Cidr::host(*ip)));
        }
        v
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    pub fn status(&self) -> Option<&str> {
        match &self.status {
            Some((m, t)) if t.elapsed() < Duration::from_secs(4) => Some(m),
            _ => None,
        }
    }

    pub fn filter_desc(&self) -> (Scope, String) {
        let st = self.stats.lock().unwrap();
        (st.filter.scope, st.filter.text.clone())
    }

    pub fn run(&mut self, term: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut next = Instant::now();
        loop {
            if Instant::now() >= next {
                self.tick();
                next = Instant::now() + self.interval;
            }
            term.draw(|f| crate::ui::draw(f, self))?;
            // Wake for the next update, but at least 4x/s for the clock & probes.
            let wait = next.saturating_duration_since(Instant::now()).min(Duration::from_millis(250));
            if event::poll(wait)? {
                let old_interval = self.interval;
                loop {
                    let quit = match event::read()? {
                        Event::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
                        Event::Mouse(m) => {
                            self.on_mouse(m);
                            false
                        }
                        _ => false,
                    };
                    if quit {
                        return Ok(());
                    }
                    if !event::poll(Duration::from_millis(0))? {
                        break;
                    }
                }
                // Apply toggles immediately instead of waiting a full interval.
                // Plain navigation only redraws: the table must not reshuffle
                // under the cursor while scrolling.
                if std::mem::take(&mut self.dirty) {
                    self.tick();
                }
                if self.interval != old_interval {
                    next = Instant::now() + self.interval;
                }
            }
        }
    }

    // ------------------------------------------------------------------ model

    fn sort_val(&self, r: &Row, i: usize) -> f64 {
        match self.line_mode {
            LineMode::Sent => r.sent[i],
            LineMode::Recv => r.recv[i],
            _ => r.sent[i] + r.recv[i],
        }
    }

    fn cmp_rows(&self, a: &Row, b: &Row) -> std::cmp::Ordering {
        let rev = self.sort_rev;
        let o = match self.sort {
            Sort::Col(i) => self.sort_val(b, i).total_cmp(&self.sort_val(a, i)),
            Sort::Sent => b.sent[0].total_cmp(&a.sent[0]),
            Sort::Recv => b.recv[0].total_cmp(&a.recv[0]),
            Sort::Total => (b.tot_sent + b.tot_recv).cmp(&(a.tot_sent + a.tot_recv)),
            Sort::Flows => b.nflows.cmp(&a.nflows),
            Sort::Src => a.local.cmp(&b.local),
            Sort::Dst => a.remote.cmp(&b.remote),
            Sort::Ip => a.key.remote.or(a.key.local).cmp(&b.key.remote.or(b.key.local)),
            // Missing geo data stays at the bottom in either direction.
            Sort::Asn => return none_last(&a.asn, &b.asn, rev),
            Sort::Country => {
                return none_last(&a.cc, &b.cc, rev).then_with(|| none_last(&a.asn, &b.asn, false));
            }
            Sort::Process => {
                let p = |r: &Row| (!r.proc.is_empty()).then(|| r.proc.clone());
                return none_last(&p(a), &p(b), rev);
            }
        };
        if rev { o.reverse() } else { o }
    }

    /// Pinned with Space (not the host panel's implicit follow).
    pub fn following(&self) -> bool {
        self.follow.is_some() && !self.follow_auto
    }

    fn sort_aggs(&mut self) {
        let mut aggs = std::mem::take(&mut self.aggs);
        let val = |x: &Agg, i: usize| match self.line_mode {
            LineMode::Sent => x.sent[i],
            LineMode::Recv => x.recv[i],
            _ => x.sent[i] + x.recv[i],
        };
        aggs.sort_by(|a, b| {
            let o = match self.sort {
                Sort::Col(i) => val(b, i).total_cmp(&val(a, i)),
                Sort::Sent => b.sent[0].total_cmp(&a.sent[0]),
                Sort::Recv => b.recv[0].total_cmp(&a.recv[0]),
                Sort::Total => (b.tot_sent + b.tot_recv).cmp(&(a.tot_sent + a.tot_recv)),
                Sort::Flows => b.nflows.cmp(&a.nflows),
                _ => a.label.cmp(&b.label),
            };
            if self.sort_rev { o.reverse() } else { o }.then_with(|| a.key.cmp(&b.key))
        });
        if self.frozen || self.held() {
            let pos: HashMap<&String, usize> = self.agg_order.iter().enumerate().map(|(i, k)| (k, i)).collect();
            aggs.sort_by_key(|a| pos.get(&a.key).copied().unwrap_or(usize::MAX));
        }
        self.agg_order = aggs.iter().map(|a| a.key.clone()).collect();
        self.aggs = aggs;
    }

    /// Rows in the current non-host view.
    pub fn view_len(&self) -> usize {
        match self.view {
            View::Hosts => self.rows.len(),
            View::Procs | View::Services | View::Countries | View::Networks => self.aggs.len(),
            View::Log => self.log.entries.len(),
            View::Ifaces => self.if_list.len(),
        }
    }

    fn set_view(&mut self, v: View) {
        if v == self.view {
            return;
        }
        self.view = v;
        self.vsel = 0;
        self.voff = 0;
        self.aggs.clear();
        self.agg_order.clear();
        self.hold_until = None;
        self.flash(format!("view: {}   (tab: next)", v.name()));
    }

    /// Enter in a non-host view.
    fn view_enter(&mut self) {
        match self.view {
            View::Hosts => self.open_host(),
            View::Procs | View::Services | View::Countries | View::Networks => {
                let Some(a) = self.aggs.get(self.vsel) else { return };
                match a.filter.clone() {
                    Some(term) => {
                        self.and_filter(&term);
                        self.set_view(View::Hosts);
                    }
                    None => self.flash("no filter for this row"),
                }
            }
            View::Log => {
                if let Some(e) = self.log.entries.get(self.vsel) {
                    let ip = e.ip;
                    self.open_host_ip(ip, false);
                }
            }
            View::Ifaces => {
                if let Some(e) = self.if_list.get(self.vsel) {
                    let n = e.name.clone();
                    self.switch_iface(n);
                }
            }
        }
    }

    pub fn held(&self) -> bool {
        self.hold_until.is_some_and(|t| Instant::now() < t)
    }

    pub fn bar_val(&self, r: &Row) -> f64 {
        let i = self.bar_interval;
        match self.line_mode {
            LineMode::One => r.sent[i] + r.recv[i],
            LineMode::Two => r.sent[i].max(r.recv[i]),
            LineMode::Sent => r.sent[i],
            LineMode::Recv => r.recv[i],
        }
    }

    pub fn host_label(&self, ip: &IpAddr) -> String {
        if self.dns {
            if let Some(n) = self.resolver.lookup(ip) {
                return n;
            }
        }
        ip.to_string()
    }

    fn endpoint(&self, ip: Option<IpAddr>, port: Option<u16>, proto: Option<u8>) -> String {
        let mut s = match ip {
            Some(ip) => self.host_label(&ip),
            None => "*".to_string(),
        };
        if let Some(p) = port {
            let proto = proto.unwrap_or(6);
            let pn = if self.port_names {
                self.services.name(p, proto).map(String::from).unwrap_or_else(|| p.to_string())
            } else {
                p.to_string()
            };
            if s.contains(':') && !s.contains('.') {
                s = format!("[{s}]");
            }
            s = format!("{s}:{pn}");
        } else if let Some(pr) = proto {
            if !has_ports(pr) {
                s = format!("{s} ({})", util::proto_name(pr));
            }
        }
        s
    }

    pub fn tick(&mut self) {
        let stats = self.stats.clone();
        let mut st = stats.lock().unwrap();
        st.expire(self.windows[2] + 2);
        let now = st.slot();
        self.capture_error = st.error.clone();
        let el = self.last_pkts.1.elapsed();
        if el >= Duration::from_secs(1) {
            self.pps = st.pkts.saturating_sub(self.last_pkts.0) as f64 / el.as_secs_f64();
            self.last_pkts = (st.pkts, Instant::now());
        }

        if self.popup == Popup::Ifaces {
            self.refresh_ifaces();
        }
        if self.paused {
            return;
        }

        // ----- totals / graph
        // Peak over every complete slot since the last update (updates may be slower than 1/s).
        let last = now.saturating_sub(1);
        let from = self.peak_slot.max(last.saturating_sub(TOTAL_HIST as u64 - 1));
        for slot in from..=last {
            let (ls, lr) = st.filtered.get(slot);
            self.peak.0 = self.peak.0.max(ls as f64);
            self.peak.1 = self.peak.1.max(lr as f64);
        }
        self.peak_slot = last;
        for (i, n) in self.windows.iter().enumerate() {
            self.rates[i] = st.filtered.avg(now, *n);
        }
        self.totals = (st.filtered.total_sent, st.filtered.total_recv);
        // One graph sample per update: the average of its `step` seconds.
        let step = self.graph_step as usize;
        let n = (TOTAL_HIST / step).min(900);
        let (tx, rx) = st.filtered.series(now, n * step);
        let per = |v: Vec<f64>| v.chunks(step).map(|c| c.iter().sum::<f64>() / step as f64).collect();
        self.graph_tx = per(tx);
        self.graph_rx = per(rx);

        // ----- rows
        let show_ports = self.show_lport || self.show_rport;
        let mut map: HashMap<RowKey, Row> = HashMap::new();
        let mut row_procs: HashMap<RowKey, HashMap<String, f64>> = HashMap::new();
        let mut nflows = 0;
        for (k, f) in st.flows.iter() {
            if !st.filter.matches(k) {
                continue;
            }
            nflows += 1;
            let ported = has_ports(k.proto);
            let rk = RowKey {
                local: (!self.hide_local).then_some(k.local),
                lport: (self.show_lport && ported).then_some(k.lport),
                remote: (!self.hide_remote).then_some(k.remote),
                rport: (self.show_rport && ported).then_some(k.rport),
                proto: show_ports.then_some(k.proto),
            };
            if let Some(p) = self.procs.lookup(k) {
                let (s, r) = f.hist.avg(now, self.windows[2]);
                *row_procs.entry(rk.clone()).or_default().entry(p.name.clone()).or_default() += s + r + 1e-9;
            }
            let row = map.entry(rk.clone()).or_insert_with(|| Row::new(rk));
            for (i, n) in self.windows.iter().enumerate() {
                let (s, r) = f.hist.avg(now, *n);
                row.sent[i] += s;
                row.recv[i] += r;
            }
            row.tot_sent += f.hist.total_sent;
            row.tot_recv += f.hist.total_recv;
            for j in 0..SPARK {
                let back = (j + 1) as u64;
                if back <= now {
                    let (s, r) = f.hist.get(now - back);
                    row.spark[SPARK - 1 - j] += s + r;
                }
            }
            row.nflows += 1;
        }
        self.nflows = nflows;
        for (rk, procs) in row_procs {
            if let Some(row) = map.get_mut(&rk) {
                let top = procs.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(n, _)| n.clone()).unwrap_or_default();
                row.proc = if procs.len() > 1 { format!("{top} +{}", procs.len() - 1) } else { top };
            }
        }
        self.procs.prune(|k| st.flows.contains_key(k));

        // ----- host panel data
        if let Some(h) = &mut self.host {
            let ip = h.ip;
            let mut flows = Vec::new();
            const HOST_HIST: usize = 240;
            let mut rx = vec![0.0; HOST_HIST];
            let mut tx = vec![0.0; HOST_HIST];
            let mut rate = (0.0, 0.0);
            let mut totals = (0, 0);
            for (k, f) in st.flows.iter() {
                if k.remote != ip && k.local != ip {
                    continue;
                }
                let (s, r) = f.hist.avg(now, 2);
                rate.0 += s;
                rate.1 += r;
                totals.0 += f.hist.total_sent;
                totals.1 += f.hist.total_recv;
                flows.push((*k, s, r, f.hist.total_sent + f.hist.total_recv));
                let (fs, fr) = f.hist.series(now, HOST_HIST);
                for i in 0..HOST_HIST {
                    tx[i] += fs[i];
                    rx[i] += fr[i];
                }
            }
            flows.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2)).then(b.3.cmp(&a.3)));
            h.flows = flows;
            h.rx = rx;
            h.tx = tx;
            h.rate = rate;
            h.totals = totals;
        }
        let before = self.log.entries.len();
        self.log.update(&st, &self.geo, &self.services, &self.procs);
        // New log entries go on top; keep a scrolled-down cursor on its entry.
        let added = self.log.entries.len().saturating_sub(before);
        if self.view == View::Log && self.vsel > 0 && added > 0 {
            self.vsel += added;
            self.voff += added;
        }
        if self.view.is_agg() {
            self.aggs = views::build(self.view, &st, now, &self.windows, &self.geo, &self.services, &self.procs);
        }
        drop(st);
        self.sort_aggs();
        self.if_hist.sample();
        if self.view == View::Ifaces {
            let addrs = util::iface_addrs();
            self.if_list = util::list_ifaces()
                .into_iter()
                .map(|name| {
                    let (rx, tx) = self.if_hist.hist.get(&name).and_then(|h| h.back().copied()).unwrap_or_default();
                    IfaceEntry { state: util::operstate(&name), addrs: addrs.get(&name).cloned().unwrap_or_default(), name, rx, tx }
                })
                .collect();
        }
        let n = self.view_len();
        self.vsel = self.vsel.min(n.saturating_sub(1));

        let mut rows: Vec<Row> = map.into_values().collect();
        for r in rows.iter_mut() {
            r.local = self.endpoint(r.key.local, r.key.lport, r.key.proto);
            r.remote = self.endpoint(r.key.remote, r.key.rport, r.key.proto);
            // Geo of the peer (or of the local side if peers are aggregated);
            // looked up even when the column is off, for sorting by ASN / country.
            if let Some(g) = r.key.remote.or(r.key.local).and_then(|ip| self.geo.lookup(&ip)) {
                r.geo = self.geo_fmt.render(&g);
                r.asn = g.asn;
                r.cc = g.cc;
                r.loc = g.loc;
            }
        }
        if !self.search.is_empty() {
            let q = self.search.to_lowercase();
            rows.retain(|r| {
                r.local.to_lowercase().contains(&q)
                    || r.remote.to_lowercase().contains(&q)
                    || r.geo.to_lowercase().contains(&q)
                    || r.proc.to_lowercase().contains(&q)
            });
        }
        rows.sort_by(|a, b| {
            self.cmp_rows(a, b)
                .then_with(|| a.remote.cmp(&b.remote))
                .then_with(|| a.local.cmp(&b.local))
        });
        // Frozen (o), or held while the user scrolls / clicks: keep the
        // previous order; new rows go to the bottom.
        if self.frozen || self.held() {
            let pos: HashMap<&RowKey, usize> = self.order.iter().enumerate().map(|(i, k)| (k, i)).collect();
            rows.sort_by_key(|r| pos.get(&r.key).copied().unwrap_or(usize::MAX));
        }
        self.order = rows.iter().map(|r| r.key.clone()).collect();

        let cur_max = rows.iter().map(|r| self.bar_val(r)).fold(0.0, f64::max);
        self.scale = (self.scale * 0.98).max(cur_max).max(125.0);

        // Follow mode: the cursor goes where the row went. Otherwise it stays
        // on its row number.
        if let Some(f) = &self.follow {
            match rows.iter().position(|r| &r.key == f) {
                Some(i) => self.selected = i,
                None if !rows.is_empty() => {
                    if !self.follow_auto {
                        self.flash("followed host is gone; cursor stays in place");
                    }
                    self.follow = None;
                    self.follow_auto = false;
                }
                None => {}
            }
        }
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        self.rows = rows;
    }

    fn refresh_ifaces(&mut self) {
        let el = self.ifstat_prev.1.elapsed().as_secs_f64();
        if el < 1.0 && !self.ifaces.is_empty() {
            return;
        }
        let now = util::read_netdev();
        let addrs = util::iface_addrs();
        let mut list = vec![IfaceEntry {
            name: "any".into(),
            state: "all interfaces".into(),
            addrs: Vec::new(),
            rx: 0.0,
            tx: 0.0,
        }];
        for name in util::list_ifaces() {
            let (rx, tx) = match (now.get(&name), self.ifstat_prev.0.get(&name)) {
                (Some(a), Some(b)) if el > 0.0 => {
                    (a.0.saturating_sub(b.0) as f64 / el, a.1.saturating_sub(b.1) as f64 / el)
                }
                _ => (0.0, 0.0),
            };
            list.push(IfaceEntry {
                state: util::operstate(&name),
                addrs: addrs.get(&name).cloned().unwrap_or_default(),
                name,
                rx,
                tx,
            });
        }
        list[0].rx = list[1..].iter().filter(|e| e.name != "lo").map(|e| e.rx).sum();
        list[0].tx = list[1..].iter().filter(|e| e.name != "lo").map(|e| e.tx).sum();
        self.ifaces = list;
        self.ifstat_prev = (now, Instant::now());
        self.iface_sel = self.iface_sel.min(self.ifaces.len().saturating_sub(1));
    }

    fn switch_iface(&mut self, name: String) {
        self.capture = None; // joins the old capture thread
        self.stats.lock().unwrap().reset();
        self.reset_view();
        match Capture::start(&name, self.promisc, self.local_nets(), self.stats.clone()) {
            Ok(c) => {
                self.capture = Some(c);
                self.flash(format!("listening on {name}"));
            }
            Err(e) => self.flash(format!("{name}: {e}")),
        }
        self.iface = name;
    }

    fn cycle_iface(&mut self, dir: i32) {
        let mut names = vec!["any".to_string()];
        names.extend(util::list_ifaces());
        let i = names.iter().position(|n| n == &self.iface).unwrap_or(0) as i32;
        let n = names.len() as i32;
        let next = names[((i + dir).rem_euclid(n)) as usize].clone();
        self.switch_iface(next);
    }

    fn reset_view(&mut self) {
        self.peak = (0.0, 0.0);
        self.log.clear();
        self.aggs.clear();
        self.rows.clear();
        self.order.clear();
        self.scale = 125.0;
    }

    fn update_filter(&mut self, f: impl FnOnce(&mut Filter)) {
        let mut st = self.stats.lock().unwrap();
        let mut filt = st.filter.clone();
        f(&mut filt);
        st.set_filter(filt);
        drop(st);
        self.peak = (0.0, 0.0);
    }

    fn open_host(&mut self) {
        let Some(r) = self.rows.get(self.selected) else { return };
        // Aggregated rows (s / d) have only one side; fall back to it.
        let (ip, local) = match (self.host_local, r.key.local, r.key.remote) {
            (true, Some(l), _) | (false, Some(l), None) => (l, true),
            (_, _, Some(rm)) => (rm, false),
            (_, None, None) => return,
        };
        // While the panel is open the cursor follows its row, so j/k move
        // from where that host is now.
        if self.follow.is_none() || self.follow_auto {
            self.follow = Some(r.key.clone());
            self.follow_auto = true;
        }
        self.open_host_ip(ip, local);
    }

    fn open_host_ip(&mut self, ip: IpAddr, local: bool) {
        // Same target (e.g. j/k between rows sharing the local host): keep the running probes.
        if let Some(h) = self.host.as_mut().filter(|h| h.ip == ip) {
            h.local = local;
            return;
        }
        self.host = Some(HostView {
            ip,
            local,
            probe: Probe::start(ip),
            flows: Vec::new(),
            rx: Vec::new(),
            tx: Vec::new(),
            rate: (0.0, 0.0),
            totals: (0, 0),
        });
    }

    /// j / k in the host panel: the next host of the hosts table or the log.
    fn host_step(&mut self, d: i64) {
        if self.view == View::Log {
            self.nav(d);
            self.dirty = true;
            self.view_enter();
        } else {
            self.move_sel(d);
            self.open_host();
        }
    }

    /// Close the host panel; the next one opens on the remote side again.
    fn close_host(&mut self) {
        self.host = None;
        self.host_local = false;
        if self.follow_auto {
            self.follow = None;
            self.follow_auto = false;
        }
    }

    /// Host panel: switch between the selected row's remote and local host.
    fn toggle_host_side(&mut self) {
        let Some(r) = self.rows.get(self.selected) else { return };
        // Toggle relative to what the panel actually shows.
        let want_local = !self.host.as_ref().map_or(self.host_local, |h| h.local);
        let other = if want_local { r.key.local } else { r.key.remote };
        if other.is_none() {
            let side = if want_local { "local" } else { "remote" };
            self.flash(format!("no {side} host on this row (aggregated)"));
            return;
        }
        self.host_local = want_local;
        self.open_host();
        self.flash(if self.host_local { "host panel: local side" } else { "host panel: remote side" });
    }

    /// The host a key acts on: the host panel's, else the selected row's shown peer.
    fn target_ip(&self) -> Option<IpAddr> {
        if let Some(h) = &self.host {
            return Some(h.ip);
        }
        match self.view {
            View::Hosts => {
                let r = self.rows.get(self.selected)?;
                r.key.remote.or(r.key.local)
            }
            View::Log => self.log.entries.get(self.vsel).map(|e| e.ip),
            _ => None,
        }
    }

    /// `e` hides the host, `E` shows only it; ANDed onto the current filter.
    fn quick_filter(&mut self, only: bool) {
        let Some(ip) = self.target_ip() else { return };
        let term = if only { format!("host {ip}") } else { format!("not host {ip}") };
        self.and_filter(&term);
    }

    /// AND `term` onto the current filter.
    fn and_filter(&mut self, term: &str) {
        let term = term.to_string();
        let old = self.stats.lock().unwrap().filter.text.clone();
        let text = if old.is_empty() { term } else { format!("({old}) and {term}") };
        match Expr::parse(&text) {
            Ok(expr) => {
                self.update_filter(|f| {
                    f.expr = expr;
                    f.text = text.clone();
                });
                self.close_host();
                self.flash(format!("filter: {text}   (F clears)"));
            }
            Err(e) => self.flash(format!("filter error: {e}")),
        }
    }

    /// `y` copies the IP, `Y` the hostname (or the IP if unresolved).
    fn copy_host(&mut self, name: bool) {
        let Some(ip) = self.target_ip() else { return };
        let text = if name { self.resolver.lookup(&ip).unwrap_or_else(|| ip.to_string()) } else { ip.to_string() };
        let via = util::copy_to_clipboard(&text);
        self.flash(format!("copied {text} via {via}"));
    }

    fn cycle_geo(&mut self) {
        if self.geo.available() {
            self.geo_fmt = self.geo_fmt.next();
            self.flash(format!("geo: {}", self.geo_fmt.name()));
        } else {
            self.flash("no GeoIP databases found (use --geoip DIR)");
        }
    }

    fn move_sel(&mut self, d: i64) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as i64;
        self.selected = (self.selected as i64 + d).clamp(0, n - 1) as usize;
        if self.follow.is_some() {
            // Following: the pin moves with the cursor.
            self.follow = self.rows.get(self.selected).map(|r| r.key.clone());
        }
    }

    /// Navigation in the table: move without recomputing, and hold the row
    /// order for a moment so rows don't jump while scrolling.
    fn nav(&mut self, d: i64) {
        if self.view == View::Hosts {
            self.move_sel(d);
        } else {
            let n = self.view_len() as i64;
            self.vsel = (self.vsel as i64 + d).clamp(0, (n - 1).max(0)) as usize;
        }
        self.hold();
    }

    fn hold(&mut self) {
        self.hold_until = Some(Instant::now() + HOLD);
        self.dirty = false;
    }

    /// Back to the top: cursor on the first row, live order, no follow.
    fn go_top(&mut self) {
        self.vsel = 0;
        self.voff = 0;
        self.selected = 0;
        self.offset = 0;
        self.hold_until = None;
        self.follow = None;
        self.follow_auto = false;
    }

    /// Space: pin the cursor to the selected host, or unpin it.
    fn toggle_follow(&mut self) {
        if self.view != View::Hosts {
            return;
        }
        if self.follow.is_some() && !self.follow_auto {
            self.follow = None;
            self.flash("cursor stays in place (top mode)");
            return;
        }
        let Some(r) = self.rows.get(self.selected) else { return };
        let label = if r.key.remote.is_some() { r.remote.clone() } else { r.local.clone() };
        self.follow = Some(r.key.clone());
        self.follow_auto = false;
        self.flash(format!("following {label}   (space: stop)"));
    }

    fn set_sort(&mut self, s: Sort) {
        if self.sort == s {
            self.sort_rev = !self.sort_rev;
        } else {
            self.sort = s;
            self.sort_rev = false;
        }
        // A new sort should be seen right away, from the top.
        self.hold_until = None;
        if self.follow.is_none() {
            self.offset = 0;
            self.selected = 0;
        }
        let dir = if self.sort.descending() != self.sort_rev { "▾" } else { "▴" };
        self.flash(format!("sort by {} {dir}", self.sort.label(&self.windows)));
    }

    fn step_scale(&mut self, dir: i64) {
        let cur = self.scale_idx.unwrap_or_else(|| {
            SCALES.iter().position(|s| s.2[0] >= self.windows[0]).unwrap_or(SCALES.len() - 1)
        });
        let i = (cur as i64 + dir).clamp(0, SCALES.len() as i64 - 1) as usize;
        let (name, ms, w) = SCALES[i];
        self.scale_idx = Some(i);
        self.interval = Duration::from_millis(ms);
        self.windows = w;
        self.graph_step = (ms / 1000).max(1);
        self.flash(format!("time scale {name}: update {}, averages {}/{}/{}s", util::fmt_ms(ms), w[0], w[1], w[2]));
    }

    // --------------------------------------------------------------- input

    fn on_mouse(&mut self, m: MouseEvent) {
        self.dirty = true;
        if self.popup != Popup::None && self.host.is_none() {
            self.on_popup_mouse(m);
            return;
        }
        if self.host.is_some() {
            if let MouseEventKind::Down(MouseButton::Right) = m.kind {
                self.close_host();
                self.popup = Popup::None;
            }
            return;
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.nav(3),
            MouseEventKind::ScrollUp => self.nav(-3),
            MouseEventKind::Down(MouseButton::Left) if m.row == self.tab_hits.0 => {
                let hit = self.tab_hits.1.iter().find(|(x0, x1, _)| m.column >= *x0 && m.column < *x1);
                if let Some(&(_, _, v)) = hit {
                    self.set_view(v);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if m.row == self.header_hits.0 => {
                let hit = self.header_hits.1.iter().find(|(x0, x1, _)| m.column >= *x0 && m.column < *x1);
                if let Some(&(_, _, s)) = hit {
                    self.set_sort(s);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if self.view != View::Hosts => {
                if let Some(&(_, idx)) = self.row_hits.iter().find(|(y, _)| *y == m.row) {
                    if self.vsel == idx {
                        self.view_enter();
                    } else {
                        self.vsel = idx;
                        self.hold();
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(&(_, idx)) = self.row_hits.iter().find(|(y, _)| *y == m.row) {
                    let again = matches!(self.last_click, Some((i, t)) if i == idx && t.elapsed() < Duration::from_millis(600));
                    let was_selected = self.selected == idx;
                    self.move_sel(idx as i64 - self.selected as i64);
                    if again || was_selected {
                        self.open_host();
                        self.last_click = None;
                    } else {
                        self.last_click = Some((idx, Instant::now()));
                        self.hold();
                    }
                }
            }
            _ => {}
        }
    }

    fn on_popup_mouse(&mut self, m: MouseEvent) {
        let inside = self
            .popup_area
            .is_some_and(|(x, y, w, h)| m.column >= x && m.column < x + w && m.row >= y && m.row < y + h);
        match m.kind {
            MouseEventKind::Down(MouseButton::Right) => self.popup = Popup::None,
            MouseEventKind::Down(MouseButton::Left) if !inside || self.popup == Popup::Help => {
                self.popup = Popup::None
            }
            MouseEventKind::Down(MouseButton::Left) if self.popup == Popup::Ifaces => {
                if let Some(&(_, idx)) = self.popup_hits.iter().find(|(y, _)| *y == m.row) {
                    if self.iface_sel == idx {
                        self.pick_iface();
                    } else {
                        self.iface_sel = idx;
                    }
                }
            }
            MouseEventKind::ScrollDown if self.popup == Popup::Ifaces => self.move_iface_sel(1),
            MouseEventKind::ScrollUp if self.popup == Popup::Ifaces => self.move_iface_sel(-1),
            MouseEventKind::Down(MouseButton::Left) if self.popup == Popup::Sort => {
                if let Some(&(_, idx)) = self.popup_hits.iter().find(|(y, _)| *y == m.row) {
                    self.sort_sel = idx;
                    self.pick_sort();
                }
            }
            MouseEventKind::ScrollDown if self.popup == Popup::Sort => self.move_sort_sel(1),
            MouseEventKind::ScrollUp if self.popup == Popup::Sort => self.move_sort_sel(-1),
            _ => {}
        }
    }

    fn move_iface_sel(&mut self, d: i64) {
        let n = self.ifaces.len() as i64;
        self.iface_sel = (self.iface_sel as i64 + d).clamp(0, (n - 1).max(0)) as usize;
    }

    fn move_sort_sel(&mut self, d: i64) {
        self.sort_sel = (self.sort_sel as i64 + d).clamp(0, Sort::MENU.len() as i64 - 1) as usize;
    }

    fn pick_sort(&mut self) {
        self.popup = Popup::None;
        self.set_sort(Sort::MENU[self.sort_sel]);
    }

    fn pick_iface(&mut self) {
        if let Some(e) = self.ifaces.get(self.iface_sel) {
            let n = e.name.clone();
            self.popup = Popup::None;
            self.switch_iface(n);
        }
    }

    /// Returns true to quit.
    pub fn on_key(&mut self, k: KeyEvent) -> bool {
        self.dirty = true;
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return true;
        }
        if let Some(kind) = self.input {
            match k.code {
                KeyCode::Esc => {
                    if kind == InputKind::Search {
                        self.search.clear();
                    }
                    self.input = None;
                }
                KeyCode::Enter => match kind {
                    InputKind::Search => self.input = None,
                    InputKind::Filter => match Expr::parse(&self.input_buf) {
                        Ok(expr) => {
                            let text = self.input_buf.trim().to_string();
                            let no_geo = expr.as_ref().is_some_and(|e| e.uses_geo()) && !self.geo.available();
                            self.update_filter(|f| {
                                f.expr = expr;
                                f.text = text;
                            });
                            self.input = None;
                            self.flash(if no_geo { "filter applied, but asn/cc/org need GeoIP databases" } else { "filter applied" });
                        }
                        Err(e) => self.flash(format!("filter error: {e}")),
                    },
                },
                KeyCode::Backspace => {
                    self.input_buf.pop();
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => self.input_buf.clear(),
                KeyCode::Char(c) => self.input_buf.push(c),
                _ => {}
            }
            if kind == InputKind::Search && self.input.is_some() {
                self.search = self.input_buf.clone();
            }
            return false;
        }

        if self.host.is_some() {
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Backspace => self.close_host(),
                KeyCode::Char('r') => {
                    // restart probes
                    if let Some(h) = &mut self.host {
                        h.probe = Probe::start(h.ip);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => self.host_step(1),
                KeyCode::Up | KeyCode::Char('k') => self.host_step(-1),
                KeyCode::Tab | KeyCode::Char('l') => self.toggle_host_side(),
                KeyCode::Char('e') => self.quick_filter(false),
                KeyCode::Char('E') => self.quick_filter(true),
                KeyCode::Char('y') => self.copy_host(false),
                KeyCode::Char('Y') => self.copy_host(true),
                KeyCode::Char('G') => self.cycle_geo(),
                _ => {}
            }
            return false;
        }

        match self.popup {
            Popup::Help => {
                self.popup = Popup::None;
                return false;
            }
            Popup::Ifaces => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('q') => self.popup = Popup::None,
                    KeyCode::Down | KeyCode::Char('j') => self.move_iface_sel(1),
                    KeyCode::Up | KeyCode::Char('k') => self.move_iface_sel(-1),
                    KeyCode::PageDown => self.move_iface_sel(10),
                    KeyCode::PageUp => self.move_iface_sel(-10),
                    KeyCode::Home | KeyCode::Char('g') => self.iface_sel = 0,
                    KeyCode::End | KeyCode::Char('G') => self.move_iface_sel(i64::MAX / 2),
                    KeyCode::Enter => self.pick_iface(),
                    _ => {}
                }
                return false;
            }
            Popup::Sort => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('O') | KeyCode::Char('q') => self.popup = Popup::None,
                    KeyCode::Down | KeyCode::Char('j') => self.move_sort_sel(1),
                    KeyCode::Up | KeyCode::Char('k') => self.move_sort_sel(-1),
                    KeyCode::Home | KeyCode::Char('g') => self.sort_sel = 0,
                    KeyCode::End | KeyCode::Char('G') => self.sort_sel = Sort::MENU.len() - 1,
                    KeyCode::Enter | KeyCode::Char(' ') => self.pick_sort(),
                    KeyCode::Char('R') => {
                        self.popup = Popup::None;
                        self.set_sort(self.sort);
                    }
                    _ => {}
                }
                return false;
            }
            Popup::None => {}
        }

        match k.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc => {
                if !self.search.is_empty() {
                    self.search.clear();
                }
            }
            KeyCode::Char('h') | KeyCode::Char('?') | KeyCode::F(1) => self.popup = Popup::Help,
            KeyCode::Char('n') => {
                self.dns = !self.dns;
                self.flash(if self.dns { "DNS resolution on" } else { "DNS resolution off" });
            }
            KeyCode::Char('N') => {
                self.port_names = !self.port_names;
                self.flash(if self.port_names { "port resolution on" } else { "port resolution off" });
            }
            KeyCode::Char('b') => {
                self.show_bars = !self.show_bars;
                self.flash(if self.show_bars { "bars on" } else { "bars off" });
            }
            KeyCode::Char('B') => {
                self.bar_interval = (self.bar_interval + 1) % 3;
                self.flash(format!("bars show {}s average", self.windows[self.bar_interval]));
            }
            KeyCode::Char('s') => {
                self.hide_local = !self.hide_local;
                self.flash(if self.hide_local { "hide source (local) host" } else { "show source (local) host" });
            }
            KeyCode::Char('d') => {
                self.hide_remote = !self.hide_remote;
                self.flash(if self.hide_remote { "hide dest (remote) host" } else { "show dest (remote) host" });
            }
            KeyCode::Char('S') => {
                self.show_lport = !self.show_lport;
                self.flash(if self.show_lport { "show source ports" } else { "hide source ports" });
            }
            KeyCode::Char('D') => {
                self.show_rport = !self.show_rport;
                self.flash(if self.show_rport { "show dest ports" } else { "hide dest ports" });
            }
            KeyCode::Char('p') => {
                let on = !(self.show_lport || self.show_rport);
                self.show_lport = on;
                self.show_rport = on;
                self.flash(if on { "port display on" } else { "port display off" });
            }
            KeyCode::Char('P') => {
                self.paused = !self.paused;
                self.flash(if self.paused { "display paused" } else { "display unpaused" });
            }
            KeyCode::Char('o') => {
                self.frozen = !self.frozen;
                self.flash(if self.frozen { "order frozen" } else { "order unfrozen" });
            }
            KeyCode::Char('1') => self.set_sort(Sort::Col(0)),
            KeyCode::Char('2') => self.set_sort(Sort::Col(1)),
            KeyCode::Char('3') => self.set_sort(Sort::Col(2)),
            KeyCode::Char('<') => self.set_sort(Sort::Src),
            KeyCode::Char('>') => self.set_sort(Sort::Dst),
            KeyCode::Char('R') => self.set_sort(self.sort),
            KeyCode::Char('O') => {
                self.popup = Popup::Sort;
                self.sort_sel = Sort::MENU.iter().position(|s| *s == self.sort).unwrap_or(0);
            }
            KeyCode::Char(' ') => self.toggle_follow(),
            KeyCode::Tab => self.set_view(self.view.step(1)),
            KeyCode::BackTab => self.set_view(self.view.step(-1)),
            KeyCode::Char('t') => {
                self.line_mode = match self.line_mode {
                    LineMode::Two => LineMode::One,
                    LineMode::One => LineMode::Sent,
                    LineMode::Sent => LineMode::Recv,
                    LineMode::Recv => LineMode::Two,
                };
                self.flash(self.line_mode.name());
            }
            KeyCode::Char('T') => {
                self.show_totals = !self.show_totals;
                self.flash(if self.show_totals { "show cumulative totals" } else { "hide cumulative totals" });
            }
            KeyCode::Char('L') => {
                self.log_scale = !self.log_scale;
                self.flash(if self.log_scale { "log scale bars" } else { "linear scale bars" });
            }
            KeyCode::Char('l') | KeyCode::Char('/') => {
                self.input = Some(InputKind::Search);
                self.input_buf = self.search.clone();
            }
            KeyCode::Char('f') => {
                self.input = Some(InputKind::Filter);
                self.input_buf = self.filter_desc().1;
            }
            KeyCode::Char('F') => {
                self.update_filter(|f| {
                    f.expr = None;
                    f.text.clear();
                });
                self.search.clear();
                self.flash("filters cleared");
            }
            KeyCode::Char('x') => {
                let mut new = Scope::All;
                self.update_filter(|f| {
                    f.scope = f.scope.next();
                    new = f.scope;
                });
                self.flash(format!("scope: {}", new.name()));
            }
            KeyCode::Char('i') => {
                self.popup = Popup::Ifaces;
                self.ifaces.clear();
                self.refresh_ifaces();
                self.iface_sel = self.ifaces.iter().position(|e| e.name == self.iface).unwrap_or(0);
            }
            KeyCode::Char('[') => self.cycle_iface(-1),
            KeyCode::Char(']') => self.cycle_iface(1),
            KeyCode::Char('g') => self.show_graph = !self.show_graph,
            KeyCode::Char('+') | KeyCode::Char('=') => self.step_scale(1),
            KeyCode::Char('-') | KeyCode::Char('_') => self.step_scale(-1),
            KeyCode::Char('G') => self.cycle_geo(),
            KeyCode::Char('m') => {
                self.map = match self.map {
                    MapMode::Off => MapMode::Side,
                    MapMode::Side => MapMode::Big,
                    MapMode::Big => MapMode::Off,
                };
                if self.map != MapMode::Off && !self.geo.available() {
                    self.flash("the map needs a GeoIP City database (--geoip DIR)");
                } else {
                    self.flash(format!("map: {}", format!("{:?}", self.map).to_lowercase()));
                }
            }
            KeyCode::Char('M') => {
                self.map_dots = !self.map_dots;
                self.flash(if self.map_dots { "map: braille dots" } else { "map: blocks" });
            }
            KeyCode::Char('c') => {
                crate::theme::set(crate::theme::index() + 1);
                self.flash(format!("theme: {}", crate::theme::th().name));
            }
            KeyCode::Char('e') => self.quick_filter(false),
            KeyCode::Char('E') => self.quick_filter(true),
            KeyCode::Char('y') => self.copy_host(false),
            KeyCode::Char('Y') => self.copy_host(true),
            KeyCode::Char('u') => {
                self.bytes = !self.bytes;
                self.flash(if self.bytes { "units: bytes" } else { "units: bits" });
            }
            KeyCode::Char('r') => {
                self.stats.lock().unwrap().reset();
                self.reset_view();
                self.flash("statistics reset");
            }
            KeyCode::Down | KeyCode::Char('j') => self.nav(1),
            KeyCode::Up | KeyCode::Char('k') => self.nav(-1),
            KeyCode::PageDown => self.nav(20),
            KeyCode::PageUp => self.nav(-20),
            KeyCode::Home => self.go_top(),
            KeyCode::End => self.nav(self.rows.len() as i64),
            KeyCode::Enter => self.view_enter(),
            _ => {}
        }
        false
    }

    pub fn port_name(&self, p: u16, proto: u8) -> String {
        self.services.name(p, proto).map(String::from).unwrap_or_else(|| p.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts() {
        for s in Sort::MENU {
            let name = format!("{s:?}");
            assert!(!s.label(&[1, 5, 20]).is_empty(), "{name}");
        }
        assert_eq!(Sort::parse("10s"), Some(Sort::Col(1)));
        assert_eq!(Sort::parse("down"), Some(Sort::Recv));
        assert_eq!(Sort::parse("asn"), Some(Sort::Asn));
        assert_eq!(Sort::parse("bogus"), None);
        use std::cmp::Ordering::*;
        assert_eq!(none_last(&Some(1), &Some(2), false), Less);
        assert_eq!(none_last(&Some(1), &Some(2), true), Greater);
        assert_eq!(none_last(&Some(1), &None, true), Less);
        assert_eq!(none_last::<u32>(&None, &Some(1), false), Greater);
        // every preset updates once per shortest window
        for (_, ms, w) in SCALES {
            assert_eq!(ms, w[0] * 1000);
        }
    }
}
