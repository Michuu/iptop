//! Rendering.

use crate::app::{App, InputKind, LineMode, MapMode, Popup, Row, Sort, SCALES, SPARK};
use crate::filter::Scope;
use crate::geo::GeoFmt;
use crate::theme::th;
use crate::util::{self, fit, fmt_bytes, fmt_dur, fmt_rate, has_ports};
use ratatui::buffer::Buffer;
use std::collections::HashMap;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

#[derive(Clone, Copy)]
enum Pal {
    Rx,
    Tx,
    Heat,
}

fn lerp(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> Color {
    let f = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    Color::Rgb(f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

/// btop-ish gradient: low -> high.
fn grad(p: Pal, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let t_ = th();
    if t_.is_tty() {
        return match (p, t < 0.34, t < 0.67) {
            (Pal::Rx | Pal::Heat, true, _) => Color::Green,
            (Pal::Tx, true, _) => Color::Blue,
            (Pal::Tx, false, true) => Color::Magenta,
            (_, false, true) => Color::Yellow,
            _ => Color::Red,
        };
    }
    let [a, b, c] = match p {
        Pal::Rx => t_.rx_grad,
        Pal::Tx => t_.tx_grad,
        Pal::Heat => t_.heat_grad,
    };
    if t < 0.5 {
        lerp(a, b, t * 2.0)
    } else {
        lerp(b, c, (t - 0.5) * 2.0)
    }
}

fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th().border))
        .title(title.into())
}

fn title(s: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(s.to_string(), Style::new().fg(th().fg).bold()),
        Span::raw(" "),
    ])
}

pub fn draw(f: &mut Frame, app: &mut App) {
    draw_all(f, app);
    // Themes with their own background: paint every cell still using the
    // terminal's default colours (including cleared popup areas).
    if let Some(bg) = th().bg {
        let fg = th().fg;
        for c in f.buffer_mut().content.iter_mut() {
            if c.bg == Color::Reset {
                c.bg = bg;
            }
            if c.fg == Color::Reset {
                c.fg = fg;
            }
        }
    }
}

fn draw_all(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let graph_h = if !app.show_graph && app.map == MapMode::Off {
        0
    } else if app.map == MapMode::Big {
        (area.height * 45 / 100).max(10)
    } else if area.height < 26 {
        8
    } else {
        (area.height / 4).clamp(11, 16)
    };
    let [header, graph, table, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(graph_h),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(f, header, app);
    if graph_h > 0 {
        draw_graph(f, graph, app);
    }
    draw_table(f, table, app);
    draw_footer(f, footer, app);
    if app.host.is_some() {
        draw_host(f, area, app);
    }
    match app.popup {
        Popup::Help => {
            app.popup_area = None;
            draw_help(f, area)
        }
        Popup::Ifaces => draw_ifaces(f, area, app),
        Popup::Sort => draw_sort(f, area, app),
        Popup::None => {}
    }
}

// ------------------------------------------------------------------ header

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let (scope, ftext) = app.filter_desc();
    let sep = || Span::styled(" │ ", Style::new().fg(th().border));
    let mut l = vec![
        Span::styled(" iptop ", Style::new().bg(th().accent).fg(th().on_accent).bold()),
        Span::raw(" "),
        Span::styled(format!("⇄ {}", app.iface), Style::new().fg(th().accent).bold()),
    ];
    if app.promisc {
        l.push(Span::styled(" promisc", Style::new().fg(th().dim)));
    }
    l.push(sep());
    l.push(match scope {
        Scope::All => Span::styled(scope.name(), Style::new().fg(th().dim)),
        Scope::External => Span::styled(scope.name(), Style::new().fg(th().warn).bold()),
        Scope::Internal => Span::styled(scope.name(), Style::new().fg(th().accent).bold()),
    });
    if !ftext.is_empty() {
        l.push(sep());
        l.push(Span::styled("filter ", Style::new().fg(th().dim)));
        l.push(Span::styled(ftext, Style::new().fg(th().tx).bold()));
    }
    if !app.search.is_empty() {
        l.push(sep());
        l.push(Span::styled("search ", Style::new().fg(th().dim)));
        l.push(Span::styled(app.search.clone(), Style::new().fg(th().rx).bold()));
    }
    if let Some(e) = &app.capture_error {
        l.push(sep());
        l.push(Span::styled(e.clone(), Style::new().fg(th().err).bold()));
    }
    f.render_widget(Paragraph::new(Line::from(l)), area);

    let sort = format!("{} {}", app.sort.label(&app.windows), sort_arrow(app));
    let mut r = Vec::new();
    if app.following() {
        r.push(Span::styled(" FOLLOW ", Style::new().bg(th().rx).fg(th().on_accent).bold()));
        r.push(Span::raw(" "));
    } else if app.held() && !app.frozen && app.follow.is_none() {
        r.push(Span::styled("held ", Style::new().fg(th().dim).italic()));
    }
    if app.paused {
        r.push(Span::styled(" PAUSED ", Style::new().bg(th().warn).fg(th().on_accent).bold()));
        r.push(Span::raw(" "));
    }
    if app.frozen {
        r.push(Span::styled(" FROZEN ", Style::new().bg(th().accent).fg(th().on_accent).bold()));
        r.push(Span::raw(" "));
    }
    r.push(Span::styled("⟳ ", Style::new().fg(th().accent)));
    if let Some(i) = app.scale_idx {
        r.push(Span::styled(format!("{} ", SCALES[i].0), Style::new().fg(th().dim)));
    }
    r.push(Span::styled(util::fmt_ms(app.interval.as_millis() as u64), Style::new().fg(th().fg)));
    r.push(sep());
    r.push(Span::styled(app.line_mode.name(), Style::new().fg(th().dim)));
    r.push(sep());
    r.push(Span::styled(format!("sort {sort}"), Style::new().fg(th().dim)));
    r.push(sep());
    r.push(Span::styled(util::clock(), Style::new().fg(th().fg)));
    r.push(Span::raw(" "));
    f.render_widget(Paragraph::new(Line::from(r)).alignment(Alignment::Right), area);
}

/// ▾ biggest / Z first, ▴ smallest / A first.
fn sort_arrow(app: &App) -> &'static str {
    if app.sort.descending() != app.sort_rev {
        "▾"
    } else {
        "▴"
    }
}

// ------------------------------------------------------------------ world map

