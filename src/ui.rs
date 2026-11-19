//! Rendering.

use crate::app::{App, InputKind, LineMode, Popup, Row, Sort, SPARK};
use crate::filter::Scope;
use crate::util::{self, fit, fmt_bytes, fmt_dur, fmt_rate, has_ports};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

const ACCENT: Color = Color::Rgb(122, 162, 247);
const DIM: Color = Color::Rgb(110, 115, 140);
const FG: Color = Color::Rgb(205, 210, 230);
const BORDER: Color = Color::Rgb(65, 72, 104);
const SEL_BG: Color = Color::Rgb(40, 44, 66);
const RX_C: Color = Color::Rgb(115, 218, 140);
const TX_C: Color = Color::Rgb(187, 154, 247);
const RX_BAR: Color = Color::Rgb(26, 72, 52);
const TX_BAR: Color = Color::Rgb(68, 46, 102);
const WARN: Color = Color::Rgb(224, 175, 104);
const ERR: Color = Color::Rgb(247, 118, 142);

#[derive(Clone, Copy)]
enum Pal {
    Rx,
    Tx,
}

fn lerp(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> Color {
    let f = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    Color::Rgb(f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

/// btop-ish gradient: low -> high.
fn grad(p: Pal, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (a, b, c) = match p {
        Pal::Rx => ((80, 200, 120), (230, 200, 70), (240, 90, 90)),
        Pal::Tx => ((90, 180, 235), (130, 120, 245), (225, 100, 220)),
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
        .border_style(Style::new().fg(BORDER))
        .title(title.into())
}

fn title(s: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(s.to_string(), Style::new().fg(FG).bold()),
        Span::raw(" "),
    ])
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let graph_h = if !app.show_graph {
        0
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
        Popup::Help => draw_help(f, area),
        Popup::Ifaces => draw_ifaces(f, area, app),
        Popup::None => {}
    }
}

// ------------------------------------------------------------------ header

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let (scope, ftext) = app.filter_desc();
    let sep = || Span::styled(" │ ", Style::new().fg(BORDER));
    let mut l = vec![
        Span::styled(" iptop ", Style::new().bg(ACCENT).fg(Color::Black).bold()),
        Span::raw(" "),
        Span::styled(format!("⇄ {}", app.iface), Style::new().fg(ACCENT).bold()),
    ];
    if app.promisc {
        l.push(Span::styled(" promisc", Style::new().fg(DIM)));
    }
    l.push(sep());
    l.push(match scope {
        Scope::All => Span::styled(scope.name(), Style::new().fg(DIM)),
        Scope::External => Span::styled(scope.name(), Style::new().fg(WARN).bold()),
        Scope::Internal => Span::styled(scope.name(), Style::new().fg(ACCENT).bold()),
    });
    if !ftext.is_empty() {
        l.push(sep());
        l.push(Span::styled("filter ", Style::new().fg(DIM)));
        l.push(Span::styled(ftext, Style::new().fg(TX_C).bold()));
    }
    if !app.search.is_empty() {
        l.push(sep());
        l.push(Span::styled("search ", Style::new().fg(DIM)));
        l.push(Span::styled(app.search.clone(), Style::new().fg(RX_C).bold()));
    }
    if let Some(e) = &app.capture_error {
        l.push(sep());
        l.push(Span::styled(e.clone(), Style::new().fg(ERR).bold()));
    }
    f.render_widget(Paragraph::new(Line::from(l)), area);

    let sort = match app.sort {
        Sort::Col(i) => format!("{}s", app.windows[i]),
        Sort::Src => "source".into(),
        Sort::Dst => "dest".into(),
    };
    let mut r = Vec::new();
    if app.paused {
        r.push(Span::styled(" PAUSED ", Style::new().bg(WARN).fg(Color::Black).bold()));
        r.push(Span::raw(" "));
    }
    if app.frozen {
        r.push(Span::styled(" FROZEN ", Style::new().bg(ACCENT).fg(Color::Black).bold()));
        r.push(Span::raw(" "));
    }
    r.push(Span::styled("⟳ ", Style::new().fg(ACCENT)));
    r.push(Span::styled(util::fmt_ms(app.interval.as_millis() as u64), Style::new().fg(FG)));
    r.push(sep());
    r.push(Span::styled(app.line_mode.name(), Style::new().fg(DIM)));
    r.push(sep());
    r.push(Span::styled(format!("sort {sort}"), Style::new().fg(DIM)));
    r.push(sep());
    r.push(Span::styled(util::clock(), Style::new().fg(FG)));
    r.push(Span::raw(" "));
    f.render_widget(Paragraph::new(Line::from(r)).alignment(Alignment::Right), area);
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
    let s = Style::new().fg(DIM);
    buf.set_string(area.x + 1, area.y, format!("↓ RX  {}/s", fmt_rate(tmax, bytes)), s);
    buf.set_string(area.x + 1, area.y + area.height - 1, format!("↑ TX  {}/s", fmt_rate(bmax, bytes)), s);
}

fn draw_graph(f: &mut Frame, area: Rect, app: &App) {
    let [g, side] = Layout::horizontal([Constraint::Min(20), Constraint::Length(30)]).areas(area);
    let secs = (g.width.saturating_sub(2) as usize * 2).min(app.graph_rx.len());
    let block = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("traffic", Style::new().fg(FG).bold()),
        Span::styled(format!(" · last {} ", fmt_dur(secs)), Style::new().fg(DIM)),
    ]));
    let inner = block.inner(g);
    f.render_widget(block, g);
    let (tm, bm) = mirror_graph(f.buffer_mut(), inner, &app.graph_rx, &app.graph_tx);
    graph_labels(f.buffer_mut(), inner, tm, bm, app.bytes);

    let b = app.bytes;
    let win_label = format!("{}/{}s", app.windows[1], app.windows[2]);
    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![
            Span::styled(format!("   {k:<8}"), Style::new().fg(DIM)),
            Span::styled(format!("{v:>15}"), Style::new().fg(c)),
        ])
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(format!(" ↓ RX {:<5}", format!("{}s", app.windows[0])), Style::new().fg(RX_C).bold()),
            Span::styled(format!("{:>15}", fmt_rate(app.rates[0].1, b)), Style::new().fg(RX_C).bold()),
        ]),
        kv(&win_label, format!("{}/{}", fmt_rate(app.rates[1].1, b), fmt_rate(app.rates[2].1, b)), FG),
        kv("peak", fmt_rate(app.peak.1, b), FG),
        kv("total", fmt_bytes(app.totals.1), FG),
        Line::from(vec![
            Span::styled(format!(" ↑ TX {:<5}", format!("{}s", app.windows[0])), Style::new().fg(TX_C).bold()),
            Span::styled(format!("{:>15}", fmt_rate(app.rates[0].0, b)), Style::new().fg(TX_C).bold()),
        ]),
        kv(&win_label, format!("{}/{}", fmt_rate(app.rates[1].0, b), fmt_rate(app.rates[2].0, b)), FG),
        kv("peak", fmt_rate(app.peak.0, b), FG),
        kv("total", fmt_bytes(app.totals.0), FG),
        Line::from(vec![
            Span::styled(format!(" {:.0} pkt/s", app.pps), Style::new().fg(DIM)),
            Span::styled(format!("  {} flows", app.nflows), Style::new().fg(DIM)),
        ]),
    ];
    let sb = panel(title("totals"));
    f.render_widget(Paragraph::new(lines).block(sb), side);
}

