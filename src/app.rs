//! Application state, aggregation of flows into display rows, key handling.

use crate::capture::Capture;
use crate::filter::{Cidr, Expr, Filter, Scope};
use crate::geo::{Geo, GeoFmt};
use crate::probe::Probe;
use crate::resolver::Resolver;
use crate::stats::{FlowKey, SharedStats, Stats, FLOW_HIST, TOTAL_HIST};
use crate::util::{self, has_ports, Services};
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
/// Averaging-window presets (seconds), cycled with `w`. iftop uses 2/10/40.
pub const WINDOW_PRESETS: [[u64; 3]; 4] = [[1, 5, 20], [2, 10, 40], [5, 30, 120], [10, 60, 300]];
/// Refresh-interval steps (ms), changed with `+` / `-` like btop.
pub const REFRESH_STEPS: [u64; 8] = [100, 250, 500, 1000, 2000, 3000, 5000, 10000];

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Col(usize),
    Src,
    Dst,
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
    pub line_mode: LineMode,
    pub show_totals: bool,
    pub log_scale: bool,
    pub bytes: bool,
    // extras
    pub show_graph: bool,
    pub geo_fmt: GeoFmt,
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
    selected_key: Option<RowKey>,
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
    pub iface_hits: Vec<(u16, usize)>,
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
        };
        let stats: SharedStats = Arc::new(Mutex::new(Stats::new(filter)));
        let iface = args.interface.clone().unwrap_or_else(util::default_iface);
        let geo_fmt = GeoFmt::parse(&args.geo_format)
            .ok_or_else(|| anyhow::anyhow!("--geo-format: expected short, org, asn, place, full or off"))?;

        let geo_paths = match &args.geoip {
            Some(p) => vec![p.into()],
            None => Geo::default_paths(),
        };
        let geo = Arc::new(Geo::load(&geo_paths));

        let sort = match args.order.as_str() {
            "10s" => Sort::Col(1),
            "40s" => Sort::Col(2),
            "source" | "src" => Sort::Src,
            "destination" | "dst" => Sort::Dst,
            _ => Sort::Col(0),
        };
        let line_mode = match args.lines.as_str() {
            "two" => LineMode::Two,
            "sent" => LineMode::Sent,
            "recv" => LineMode::Recv,
            _ => LineMode::One,
        };

        let windows = match &args.windows {
            Some(w) => {
                let v: Vec<u64> = w.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() != 3 || v.iter().any(|&x| x == 0 || x as usize > FLOW_HIST - 4) {
                    anyhow::bail!("--windows needs three values in 1..={}, e.g. 2,10,40", FLOW_HIST - 4);
                }
                [v[0], v[1], v[2]]
            }
            None => [2, 10, 40],
        };
        let interval = Duration::from_millis(args.interval.clamp(50, 60_000));

        let mut app = App {
            stats,
            capture: None,
            iface: iface.clone(),
            promisc: args.promiscuous,
            user_nets,
            resolver: Resolver::new(4),
            geo_fmt: if geo.available() { geo_fmt } else { GeoFmt::Off },
            geo,
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
            line_mode,
            show_totals: true,
            log_scale: true,
            bytes: args.bytes,
            show_graph: true,
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
            selected_key: None,
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
            iface_hits: Vec::new(),
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
                self.tick();
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

    pub fn bar_val(&self, r: &Row) -> f64 {
        let i = self.bar_interval;
        match self.line_mode {
            LineMode::One => r.sent[i] + r.recv[i],
            LineMode::Two => r.sent[i].max(r.recv[i]),
            LineMode::Sent => r.sent[i],
            LineMode::Recv => r.recv[i],
        }
    }

    fn host_label(&self, ip: &IpAddr) -> String {
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
        let (tx, rx) = st.filtered.series(now, TOTAL_HIST);
        self.graph_tx = tx;
        self.graph_rx = rx;

        // ----- rows
        let show_ports = self.show_lport || self.show_rport;
        let mut map: HashMap<RowKey, Row> = HashMap::new();
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
        drop(st);

        let mut rows: Vec<Row> = map.into_values().collect();
        for r in rows.iter_mut() {
            r.local = self.endpoint(r.key.local, r.key.lport, r.key.proto);
            r.remote = self.endpoint(r.key.remote, r.key.rport, r.key.proto);
            if self.geo_fmt != GeoFmt::Off {
                // Geo of the peer (or of the local side if peers are aggregated).
                if let Some(ip) = r.key.remote.or(r.key.local) {
                    if let Some(g) = self.geo.lookup(&ip) {
                        r.geo = self.geo_fmt.render(&g);
                    }
                }
            }
        }
        if !self.search.is_empty() {
            let q = self.search.to_lowercase();
            rows.retain(|r| {
                r.local.to_lowercase().contains(&q)
                    || r.remote.to_lowercase().contains(&q)
                    || r.geo.to_lowercase().contains(&q)
            });
        }
        match self.sort {
            Sort::Col(i) => rows.sort_by(|a, b| {
                self.sort_val(b, i)
                    .total_cmp(&self.sort_val(a, i))
                    .then_with(|| a.remote.cmp(&b.remote))
                    .then_with(|| a.local.cmp(&b.local))
            }),
            Sort::Src => rows.sort_by(|a, b| a.local.cmp(&b.local).then_with(|| a.remote.cmp(&b.remote))),
            Sort::Dst => rows.sort_by(|a, b| a.remote.cmp(&b.remote).then_with(|| a.local.cmp(&b.local))),
        }
        if self.frozen {
            let pos: HashMap<&RowKey, usize> = self.order.iter().enumerate().map(|(i, k)| (k, i)).collect();
            rows.sort_by_key(|r| pos.get(&r.key).copied().unwrap_or(usize::MAX));
        }
        self.order = rows.iter().map(|r| r.key.clone()).collect();

        let cur_max = rows.iter().map(|r| self.bar_val(r)).fold(0.0, f64::max);
        self.scale = (self.scale * 0.98).max(cur_max).max(125.0);

        if let Some(sel) = &self.selected_key {
            if let Some(i) = rows.iter().position(|r| &r.key == sel) {
                self.selected = i;
            }
        }
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        self.rows = rows;
        self.selected_key = self.rows.get(self.selected).map(|r| r.key.clone());
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
        let Some(ip) = r.key.remote.or(r.key.local) else { return };
        self.host = Some(HostView {
            ip,
            probe: Probe::start(ip),
            flows: Vec::new(),
            rx: Vec::new(),
            tx: Vec::new(),
            rate: (0.0, 0.0),
            totals: (0, 0),
        });
    }

    fn move_sel(&mut self, d: i64) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as i64;
        self.selected = (self.selected as i64 + d).clamp(0, n - 1) as usize;
        self.selected_key = self.rows.get(self.selected).map(|r| r.key.clone());
    }

    // --------------------------------------------------------------- input

    fn on_mouse(&mut self, m: MouseEvent) {
        if self.popup != Popup::None && self.host.is_none() {
            self.on_popup_mouse(m);
            return;
        }
        if self.host.is_some() {
            if let MouseEventKind::Down(MouseButton::Right) = m.kind {
                self.host = None;
                self.popup = Popup::None;
            }
            return;
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.move_sel(3),
            MouseEventKind::ScrollUp => self.move_sel(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(&(_, idx)) = self.row_hits.iter().find(|(y, _)| *y == m.row) {
                    let again = matches!(self.last_click, Some((i, t)) if i == idx && t.elapsed() < Duration::from_millis(600));
                    let was_selected = self.selected == idx;
                    self.selected = idx;
                    self.selected_key = self.rows.get(idx).map(|r| r.key.clone());
                    if again || was_selected {
                        self.open_host();
                        self.last_click = None;
                    } else {
                        self.last_click = Some((idx, Instant::now()));
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
                if let Some(&(_, idx)) = self.iface_hits.iter().find(|(y, _)| *y == m.row) {
                    if self.iface_sel == idx {
                        self.pick_iface();
                    } else {
                        self.iface_sel = idx;
                    }
                }
            }
            MouseEventKind::ScrollDown if self.popup == Popup::Ifaces => self.move_iface_sel(1),
            MouseEventKind::ScrollUp if self.popup == Popup::Ifaces => self.move_iface_sel(-1),
            _ => {}
        }
    }

    fn move_iface_sel(&mut self, d: i64) {
        let n = self.ifaces.len() as i64;
        self.iface_sel = (self.iface_sel as i64 + d).clamp(0, (n - 1).max(0)) as usize;
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
                            self.update_filter(|f| {
                                f.expr = expr;
                                f.text = text;
                            });
                            self.input = None;
                            self.flash("filter applied");
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
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Backspace => self.host = None,
                KeyCode::Char('r') => {
                    // restart probes
                    if let Some(h) = &mut self.host {
                        h.probe = Probe::start(h.ip);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_sel(1);
                    self.open_host();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_sel(-1);
                    self.open_host();
                }
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
            KeyCode::Char('1') => self.set_sort(Sort::Col(0), "sort by 2s"),
            KeyCode::Char('2') => self.set_sort(Sort::Col(1), "sort by 10s"),
            KeyCode::Char('3') => self.set_sort(Sort::Col(2), "sort by 40s"),
            KeyCode::Char('<') => self.set_sort(Sort::Src, "sort by source"),
            KeyCode::Char('>') => self.set_sort(Sort::Dst, "sort by dest"),
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
            KeyCode::Char('+') | KeyCode::Char('=') => self.step_interval(1),
            KeyCode::Char('-') | KeyCode::Char('_') => self.step_interval(-1),
            KeyCode::Char('w') => {
                let cur = WINDOW_PRESETS.iter().position(|p| *p == self.windows);
                self.windows = WINDOW_PRESETS[cur.map_or(1, |i| (i + 1) % WINDOW_PRESETS.len())];
                let [a, b, c] = self.windows;
                self.flash(format!("averaging windows {a}s / {b}s / {c}s"));
            }
            KeyCode::Char('G') => {
                if self.geo.available() {
                    self.geo_fmt = self.geo_fmt.next();
                    self.flash(format!("geo column: {}", self.geo_fmt.name()));
                } else {
                    self.flash("no GeoIP databases found (use --geoip DIR)");
                }
            }
            KeyCode::Char('u') => {
                self.bytes = !self.bytes;
                self.flash(if self.bytes { "units: bytes" } else { "units: bits" });
            }
            KeyCode::Char('r') => {
                self.stats.lock().unwrap().reset();
                self.reset_view();
                self.flash("statistics reset");
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(20),
            KeyCode::PageUp => self.move_sel(-20),
            KeyCode::Home => self.move_sel(-(self.rows.len() as i64)),
            KeyCode::End => self.move_sel(self.rows.len() as i64),
            KeyCode::Enter => self.open_host(),
            _ => {}
        }
        false
    }

    pub fn port_name(&self, p: u16, proto: u8) -> String {
        self.services.name(p, proto).map(String::from).unwrap_or_else(|| p.to_string())
    }

    fn step_interval(&mut self, dir: i32) {
        let steps = &REFRESH_STEPS[..];
        let cur = self.interval.as_millis() as u64;
        let ms = if dir > 0 {
            steps.iter().copied().find(|&s| s > cur).unwrap_or(*steps.last().unwrap())
        } else {
            steps.iter().rev().copied().find(|&s| s < cur).unwrap_or(steps[0])
        };
        self.interval = Duration::from_millis(ms);
        self.flash(format!("update every {}", crate::util::fmt_ms(ms)));
    }

    fn set_sort(&mut self, s: Sort, msg: &str) {
        self.sort = s;
        self.flash(msg);
    }
}