/// 0.5° land bitmap, 720 × 360, lat 90 → -90, lon -180 → 180, one bit per
/// cell, MSB first (rasterized from Natural Earth's public-domain 1:110m land).
static LAND: &[u8; 32400] = include_bytes!("../assets/land.bin");
/// Shown latitudes (Antarctica cropped).
const MAP_N: f32 = 84.0;
const MAP_S: f32 = -58.0;
const MAP_ASPECT: f32 = 360.0 / (MAP_N - MAP_S);

/// Blend two true colours (`t` of `b`); anything else just keeps `a`.
fn mix(a: Color, b: Color, t: f64) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => lerp((r1, g1, b1), (r2, g2, b2), t),
        _ => a,
    }
}

fn land_at(lat: f32, lon: f32) -> bool {
    let y = (((90.0 - lat) * 2.0) as i32).clamp(0, 359) as usize;
    let x = (((lon + 180.0) * 2.0) as i32).rem_euclid(720) as usize;
    let i = y * 720 + x;
    LAND[i / 8] & (0x80 >> (i % 8)) != 0
}

/// Pixelated world map: land/sea in half-block pixels, each located host's
/// traffic as a heat pixel (log scale), the selected host blinking.
fn draw_map(f: &mut Frame, area: Rect, app: &App) {
    let located = app.rows.iter().filter(|r| r.loc.is_some()).count();
    let block = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("world", Style::new().fg(th().fg).bold()),
        Span::styled(format!(" {located}/{} hosts ", app.rows.len()), Style::new().fg(th().dim)),
    ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 8 || inner.height < 3 {
        return;
    }
    let (pw, ph) = (inner.width as usize, inner.height as usize * 2);
    // Degrees per pixel, same on both axes; centre the map.
    let deg = (360.0 / pw as f32).max((MAP_N - MAP_S) / ph as f32);
    let mw = ((360.0 / deg) as usize).min(pw);
    let mh = (((MAP_N - MAP_S) / deg) as usize).min(ph);
    let (ox, oy) = ((pw - mw) / 2, (ph - mh) / 2);
    let to_px = |lat: f32, lon: f32| -> Option<(usize, usize)> {
        if !(MAP_S..=MAP_N).contains(&lat) {
            return None;
        }
        let x = (((lon + 180.0) / deg) as usize).min(mw - 1) + ox;
        let y = (((MAP_N - lat) / deg) as usize).min(mh - 1) + oy;
        Some((x, y))
    };

    // Land: a pixel is land if enough of its 3×3 samples are.
    let mut px = vec![th().bg.unwrap_or(Color::Reset); pw * ph];
    for y in 0..mh {
        for x in 0..mw {
            let mut n = 0;
            for sy in 0..3 {
                for sx in 0..3 {
                    let lat = MAP_N - (y as f32 + (sy as f32 + 0.5) / 3.0) * deg;
                    let lon = -180.0 + (x as f32 + (sx as f32 + 0.5) / 3.0) * deg;
                    n += land_at(lat, lon) as u32;
                }
            }
            px[(y + oy) * pw + x + ox] = if n >= 3 { th().land } else { th().sea };
        }
    }

    // Traffic heat per pixel over the bar window.
    let i = app.bar_interval;
    let mut heat: HashMap<(usize, usize), f64> = HashMap::new();
    for r in &app.rows {
        if let Some((x, y)) = r.loc.and_then(|(la, lo)| to_px(la, lo)) {
            *heat.entry((x, y)).or_default() += r.sent[i] + r.recv[i];
        }
    }
    let max = heat.values().cloned().fold(0.0, f64::max).max(125.0);
    let mut hot: Vec<((usize, usize), f64)> = heat
        .into_iter()
        .map(|(p, v)| (p, if v <= 1.0 { 0.0 } else { ((v / 125.0).max(1.0).log10() / (max / 125.0).log10().max(0.1)).clamp(0.0, 1.0) }))
        .collect();
    // Coolest first so the hottest pixel and glow win where they overlap.
    hot.sort_by(|a, b| a.1.total_cmp(&b.1));
    for &((x, y), t) in &hot {
        // A soft glow on the four neighbours keeps single pixels visible.
        let c = grad(Pal::Heat, t);
        for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx >= 0 && ny >= 0 && (nx as usize) < pw && (ny as usize) < ph {
                let i = ny as usize * pw + nx as usize;
                px[i] = mix(px[i], c, 0.45);
            }
        }
    }
    for &((x, y), t) in &hot {
        px[y * pw + x] = grad(Pal::Heat, t);
    }
    // Selected host, blinking.
    let blink = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() / 400)
        % 2
        == 0;
    if let Some((x, y)) = app.rows.get(app.selected).and_then(|r| r.loc).and_then(|(la, lo)| to_px(la, lo)) {
        if blink {
            px[y * pw + x] = th().bright;
        }
    }

    let buf = f.buffer_mut();
    for cy in 0..inner.height as usize {
        for cx in 0..pw {
            let (top, bot) = (px[2 * cy * pw + cx], px[(2 * cy + 1) * pw + cx]);
            if let Some(c) = buf.cell_mut((inner.x + cx as u16, inner.y + cy as u16)) {
                // Reset = no colour (letterbox, or the tty theme's sea): never
                // draw it as a foreground block.
                match (top == Color::Reset, bot == Color::Reset) {
                    (true, true) => c.set_symbol(" "),
                    (true, false) => c.set_symbol("▄").set_fg(bot),
                    (false, _) => c.set_symbol("▀").set_fg(top).set_bg(bot),
                };
            }
        }
    }
}

// ------------------------------------------------------------------ graph

const BR_UP_L: [u32; 4] = [0x40, 0x04, 0x02, 0x01]; // bottom -> top, left column
const BR_UP_R: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
const BR_DN_L: [u32; 4] = [0x01, 0x02, 0x04, 0x40]; // top -> bottom
const BR_DN_R: [u32; 4] = [0x08, 0x10, 0x20, 0x80];

fn tail(v: &[f64], n: usize) -> Vec<f64> {
    if v.len() >= n {
        v[v.len() - n..].to_vec()
    } else {
        let mut o = vec![0.0; n - v.len()];
        o.extend_from_slice(v);
        o
    }
}

fn set_braille(buf: &mut Buffer, x: u16, y: u16, bits: u32, fg: Color) {
    if bits == 0 {
        return;
    }
    if let Some(ch) = char::from_u32(0x2800 + bits) {
        if let Some(c) = buf.cell_mut((x, y)) {
            let mut tmp = [0u8; 4];
            c.set_symbol(ch.encode_utf8(&mut tmp)).set_fg(fg);
        }
    }
}