// ------------------------------------------------------------------ table

const SPARK_CH: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn spark_line(buf: &mut Buffer, x: u16, y: u16, w: usize, data: &[u64]) {
    let d = &data[data.len().saturating_sub(w)..];
    let max = d.iter().copied().max().unwrap_or(0);
    if max == 0 {
        buf.set_string(x, y, "·".repeat(d.len()), Style::new().fg(BORDER));
        return;
    }
    for (i, v) in d.iter().enumerate() {
        let lvl = if *v == 0 { 0 } else { ((*v as f64 / max as f64) * 8.0).ceil() as usize }.min(8);
        let (ch, c) = if lvl == 0 {
            ('·', BORDER)
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
    let geo_w = if !app.show_geo || w < 90 { 0 } else if w > 150 { 30 } else { 20 };
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
        Span::styled("hosts", Style::new().fg(FG).bold()),
        Span::styled(format!(" {n} "), Style::new().fg(DIM)),
    ]))
    .title_bottom(Line::from(vec![Span::styled(
        format!(
            " bars {}s {} · {} ",
            app.windows[app.bar_interval],
            if app.log_scale { "log" } else { "lin" },
            if app.bytes { "bytes" } else { "bits" }
        ),
        Style::new().fg(DIM),
    )]).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let c = columns(inner, app);
    let buf = f.buffer_mut();

    // header line
    let hs = Style::new().fg(DIM).add_modifier(Modifier::BOLD);
    let label_hdr = if app.hide_local {
        "remote".to_string()
    } else if app.hide_remote {
        "local".to_string()
    } else {
        let half = c.label_w.saturating_sub(3) / 2;
        format!("{:<half$}   remote", "local")
    };
    buf.set_stringn(c.label_x, inner.y, &label_hdr, c.label_w, hs);
    if c.geo_w > 0 {
        buf.set_stringn(c.geo_x, inner.y, "geo / asn", c.geo_w, hs);
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
        let sorted = matches!(app.sort, Sort::Col(s) if s == i);
        let st = if sorted { Style::new().fg(ACCENT).bold().underlined() } else { hs };
        buf.set_string(c.num_x + (i * c.num_w) as u16, inner.y, format!("{h:>w$}", w = c.num_w), st);
    }

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
        buf.set_string(c.label_x, inner.y + 2, msg, Style::new().fg(DIM).italic());
        return;
    }

    let b = app.bytes;
    for (vi, idx) in (app.offset..n).take(visible).enumerate() {
        let r = &app.rows[idx];
        let y = inner.y + 1 + (vi * lines_per) as u16;
        let sel = idx == app.selected;
        let row_rect = Rect::new(inner.x, y, inner.width, lines_per as u16);
        if sel {
            buf.set_style(row_rect, Style::new().bg(SEL_BG));
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
                    buf.set_style(Rect::new(c.label_x, y, wr, 1), Style::new().bg(RX_BAR));
                    buf.set_style(Rect::new(c.label_x + wr, y, w - wr, 1), Style::new().bg(TX_BAR));
                }
                LineMode::Two => {
                    let ws = (bar_frac(app, r.sent[i]) * bw).round() as u16;
                    let wr = (bar_frac(app, r.recv[i]) * bw).round() as u16;
                    buf.set_style(Rect::new(c.label_x, y, ws, 1), Style::new().bg(TX_BAR));
                    buf.set_style(Rect::new(c.label_x, y + 1, wr, 1), Style::new().bg(RX_BAR));
                }
                LineMode::Sent => {
                    let w = (bar_frac(app, r.sent[i]) * bw).round() as u16;
                    buf.set_style(Rect::new(c.label_x, y, w, 1), Style::new().bg(TX_BAR));
                }
                LineMode::Recv => {
                    let w = (bar_frac(app, r.recv[i]) * bw).round() as u16;
                    buf.set_style(Rect::new(c.label_x, y, w, 1), Style::new().bg(RX_BAR));
                }
            }
        }
        if sel {
            buf.set_string(inner.x, y, "▌", Style::new().fg(ACCENT));
        }
        let text = if sel { Style::new().fg(Color::White).bold() } else { Style::new().fg(FG) };
        let num = |v: f64| {
            if v < 0.5 {
                Style::new().fg(BORDER)
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
            buf.set_stringn(c.geo_x, y, fit(&r.geo, c.geo_w), c.geo_w, Style::new().fg(DIM));
        }
        if c.spark_w > 0 {
            spark_line(buf, c.spark_x, y, c.spark_w, &r.spark);
        }
        for (i, v) in vals1.iter().enumerate() {
            buf.set_string(c.num_x + (i * c.num_w) as u16, y, format!("{:>w$}", fmt_rate(*v, b), w = c.num_w), num(*v));
        }
        if app.show_totals {
            buf.set_string(c.num_x + (3 * c.num_w) as u16, y, format!("{:>w$}", fmt_bytes(tot1), w = c.num_w), Style::new().fg(DIM));
        }
        app.row_hits.push((y, idx));
        if app.line_mode == LineMode::Two {
            let y2 = y + 1;
            buf.set_stringn(c.label_x, y2, pair_label(app, r, c.label_w, "←", false), c.label_w, Style::new().fg(DIM));
            for (i, v) in r.recv.iter().enumerate() {
                buf.set_string(c.num_x + (i * c.num_w) as u16, y2, format!("{:>w$}", fmt_rate(*v, b), w = c.num_w), num(*v));
            }
            if app.show_totals {
                buf.set_string(c.num_x + (3 * c.num_w) as u16, y2, format!("{:>w$}", fmt_bytes(r.tot_recv), w = c.num_w), Style::new().fg(DIM));
            }
            app.row_hits.push((y2, idx));
        }
    }
    // scrollbar hint
    if n > visible {
        let pos = format!(" {}-{}/{} ", app.offset + 1, (app.offset + visible).min(n), n);
        let x = area.x + area.width.saturating_sub(pos.len() as u16 + 2);
        buf.set_string(x, area.y, pos, Style::new().fg(DIM));
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
            Span::styled(format!(" {p} "), Style::new().bg(ACCENT).fg(Color::Black).bold()),
            Span::raw(" "),
            Span::styled(app.input_buf.clone(), Style::new().fg(Color::White).bold()),
            Span::styled("█", Style::new().fg(ACCENT)),
            Span::styled(format!("   {hint}"), Style::new().fg(DIM)),
        ]);
        f.render_widget(Paragraph::new(l), area);
        if let Some(s) = app.status() {
            f.render_widget(
                Paragraph::new(Span::styled(format!("{s} "), Style::new().fg(WARN))).alignment(Alignment::Right),
                area,
            );
        }
        return;
    }
    let keys: &[(&str, &str)] = if app.host.is_some() {
        &[("esc", "back"), ("j/k", "prev/next host"), ("r", "restart probes")]
    } else {
        &[
            ("h", "help"),
            ("⏎", "host"),
            ("i", "iface"),
            ("x", "scope"),
            ("f", "filter"),
            ("l", "search"),
            ("t", "lines"),
            ("p", "ports"),
            ("s/d", "aggr"),
            ("G", "geo"),
            ("P", "pause"),
            ("q", "quit"),
        ]
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in keys {
        spans.push(Span::styled(k.to_string(), Style::new().fg(ACCENT).bold()));
        spans.push(Span::styled(format!(" {d}  "), Style::new().fg(DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if let Some(s) = app.status() {
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {s} "), Style::new().bg(SEL_BG).fg(WARN).bold()))
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
                ("1 2 3 < >", "sort by window 1 / 2 / 3 / source / dest"),
                ("o", "freeze current order"),
                ("b / B", "toggle bars / cycle which window the bars show"),
                ("L", "log / linear bar scale"),
                ("T", "toggle cumulative totals column"),
                ("P", "pause display"),
                ("l  or  /", "screen search (host, port, geo)"),
                ("f", "edit flow filter"),
                ("j k ↑ ↓", "move selection   PgUp/PgDn/Home/End"),
                ("q", "quit"),
            ],
        ),
        (
            "iptop extras",
            &[
                ("⏎ / click", "host panel: geo, ASN, ping, traceroute, flows"),
                ("i  [  ]", "interface picker / previous / next interface"),
                ("x", "scope: all → external only → internal only"),
                ("F", "clear filter and search"),
                ("G", "toggle geo / ASN column"),
                ("g", "toggle traffic graph"),
                ("+ / -", "update interval 100ms … 10s (like btop)"),
                ("w", "averaging windows 1/5/20 · 2/10/40 · 5/30/120 · 10/60/300 s"),
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
                ("not and or ()", "e.g. tcp and not (port 22 or port 443)"),
            ],
        ),
    ];
    let mut lines = Vec::new();
    for (name, keys) in sections {
        lines.push(Line::from(Span::styled(format!(" {name}"), Style::new().fg(ACCENT).bold())));
        for (k, d) in keys.iter() {
            lines.push(Line::from(vec![
                Span::styled(format!("   {k:<15}"), Style::new().fg(WARN)),
                Span::styled(d.to_string(), Style::new().fg(FG)),
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

fn draw_ifaces(f: &mut Frame, area: Rect, app: &App) {
    let h = app.ifaces.len() as u16 + 3;
    let r = centered(area, 84, h.max(6));
    f.render_widget(Clear, r);
    let block = panel(title("interfaces")).title_bottom(Line::from(" ⏎ switch · esc close ").right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let buf = f.buffer_mut();
    let hs = Style::new().fg(DIM).bold();
    buf.set_string(inner.x + 2, inner.y, format!("{:<16} {:<8} {:>9} {:>9}  addresses", "name", "state", "↓ rx", "↑ tx"), hs);
    for (i, e) in app.ifaces.iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let sel = i == app.iface_sel;
        if sel {
            buf.set_style(Rect::new(inner.x, y, inner.width, 1), Style::new().bg(SEL_BG));
            buf.set_string(inner.x, y, "▌", Style::new().fg(ACCENT));
        }
        let cur = e.name == app.iface;
        let name_style = if cur { Style::new().fg(ACCENT).bold() } else { Style::new().fg(FG) };
        let st_style = match e.state.as_str() {
            "up" => Style::new().fg(RX_C),
            "down" => Style::new().fg(ERR),
            _ => Style::new().fg(DIM),
        };
        let addrs: Vec<String> = e.addrs.iter().map(|a| a.to_string()).collect();
        buf.set_string(inner.x + 2, y, format!("{:<16} ", fit(&e.name, 16)), name_style);
        buf.set_string(inner.x + 19, y, format!("{:<8} ", fit(&e.state, 8)), st_style);
        buf.set_string(inner.x + 28, y, format!("{:>9}", fmt_rate(e.rx, app.bytes)), Style::new().fg(RX_C));
        buf.set_string(inner.x + 38, y, format!("{:>9}", fmt_rate(e.tx, app.bytes)), Style::new().fg(TX_C));
        let aw = inner.width.saturating_sub(50) as usize;
        buf.set_stringn(inner.x + 50, y, addrs.join(" "), aw, Style::new().fg(DIM));
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
        Span::styled(h.ip.to_string(), Style::new().fg(ACCENT).bold()),
    ];
    if let Some(n) = &name {
        t.push(Span::styled(format!("  {n}"), Style::new().fg(FG)));
    }
    t.push(Span::raw(" "));
    let block = panel(Line::from(t)).title_bottom(Line::from(" esc back · j/k prev/next · r restart probes ").right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);

    let [top, ping, bottom] = Layout::vertical([Constraint::Length(8), Constraint::Length(7), Constraint::Min(5)]).areas(inner);
    let [info, traffic] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(top);
    let [trace, flows] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(bottom);

    // --- info
    let geo = app.geo.lookup(&h.ip);
    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![Span::styled(format!(" {k:<9}"), Style::new().fg(DIM)), Span::styled(v, Style::new().fg(c))])
    };
    let internal = crate::filter::is_internal(&h.ip, &app.stats.lock().unwrap().filter.internal_nets);
    let mut lines = vec![
        kv("address", h.ip.to_string(), FG),
        kv("name", name.clone().unwrap_or_else(|| if app.dns { "…".into() } else { "(dns off)".into() }), FG),
        kv("scope", if internal { "internal".into() } else { "external".into() }, if internal { ACCENT } else { WARN }),
    ];
    match &geo {
        Some(g) => {
            if !g.place().is_empty() {
                lines.push(kv("location", g.place(), RX_C));
            }
            if !g.as_text().is_empty() {
                lines.push(kv("ASN", g.as_text(), TX_C));
            }
        }
        None => lines.push(kv("geo", if app.geo.available() { "unknown".into() } else { "no GeoIP db".into() }, DIM)),
    }
    lines.push(kv("traffic", format!("↓ {}  ↑ {}", fmt_bytes(h.totals.1), fmt_bytes(h.totals.0)), FG));
    f.render_widget(Paragraph::new(lines).block(panel(title("host"))), info);

    // --- traffic graph
    let tb = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("traffic", Style::new().fg(FG).bold()),
        Span::styled(format!(" ↓ {}  ↑ {} ", fmt_rate(h.rate.1, app.bytes), fmt_rate(h.rate.0, app.bytes)), Style::new().fg(DIM)),
    ]));
    let ti = tb.inner(traffic);
    f.render_widget(tb, traffic);
    let (tm, bm) = mirror_graph(f.buffer_mut(), ti, &h.rx, &h.tx);
    graph_labels(f.buffer_mut(), ti, tm, bm, app.bytes);

    // --- ping
    let st = h.probe.state.lock().unwrap();
    let mut pt = vec![Span::raw(" "), Span::styled("ping", Style::new().fg(FG).bold()), Span::raw(" ")];
    if let Some((last, min, avg, max)) = st.rtt_stats() {
        pt.push(Span::styled(format!("{last:.1} ms", ), Style::new().fg(rtt_color(last)).bold()));
        pt.push(Span::styled(format!("  min {min:.1}  avg {avg:.1}  max {max:.1}"), Style::new().fg(DIM)));
    }
    if let Some(loss) = st.loss() {
        let c = if loss > 0.0 { ERR } else { RX_C };
        pt.push(Span::styled(format!("  loss {loss:.0}%"), Style::new().fg(c)));
    }
    pt.push(Span::raw(" "));
    let pb = panel(Line::from(pt));
    let pi = pb.inner(ping);
    f.render_widget(pb, ping);
    {
        let buf = f.buffer_mut();
        if let Some(e) = &st.ping_err {
            buf.set_string(pi.x + 1, pi.y, e, Style::new().fg(ERR));
        } else if st.replies.is_empty() {
            buf.set_string(pi.x + 1, pi.y, if st.max_seq > 2 { "no replies" } else { "pinging…" }, Style::new().fg(DIM).italic());
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
                            c.set_char('×').set_fg(ERR);
                        }
                    }
                    None => {}
                }
            }
        }
    }

    // --- traceroute
    let mut tt = vec![Span::raw(" "), Span::styled("traceroute", Style::new().fg(FG).bold())];
    if !st.trace_tool.is_empty() {
        tt.push(Span::styled(format!(" ({})", st.trace_tool), Style::new().fg(DIM)));
    }
    if !st.trace_done {
        tt.push(Span::styled(" running…", Style::new().fg(WARN)));
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
            buf.set_string(tri.x + 1, tri.y, e, Style::new().fg(ERR));
        }
        let w = tri.width as usize;
        let name_w = w.saturating_sub(4 + 16 + 9 + 4) * 45 / 100;
        let geo_w = w.saturating_sub(4 + 16 + 9 + name_w + 4);
        for (i, hop) in hops.iter().enumerate().take(tri.height as usize) {
            let y = tri.y + i as u16;
            let mut x = tri.x + 1;
            buf.set_string(x, y, format!("{:>2}", hop.ttl), Style::new().fg(DIM));
            x += 3;
            match hop.ip {
                None => {
                    buf.set_string(x, y, "*", Style::new().fg(DIM));
                }
                Some(ip) => {
                    let is_target = ip == h.ip;
                    let st = if is_target { Style::new().fg(ACCENT).bold() } else { Style::new().fg(FG) };
                    buf.set_stringn(x, y, ip.to_string(), 16, st);
                    x += 17;
                    let rtt = hop.rtt.map(|r| format!("{r:>6.1}ms")).unwrap_or_default();
                    buf.set_string(x, y, &rtt, Style::new().fg(hop.rtt.map(rtt_color).unwrap_or(DIM)));
                    x += 9;
                    let nm = if app.dns { app.resolver.lookup(&ip).unwrap_or_default() } else { String::new() };
                    buf.set_stringn(x, y, fit(&nm, name_w), name_w, Style::new().fg(DIM));
                    x += name_w as u16 + 1;
                    if let Some(g) = app.geo.lookup(&ip) {
                        let s = g.short();
                        buf.set_stringn(x, y, fit(&s, geo_w), geo_w, Style::new().fg(TX_C));
                    }
                }
            }
        }
    }

    // --- flows
    let fb = panel(Line::from(vec![
        Span::raw(" "),
        Span::styled("flows", Style::new().fg(FG).bold()),
        Span::styled(format!(" {} ", h.flows.len()), Style::new().fg(DIM)),
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
        buf.set_stringn(fi.x + 1, y, fit(&desc, dw), dw, Style::new().fg(FG));
        let x = fi.x + 1 + dw as u16;
        buf.set_string(x, y, format!("{:>8}", fmt_rate(*rr, app.bytes)), Style::new().fg(RX_C));
        buf.set_string(x + 9, y, format!("{:>8}", fmt_rate(*s, app.bytes)), Style::new().fg(TX_C));
        buf.set_string(x + 18, y, format!("{:>9}", fmt_bytes(*tot)), Style::new().fg(DIM));
    }
}

fn app_port(app: &App, p: u16, proto: u8) -> String {
    app.port_name(p, proto)
}