/// Filled, mirrored braille graph: `top` grows up from the middle, `bot`
/// grows down. Each cell holds two samples. Returns the (top, bot) scale.
fn mirror_graph(buf: &mut Buffer, area: Rect, top: &[f64], bot: &[f64]) -> (f64, f64) {
    if area.width == 0 || area.height < 2 {
        return (0.0, 0.0);
    }
    let w = area.width as usize;
    let top_h = (area.height + 1) / 2;
    let bot_h = area.height - top_h;
    let tv = tail(top, w * 2);
    let bv = tail(bot, w * 2);
    let tmax = tv.iter().cloned().fold(0.0, f64::max).max(1.0) * 1.05;
    let bmax = bv.iter().cloned().fold(0.0, f64::max).max(1.0) * 1.05;
    let lvl = |v: f64, max: f64, rows: u16| -> usize {
        if v <= 0.0 {
            0
        } else {
            ((v / max * rows as f64 * 4.0).round() as usize).max(1)
        }
    };
    for cx in 0..w {
        let x = area.x + cx as u16;
        let (l0, l1) = (lvl(tv[2 * cx], tmax, top_h), lvl(tv[2 * cx + 1], tmax, top_h));
        for r in 0..top_h {
            let base = ((top_h - 1 - r) * 4) as usize;
            let mut bits = 0;
            for i in 0..4 {
                if l0 > base + i {
                    bits |= BR_UP_L[i];
                }
                if l1 > base + i {
                    bits |= BR_UP_R[i];
                }
            }
            let frac = (top_h - r) as f64 / top_h as f64;
            set_braille(buf, x, area.y + r, bits, grad(Pal::Rx, frac));
        }
        let (l0, l1) = (lvl(bv[2 * cx], bmax, bot_h), lvl(bv[2 * cx + 1], bmax, bot_h));
        for r in 0..bot_h {
            let base = (r * 4) as usize;
            let mut bits = 0;
            for i in 0..4 {
                if l0 > base + i {
                    bits |= BR_DN_L[i];
                }
                if l1 > base + i {
                    bits |= BR_DN_R[i];
                }
            }
            let frac = (r + 1) as f64 / bot_h.max(1) as f64;
            set_braille(buf, x, area.y + top_h + r, bits, grad(Pal::Tx, frac));
        }
    }
    (tmax, bmax)
}

fn graph_labels(buf: &mut Buffer, area: Rect, tmax: f64, bmax: f64, bytes: bool) {
    if area.height < 2 {
        return;
    }
    let s = Style::new().fg(th().dim);
    buf.set_string(area.x + 1, area.y, format!("↓ RX  {}/s", fmt_rate(tmax, bytes)), s);
    buf.set_string(area.x + 1, area.y + area.height - 1, format!("↑ TX  {}/s", fmt_rate(bmax, bytes)), s);
}

fn draw_graph(f: &mut Frame, area: Rect, app: &App) {
    let [mut g, side] = Layout::horizontal([Constraint::Min(20), Constraint::Length(30)]).areas(area);
    if app.map != MapMode::Off {
        // Width that fits the map's aspect at this height (half-block pixels are square).
        let want = (area.height.saturating_sub(2) as f32 * 2.0 * MAP_ASPECT) as u16 + 2;
        let room_for_graph = app.show_graph && app.map == MapMode::Side && g.width >= want + 40;
        let m = if room_for_graph {
            let [rest, m] = Layout::horizontal([Constraint::Min(40), Constraint::Length(want)]).areas(g);
            g = rest;
            m
        } else {
            let m = g;
            g = Rect::default();
            m
        };
        draw_map(f, m, app);
        if g.width == 0 {
            let sb = panel(title("totals"));
            f.render_widget(Paragraph::new(totals_lines(app)).block(sb), side);
            return;
        }
    }
    let secs = (g.width.saturating_sub(2) as usize * 2).min(app.graph_rx.len()) * app.graph_step as usize;
    let block = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("traffic", Style::new().fg(th().fg).bold()),
        Span::styled(format!(" · last {} ", fmt_dur(secs)), Style::new().fg(th().dim)),
    ]));
    let inner = block.inner(g);
    f.render_widget(block, g);
    let (tm, bm) = mirror_graph(f.buffer_mut(), inner, &app.graph_rx, &app.graph_tx);
    graph_labels(f.buffer_mut(), inner, tm, bm, app.bytes);

    let sb = panel(title("totals"));
    f.render_widget(Paragraph::new(totals_lines(app)).block(sb), side);
}

fn totals_lines(app: &App) -> Vec<Line<'static>> {
    let b = app.bytes;
    let win_label = format!("{}/{}s", app.windows[1], app.windows[2]);
    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![
            Span::styled(format!("   {k:<8}"), Style::new().fg(th().dim)),
            Span::styled(format!("{v:>15}"), Style::new().fg(c)),
        ])
    };
    vec![
        Line::from(vec![
            Span::styled(format!(" ↓ RX {:<5}", format!("{}s", app.windows[0])), Style::new().fg(th().rx).bold()),
            Span::styled(format!("{:>15}", fmt_rate(app.rates[0].1, b)), Style::new().fg(th().rx).bold()),
        ]),
        kv(&win_label, format!("{}/{}", fmt_rate(app.rates[1].1, b), fmt_rate(app.rates[2].1, b)), th().fg),
        kv("peak", fmt_rate(app.peak.1, b), th().fg),
        kv("total", fmt_bytes(app.totals.1), th().fg),
        Line::from(vec![
            Span::styled(format!(" ↑ TX {:<5}", format!("{}s", app.windows[0])), Style::new().fg(th().tx).bold()),
            Span::styled(format!("{:>15}", fmt_rate(app.rates[0].0, b)), Style::new().fg(th().tx).bold()),
        ]),
        kv(&win_label, format!("{}/{}", fmt_rate(app.rates[1].0, b), fmt_rate(app.rates[2].0, b)), th().fg),
        kv("peak", fmt_rate(app.peak.0, b), th().fg),
        kv("total", fmt_bytes(app.totals.0), th().fg),
        Line::from(vec![
            Span::styled(format!(" {:.0} pkt/s", app.pps), Style::new().fg(th().dim)),
            Span::styled(format!("  {} flows", app.nflows), Style::new().fg(th().dim)),
        ]),
    ]
}

// ------------------------------------------------------------------ table

const SPARK_CH: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn spark_line(buf: &mut Buffer, x: u16, y: u16, w: usize, data: &[u64]) {
    let d = &data[data.len().saturating_sub(w)..];
    let max = d.iter().copied().max().unwrap_or(0);
    if max == 0 {
        buf.set_string(x, y, "·".repeat(d.len()), Style::new().fg(th().border));
        return;
    }
    for (i, v) in d.iter().enumerate() {
        let lvl = if *v == 0 { 0 } else { ((*v as f64 / max as f64) * 8.0).ceil() as usize }.min(8);
        let (ch, c) = if lvl == 0 {
            ('·', th().border)
        } else {
            (SPARK_CH[lvl], grad(Pal::Rx, lvl as f64 / 8.0))
        };
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_char(ch).set_fg(c);
        }
    }
}

fn bar_frac(app: &App, v: f64) -> f64 {
    if v <= 0.0 {
        return 0.0;
    }
    let max = app.scale.max(1.0);
    if app.log_scale {
        let min = 125.0; // 1 kbit/s
        if v <= min {
            return 0.01;
        }
        ((v / min).log10() / (max / min).log10().max(0.1)).clamp(0.0, 1.0)
    } else {
        (v / max).clamp(0.0, 1.0)
    }
}

struct Cols {
    label_x: u16,
    label_w: usize,
    geo_x: u16,
    geo_w: usize,
    spark_x: u16,
    spark_w: usize,
    num_x: u16,
    num_w: usize,
    ncols: usize,
}

fn columns(inner: Rect, app: &App) -> Cols {
    let w = inner.width as usize;
    let num_w = 9;
    let ncols = if app.show_totals { 4 } else { 3 };
    let nums = num_w * ncols;
    let spark_w = if w > 120 { SPARK } else if w > 95 { 16 } else { 0 };
    let wide = if app.geo_fmt == GeoFmt::Full { 40 } else { 30 };
    let geo_w = if app.geo_fmt == GeoFmt::Off || w < 90 { 0 } else if w > 150 { wide } else { 20 };
    let fixed = 2 + nums + if spark_w > 0 { spark_w + 2 } else { 0 } + if geo_w > 0 { geo_w + 1 } else { 0 };
    let label_w = w.saturating_sub(fixed).max(10);
    let label_x = inner.x + 2;
    let geo_x = label_x + label_w as u16 + 1;
    let spark_x = geo_x + if geo_w > 0 { geo_w as u16 + 1 } else { 0 };
    let num_x = spark_x + if spark_w > 0 { spark_w as u16 + 1 } else { 0 };
    Cols { label_x, label_w, geo_x, geo_w, spark_x, spark_w, num_x, num_w, ncols }
}

fn pair_label(app: &App, r: &Row, w: usize, arrow: &str, show_remote: bool) -> String {
    let (hl, hr) = (app.hide_local, app.hide_remote);
    if hl && hr {
        return fit("*", w);
    }
    if hl {
        return fit(&format!("{arrow} {}", if show_remote { &r.remote } else { "" }), w);
    }
    if hr {
        return fit(&r.local, w);
    }
    let half = w.saturating_sub(3) / 2;
    let left = fit(&r.local, half);
    let right = if show_remote { fit(&r.remote, w.saturating_sub(half + 3)) } else { String::new() };
    format!("{left:<half$} {arrow} {right}")
}

fn draw_table(f: &mut Frame, area: Rect, app: &mut App) {
    let n = app.rows.len();
    let block = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("hosts", Style::new().fg(th().fg).bold()),
        Span::styled(format!(" {n} "), Style::new().fg(th().dim)),
    ]))
    .title_bottom(Line::from(vec![Span::styled(
        format!(
            " bars {}s {} · {} ",
            app.windows[app.bar_interval],
            if app.log_scale { "log" } else { "lin" },
            if app.bytes { "bytes" } else { "bits" }
        ),
        Style::new().fg(th().dim),
    )]).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let c = columns(inner, app);
    let buf = f.buffer_mut();

    // header line; every column header is clickable to sort by it
    let hs = Style::new().fg(th().dim).add_modifier(Modifier::BOLD);
    let on = Style::new().fg(th().accent).bold().underlined();
    let arrow = sort_arrow(app);
    let mut hits = Vec::new();
    let mut head = |buf: &mut Buffer, x: u16, w: usize, text: &str, s: Sort, right: bool| {
        let cur = app.sort == s;
        let t = if cur { format!("{text} {arrow}") } else { text.to_string() };
        let t = if right { format!("{t:>w$}") } else { t };
        buf.set_stringn(x, inner.y, &t, w, if cur { on } else { hs });
        hits.push((x, x + w as u16, s));
    };
    if app.hide_local {
        head(buf, c.label_x, c.label_w, "remote", Sort::Dst, false);
    } else if app.hide_remote {
        head(buf, c.label_x, c.label_w, "local", Sort::Src, false);
    } else {
        let half = c.label_w.saturating_sub(3) / 2;
        head(buf, c.label_x, half, "local", Sort::Src, false);
        head(buf, c.label_x + half as u16 + 3, c.label_w.saturating_sub(half + 3), "remote", Sort::Dst, false);
    }
    if c.geo_w > 0 {
        let gs = if matches!(app.geo_fmt, GeoFmt::Place) { Sort::Country } else { Sort::Asn };
        let gs = if app.sort == Sort::Country { Sort::Country } else { gs };
        head(buf, c.geo_x, c.geo_w, app.geo_fmt.header(), gs, false);
    }
    if c.spark_w > 0 {
        buf.set_stringn(c.spark_x, inner.y, format!("last {}s", c.spark_w), c.spark_w, hs);
    }
    let hdrs = [
        format!("{}s", app.windows[0]),
        format!("{}s", app.windows[1]),
        format!("{}s", app.windows[2]),
        "total".to_string(),
    ];
    for (i, h) in hdrs.iter().take(c.ncols).enumerate() {
        let s = if i == 3 { Sort::Total } else { Sort::Col(i) };
        head(buf, c.num_x + (i * c.num_w) as u16, c.num_w, h, s, true);
    }
    app.header_hits = (inner.y, hits);

    // rows
    let lines_per = if app.line_mode == LineMode::Two { 2 } else { 1 };
    let visible = ((inner.height - 1) as usize / lines_per).max(1);
    if app.selected < app.offset {
        app.offset = app.selected;
    } else if app.selected >= app.offset + visible {
        app.offset = app.selected + 1 - visible;
    }
    app.offset = app.offset.min(n.saturating_sub(visible).max(0));
    app.row_hits.clear();

    if n == 0 {
        let msg = if app.capture_error.is_some() { "capture stopped" } else { "waiting for traffic…" };
        buf.set_string(c.label_x, inner.y + 2, msg, Style::new().fg(th().dim).italic());
        return;
    }

    let b = app.bytes;
    for (vi, idx) in (app.offset..n).take(visible).enumerate() {
        let r = &app.rows[idx];
        let y = inner.y + 1 + (vi * lines_per) as u16;
        let sel = idx == app.selected;
        let row_rect = Rect::new(inner.x, y, inner.width, lines_per as u16);
        if sel {
            buf.set_style(row_rect, Style::new().bg(th().sel_bg));
        }
        // bars
        if app.show_bars {
            let bw = c.label_w as f64 + 1.0;
            let i = app.bar_interval;
            match app.line_mode {
                LineMode::One => {
                    let tot = r.sent[i] + r.recv[i];
                    let w = (bar_frac(app, tot) * bw).round() as u16;
                    let wr = if tot > 0.0 { (w as f64 * r.recv[i] / tot).round() as u16 } else { 0 };
                    buf.set_style(Rect::new(c.label_x, y, wr, 1), Style::new().bg(th().rx_bar));
                    buf.set_style(Rect::new(c.label_x + wr, y, w - wr, 1), Style::new().bg(th().tx_bar));
                }
                LineMode::Two => {
                    let ws = (bar_frac(app, r.sent[i]) * bw).round() as u16;
                    let wr = (bar_frac(app, r.recv[i]) * bw).round() as u16;
                    buf.set_style(Rect::new(c.label_x, y, ws, 1), Style::new().bg(th().tx_bar));
                    buf.set_style(Rect::new(c.label_x, y + 1, wr, 1), Style::new().bg(th().rx_bar));
                }
                LineMode::Sent => {
                    let w = (bar_frac(app, r.sent[i]) * bw).round() as u16;
                    buf.set_style(Rect::new(c.label_x, y, w, 1), Style::new().bg(th().tx_bar));
                }
                LineMode::Recv => {
                    let w = (bar_frac(app, r.recv[i]) * bw).round() as u16;
                    buf.set_style(Rect::new(c.label_x, y, w, 1), Style::new().bg(th().rx_bar));
                }
            }
        }
        if sel {
            buf.set_string(inner.x, y, "▌", Style::new().fg(th().accent));
        }
        let text = if sel { Style::new().fg(th().bright).bold() } else { Style::new().fg(th().fg) };
        let num = |v: f64| {
            if v < 0.5 {
                Style::new().fg(th().border)
            } else {
                text
            }
        };

        let (arrow1, vals1, tot1): (&str, [f64; 3], u64) = match app.line_mode {
            LineMode::Two | LineMode::Sent => ("→", r.sent, r.tot_sent),
            LineMode::Recv => ("←", r.recv, r.tot_recv),
            LineMode::One => (
                "⇄",
                [r.sent[0] + r.recv[0], r.sent[1] + r.recv[1], r.sent[2] + r.recv[2]],
                r.tot_sent + r.tot_recv,
            ),
        };
        buf.set_stringn(c.label_x, y, pair_label(app, r, c.label_w, arrow1, true), c.label_w, text);
        if c.geo_w > 0 {
            buf.set_stringn(c.geo_x, y, fit(&r.geo, c.geo_w), c.geo_w, Style::new().fg(th().dim));
        }
        if c.spark_w > 0 {
            spark_line(buf, c.spark_x, y, c.spark_w, &r.spark);
        }
        for (i, v) in vals1.iter().enumerate() {
            buf.set_string(c.num_x + (i * c.num_w) as u16, y, format!("{:>w$}", fmt_rate(*v, b), w = c.num_w), num(*v));
        }
        if app.show_totals {
            buf.set_string(c.num_x + (3 * c.num_w) as u16, y, format!("{:>w$}", fmt_bytes(tot1), w = c.num_w), Style::new().fg(th().dim));
        }
        app.row_hits.push((y, idx));
        if app.line_mode == LineMode::Two {
            let y2 = y + 1;
            buf.set_stringn(c.label_x, y2, pair_label(app, r, c.label_w, "←", false), c.label_w, Style::new().fg(th().dim));
            for (i, v) in r.recv.iter().enumerate() {
                buf.set_string(c.num_x + (i * c.num_w) as u16, y2, format!("{:>w$}", fmt_rate(*v, b), w = c.num_w), num(*v));
            }
            if app.show_totals {
                buf.set_string(c.num_x + (3 * c.num_w) as u16, y2, format!("{:>w$}", fmt_bytes(r.tot_recv), w = c.num_w), Style::new().fg(th().dim));
            }
            app.row_hits.push((y2, idx));
        }
    }
    // scrollbar hint
    if n > visible {
        let pos = format!(" {}-{}/{} ", app.offset + 1, (app.offset + visible).min(n), n);
        let x = area.x + area.width.saturating_sub(pos.len() as u16 + 2);
        buf.set_string(x, area.y, pos, Style::new().fg(th().dim));
    }
}

// ------------------------------------------------------------------ footer

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    if let Some(kind) = app.input {
        let (p, hint) = match kind {
            InputKind::Search => ("search", "type to filter rows by host/geo · Enter keep · Esc clear"),
            InputKind::Filter => (
                "filter",
                "e.g. tcp and not port 22 · net 10.0.0.0/8 · external · Enter apply · Esc cancel",
            ),
        };
        let l = Line::from(vec![
            Span::styled(format!(" {p} "), Style::new().bg(th().accent).fg(th().on_accent).bold()),
            Span::raw(" "),
            Span::styled(app.input_buf.clone(), Style::new().fg(th().bright).bold()),
            Span::styled("█", Style::new().fg(th().accent)),
            Span::styled(format!("   {hint}"), Style::new().fg(th().dim)),
        ]);
        f.render_widget(Paragraph::new(l), area);
        if let Some(s) = app.status() {
            f.render_widget(
                Paragraph::new(Span::styled(format!("{s} "), Style::new().fg(th().warn))).alignment(Alignment::Right),
                area,
            );
        }
        return;
    }
    let keys: &[(&str, &str)] = if app.host.is_some() {
        &[
            ("esc", "back"),
            ("j/k", "prev/next host"),
            ("tab", "local↔remote"),
            ("e/E", "hide/only"),
            ("y", "copy"),
            ("G", "geo"),
            ("r", "restart probes"),
        ]
    } else {
        &[
            ("h", "help"),
            ("⏎", "host"),
            ("O", "sort"),
            ("␣", "follow"),
            ("i", "iface"),
            ("x", "scope"),
            ("f", "filter"),
            ("l", "search"),
            ("t", "lines"),
            ("p", "ports"),
            ("s/d", "aggr"),
            ("e/E", "hide/only"),
            ("G", "geo"),
            ("m", "map"),
            ("c", "theme"),
            ("P", "pause"),
            ("q", "quit"),
        ]
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in keys {
        spans.push(Span::styled(k.to_string(), Style::new().fg(th().accent).bold()));
        spans.push(Span::styled(format!(" {d}  "), Style::new().fg(th().dim)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if let Some(s) = app.status() {
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {s} "), Style::new().bg(th().sel_bg).fg(th().warn).bold()))
                .alignment(Alignment::Right),
            area,
        );
    }
}

// ------------------------------------------------------------------ popups

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_help(f: &mut Frame, area: Rect) {
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "iftop keys",
            &[
                ("n / N", "toggle DNS / port-name resolution"),
                ("s / d", "hide local (source) / remote (dest) host"),
                ("S / D / p", "toggle local / remote / all ports"),
                ("t", "cycle line display: two-line, one-line, sent, recv"),
                ("1 2 3 < >", "sort by window 1 / 2 / 3 / source / dest (again: reverse)"),
                ("o", "freeze current order"),
                ("b / B", "toggle bars / cycle which window the bars show"),
                ("L", "log / linear bar scale"),
                ("T", "toggle cumulative totals column"),
                ("P", "pause display"),
                ("l  or  /", "screen search (host, port, geo)"),
                ("f", "edit flow filter"),
                ("j k ↑ ↓", "move cursor (rows hold still)  PgUp/PgDn/End; Home: top"),
                ("q", "quit"),
            ],
        ),
        (
            "iptop extras",
            &[
                ("⏎ / click", "host panel: geo, ASN, ping, traceroute, flows"),
                ("tab / l", "host panel: probe the remote ↔ local host"),
                ("O / R", "sort menu (up, down, total, flows, ip, asn, …) / reverse"),
                ("click header", "sort by that column; again to reverse"),
                ("space", "follow the selected host as it moves (or stay in place)"),
                ("i  [  ]", "interface picker / previous / next interface"),
                ("x", "scope: all → external only → internal only"),
                ("e / E", "filter out this host / show only this host"),
                ("F", "clear filter and search"),
                ("y / Y", "copy IP / hostname to the clipboard"),
                ("G", "geo (table + traceroute): AS+org → org → AS → city → city+AS → off"),
                ("g", "toggle traffic graph"),
                ("m", "world map: off → beside the graph → big"),
                ("c", "colour theme: tokyo-night, dracula, nord, gruvbox, …"),
                ("+ / -", "time scale: update 1/2/5/10 s, averages 1/5/20 … 10/60/300 s"),
                ("u", "bits ↔ bytes"),
                ("r", "reset statistics"),
            ],
        ),
        (
            "filter syntax",
            &[
                ("host/net CIDR", "either side;  src/dst (= local/remote) CIDR"),
                ("port N", "either side;  src port N, dst port N"),
                ("tcp udp icmp", "protocols;  proto N;  ip / ip6"),
                ("external", "has an external peer;  internal"),
                ("asn cc org", "peer's GeoIP: asn 15169, cc PL, org \"google llc\""),
                ("not and or ()", "e.g. tcp and not (port 22 or port 443)"),
            ],
        ),
    ];
    let mut lines = Vec::new();
    for (name, keys) in sections {
        lines.push(Line::from(Span::styled(format!(" {name}"), Style::new().fg(th().accent).bold())));
        for (k, d) in keys.iter() {
            lines.push(Line::from(vec![
                Span::styled(format!("   {k:<15}"), Style::new().fg(th().warn)),
                Span::styled(d.to_string(), Style::new().fg(th().fg)),
            ]));
        }
        lines.push(Line::raw(""));
    }
    let h = lines.len() as u16 + 2;
    let r = centered(area, 76, h);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(panel(title("help")).title_bottom(Line::from(" any key to close ").right_aligned())),
        r,
    );
}

fn draw_ifaces(f: &mut Frame, area: Rect, app: &mut App) {
    let n = app.ifaces.len();
    let h = n as u16 + 3;
    let r = centered(area, 84, h.max(6).min(area.height.saturating_sub(2)));
    app.popup_area = Some((r.x, r.y, r.width, r.height));
    f.render_widget(Clear, r);
    // Rows that fit under the header; keep the selection in view.
    let vis = r.height.saturating_sub(3) as usize;
    if app.iface_sel < app.iface_off {
        app.iface_off = app.iface_sel;
    } else if vis > 0 && app.iface_sel >= app.iface_off + vis {
        app.iface_off = app.iface_sel + 1 - vis;
    }
    app.iface_off = app.iface_off.min(n.saturating_sub(vis));
    let pos = if n > vis { format!(" {}/{} ·", app.iface_sel + 1, n) } else { String::new() };
    let block = panel(title("interfaces"))
        .title_bottom(Line::from(format!("{pos} ⏎/click switch · esc close ")).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    app.popup_hits.clear();
    let buf = f.buffer_mut();
    let hs = Style::new().fg(th().dim).bold();
    buf.set_string(inner.x + 2, inner.y, format!("{:<16} {:<8} {:>9} {:>9}  addresses", "name", "state", "↓ rx", "↑ tx"), hs);
    for (i, e) in app.ifaces.iter().enumerate().skip(app.iface_off) {
        let y = inner.y + 1 + (i - app.iface_off) as u16;
        if y >= inner.y + inner.height {
            break;
        }
        app.popup_hits.push((y, i));
        let sel = i == app.iface_sel;
        if sel {
            buf.set_style(Rect::new(inner.x, y, inner.width, 1), Style::new().bg(th().sel_bg));
            buf.set_string(inner.x, y, "▌", Style::new().fg(th().accent));
        }
        let cur = e.name == app.iface;
        let name_style = if cur { Style::new().fg(th().accent).bold() } else { Style::new().fg(th().fg) };
        let st_style = match e.state.as_str() {
            "up" => Style::new().fg(th().rx),
            "down" => Style::new().fg(th().err),
            _ => Style::new().fg(th().dim),
        };
        let addrs: Vec<String> = e.addrs.iter().map(|a| a.to_string()).collect();
        buf.set_string(inner.x + 2, y, format!("{:<16} ", fit(&e.name, 16)), name_style);
        buf.set_string(inner.x + 19, y, format!("{:<8} ", fit(&e.state, 8)), st_style);
        buf.set_string(inner.x + 28, y, format!("{:>9}", fmt_rate(e.rx, app.bytes)), Style::new().fg(th().rx));
        buf.set_string(inner.x + 38, y, format!("{:>9}", fmt_rate(e.tx, app.bytes)), Style::new().fg(th().tx));
        let aw = inner.width.saturating_sub(50) as usize;
        buf.set_stringn(inner.x + 50, y, addrs.join(" "), aw, Style::new().fg(th().dim));
    }
}

fn draw_sort(f: &mut Frame, area: Rect, app: &mut App) {
    let n = Sort::MENU.len() as u16;
    let r = centered(area, 40, n + 2);
    app.popup_area = Some((r.x, r.y, r.width, r.height));
    f.render_widget(Clear, r);
    let block = panel(title("sort by")).title_bottom(Line::from(" ⏎/click pick · R reverse ").right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    app.popup_hits.clear();
    let arrow = sort_arrow(app);
    let buf = f.buffer_mut();
    for (i, s) in Sort::MENU.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        app.popup_hits.push((y, i));
        if i == app.sort_sel {
            buf.set_style(Rect::new(inner.x, y, inner.width, 1), Style::new().bg(th().sel_bg));
            buf.set_string(inner.x, y, "▌", Style::new().fg(th().accent));
        }
        let cur = *s == app.sort;
        let st = if cur { Style::new().fg(th().accent).bold() } else { Style::new().fg(th().fg) };
        let key = match s {
            Sort::Col(k) => format!("{}", k + 1),
            Sort::Src => "<".into(),
            Sort::Dst => ">".into(),
            _ => String::new(),
        };
        buf.set_string(inner.x + 2, y, format!("{key:>1}"), Style::new().fg(th().warn));
        let mark = if cur { format!(" {arrow}") } else { String::new() };
        buf.set_stringn(inner.x + 5, y, format!("{}{mark}", s.label(&app.windows)), inner.width.saturating_sub(6) as usize, st);
    }
}

// ------------------------------------------------------------------ host panel

fn rtt_color(ms: f64) -> Color {
    // 0 ms green … 150+ ms red
    grad(Pal::Rx, (ms / 150.0).min(1.0))
}

fn draw_host(f: &mut Frame, area: Rect, app: &App) {
    let Some(h) = &app.host else { return };
    let r = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    f.render_widget(Clear, r);
    let name = if app.dns { app.resolver.lookup(&h.ip) } else { None };
    let mut t = vec![
        Span::raw(" "),
        Span::styled(h.ip.to_string(), Style::new().fg(th().accent).bold()),
    ];
    if let Some(n) = &name {
        t.push(Span::styled(format!("  {n}"), Style::new().fg(th().fg)));
    }
    t.push(Span::styled(if h.local { "  local side" } else { "  remote side" }, Style::new().fg(th().dim)));
    t.push(Span::raw(" "));
    let block = panel(Line::from(t)).title_bottom(
        Line::from(" esc back · j/k prev/next · tab/l local↔remote · e/E hide/only · y/Y copy · G geo · r restart ").right_aligned(),
    );
    let inner = block.inner(r);
    f.render_widget(block, r);

    let [top, ping, bottom] = Layout::vertical([Constraint::Length(9), Constraint::Length(7), Constraint::Min(5)]).areas(inner);
    let [info, traffic] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(top);
    let [trace, flows] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(bottom);

    // --- info
    let geo = app.geo.lookup(&h.ip);
    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![Span::styled(format!(" {k:<9}"), Style::new().fg(th().dim)), Span::styled(v, Style::new().fg(c))])
    };
    let internal = crate::filter::is_internal(&h.ip, &app.stats.lock().unwrap().filter.internal_nets);
    let mut lines = vec![
        kv("address", h.ip.to_string(), th().fg),
        kv("name", name.clone().unwrap_or_else(|| if app.dns { "…".into() } else { "(dns off)".into() }), th().fg),
        kv("scope", if internal { "internal".into() } else { "external".into() }, if internal { th().accent } else { th().warn }),
    ];
    match &geo {
        Some(g) => {
            if !g.place().is_empty() {
                lines.push(kv("location", g.place(), th().rx));
            }
            if !g.as_text().is_empty() {
                lines.push(kv("ASN", g.as_text(), th().tx));
            }
            if let Some(n) = &g.net {
                lines.push(kv("network", n.clone(), th().dim));
            }
        }
        None => lines.push(kv("geo", if app.geo.available() { "unknown".into() } else { "no GeoIP db".into() }, th().dim)),
    }
    lines.push(kv("traffic", format!("↓ {}  ↑ {}", fmt_bytes(h.totals.1), fmt_bytes(h.totals.0)), th().fg));
    f.render_widget(Paragraph::new(lines).block(panel(title("host"))), info);

    // --- traffic graph
    let tb = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("traffic", Style::new().fg(th().fg).bold()),
        Span::styled(format!(" ↓ {}  ↑ {} ", fmt_rate(h.rate.1, app.bytes), fmt_rate(h.rate.0, app.bytes)), Style::new().fg(th().dim)),
    ]));
    let ti = tb.inner(traffic);
    f.render_widget(tb, traffic);
    let (tm, bm) = mirror_graph(f.buffer_mut(), ti, &h.rx, &h.tx);
    graph_labels(f.buffer_mut(), ti, tm, bm, app.bytes);

    // --- ping
    let st = h.probe.state.lock().unwrap();
    let mut pt = vec![Span::raw(" "), Span::styled("ping", Style::new().fg(th().fg).bold()), Span::raw(" ")];
    if let Some((last, min, avg, max)) = st.rtt_stats() {
        pt.push(Span::styled(format!("{last:.1} ms", ), Style::new().fg(rtt_color(last)).bold()));
        pt.push(Span::styled(format!("  min {min:.1}  avg {avg:.1}  max {max:.1}"), Style::new().fg(th().dim)));
    }
    if let Some(loss) = st.loss() {
        let c = if loss > 0.0 { th().err } else { th().rx };
        pt.push(Span::styled(format!("  loss {loss:.0}%"), Style::new().fg(c)));
    }
    pt.push(Span::raw(" "));
    let pb = panel(Line::from(pt));
    let pi = pb.inner(ping);
    f.render_widget(pb, ping);
    {
        let buf = f.buffer_mut();
        if let Some(e) = &st.ping_err {
            buf.set_string(pi.x + 1, pi.y, e, Style::new().fg(th().err));
        } else if st.replies.is_empty() {
            buf.set_string(pi.x + 1, pi.y, if st.max_seq > 2 { "no replies" } else { "pinging…" }, Style::new().fg(th().dim).italic());
        } else {
            // bar per probe, newest right; missing seqs shown as red '×'
            let w = pi.width as usize;
            let maxseq = st.max_seq;
            let first = maxseq.saturating_sub(w as u64 - 1).max(1);
            let maxr = st.replies.iter().filter(|(s, _)| *s >= first).map(|(_, r)| *r).fold(1.0, f64::max);
            let rows = pi.height as f64;
            for (i, seq) in (first..=maxseq).enumerate() {
                let x = pi.x + i as u16;
                match st.replies.iter().rev().find(|(s, _)| *s == seq) {
                    Some((_, rtt)) => {
                        let hgt = (rtt / maxr * rows * 8.0).max(1.0);
                        for row in 0..pi.height {
                            let from_bottom = (pi.height - 1 - row) as f64 * 8.0;
                            let fill = (hgt - from_bottom).clamp(0.0, 8.0) as usize;
                            if fill > 0 {
                                if let Some(c) = buf.cell_mut((x, pi.y + row)) {
                                    c.set_char(SPARK_CH[fill]).set_fg(rtt_color(*rtt));
                                }
                            }
                        }
                    }
                    None if seq < maxseq => {
                        if let Some(c) = buf.cell_mut((x, pi.y + pi.height - 1)) {
                            c.set_char('×').set_fg(th().err);
                        }
                    }
                    None => {}
                }
            }
        }
    }

    // --- traceroute
    let mut tt = vec![Span::raw(" "), Span::styled("traceroute", Style::new().fg(th().fg).bold())];
    if !st.trace_tool.is_empty() {
        tt.push(Span::styled(format!(" ({})", st.trace_tool), Style::new().fg(th().dim)));
    }
    if !st.trace_done {
        tt.push(Span::styled(" running…", Style::new().fg(th().warn)));
    }
    tt.push(Span::raw(" "));
    let trb = panel(Line::from(tt));
    let tri = trb.inner(trace);
    f.render_widget(trb, trace);
    let hops = st.hops.clone();
    let trace_err = st.trace_err.clone();
    drop(st);
    {
        let buf = f.buffer_mut();
        if let Some(e) = trace_err {
            buf.set_string(tri.x + 1, tri.y, e, Style::new().fg(th().err));
        }
        let w = tri.width as usize;
        // With geo off the hostname takes the whole remaining width.
        let rest = w.saturating_sub(4 + 16 + 9 + 4);
        let name_w = if app.geo_fmt == GeoFmt::Off { rest } else { rest * 45 / 100 };
        let geo_w = if app.geo_fmt == GeoFmt::Off { 0 } else { w.saturating_sub(4 + 16 + 9 + name_w + 4) };
        for (i, hop) in hops.iter().enumerate().take(tri.height as usize) {
            let y = tri.y + i as u16;
            let mut x = tri.x + 1;
            buf.set_string(x, y, format!("{:>2}", hop.ttl), Style::new().fg(th().dim));
            x += 3;
            match hop.ip {
                None => {
                    buf.set_string(x, y, "*", Style::new().fg(th().dim));
                }
                Some(ip) => {
                    let is_target = ip == h.ip;
                    let st = if is_target { Style::new().fg(th().accent).bold() } else { Style::new().fg(th().fg) };
                    buf.set_stringn(x, y, ip.to_string(), 16, st);
                    x += 17;
                    let rtt = hop.rtt.map(|r| format!("{r:>6.1}ms")).unwrap_or_default();
                    buf.set_string(x, y, &rtt, Style::new().fg(hop.rtt.map(rtt_color).unwrap_or(th().dim)));
                    x += 9;
                    let nm = if app.dns { app.resolver.lookup(&ip).unwrap_or_default() } else { String::new() };
                    buf.set_stringn(x, y, fit(&nm, name_w), name_w, Style::new().fg(th().dim));
                    x += name_w as u16 + 1;
                    if geo_w > 0 {
                        if let Some(g) = app.geo.lookup(&ip) {
                            let s = app.geo_fmt.render(&g);
                            buf.set_stringn(x, y, fit(&s, geo_w), geo_w, Style::new().fg(th().tx));
                        }
                    }
                }
            }
        }
    }

    // --- flows
    let fb = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("flows", Style::new().fg(th().fg).bold()),
        Span::styled(format!(" {} ", h.flows.len()), Style::new().fg(th().dim)),
    ]));
    let fi = fb.inner(flows);
    f.render_widget(fb, flows);
    let buf = f.buffer_mut();
    for (i, (k, s, rr, tot)) in h.flows.iter().enumerate().take(fi.height as usize) {
        let y = fi.y + i as u16;
        let port = |p: u16| {
            if !has_ports(k.proto) {
                String::new()
            } else if app.port_names {
                app_port(app, p, k.proto)
            } else {
                p.to_string()
            }
        };
        let desc = if has_ports(k.proto) {
            format!("{} {}→{}", util::proto_name(k.proto), port(k.lport), port(k.rport))
        } else {
            util::proto_name(k.proto)
        };
        let dw = fi.width.saturating_sub(30) as usize;
        buf.set_stringn(fi.x + 1, y, fit(&desc, dw), dw, Style::new().fg(th().fg));
        let x = fi.x + 1 + dw as u16;
        buf.set_string(x, y, format!("{:>8}", fmt_rate(*rr, app.bytes)), Style::new().fg(th().rx));
        buf.set_string(x + 9, y, format!("{:>8}", fmt_rate(*s, app.bytes)), Style::new().fg(th().tx));
        buf.set_string(x + 18, y, format!("{:>9}", fmt_bytes(*tot)), Style::new().fg(th().dim));
    }
}

fn app_port(app: &App, p: u16, proto: u8) -> String {
    app.port_name(p, proto)
}
