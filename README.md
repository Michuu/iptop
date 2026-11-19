<div align="center">

```
              _       _
             (_)_ __ | |_ ___  _ __
             | | '_ \| __/ _ \| '_ \          .--------.
             | | |_) | || (_) | |_) |        /  ⊙    ⊙  \    "who is using
             |_| .__/ \__\___/| .__/        |    ‿‿‿    |    all the bandwidth?"
               |_|            |_|            \  ⇄ ⇄ ⇄  /
                                              '--------'
         ⣀⣠⣤⣶⣿⣷⣦⣄⣀⡀  ⢀⣀⣤⣶⣿⣿⣶⣤⣀  ⣀⣤⣴⣶⣦⣤⣀⡀⢀⣀⣤⣶⣾⣿⣷⣶⣤⣀  ⣀⣠⣴⣶⣦⣄⣀
         ⠉⠛⠻⠿⣿⠿⠟⠋⠉⠁  ⠈⠉⠛⠿⣿⣿⠿⠛⠉  ⠉⠛⠻⠿⠟⠛⠉⠁⠈⠉⠛⠿⢿⣿⡿⠿⠛⠉  ⠉⠙⠻⠿⠟⠋⠉
```

**iftop, but it went to the gym and discovered colours.**

*iptop is to iftop what btop is to top.*

</div>

---

iptop is a terminal bandwidth monitor written in Rust. It shows **who your machine is talking to,
how fast, and where in the world they are**, with braille graphs, sparklines, GeoIP and ASN.
It also keeps iftop's muscle memory: if you know iftop's keys, you already know iptop.

```
 iptop  ⇄ enp1s0 │ external only                                              ⟳ 1s │ one-line │ sort 2s │ 13:37:00
╭ traffic · last 3m ───────────────────────────────────────────────────╮╭ totals ──────────────────────╮
│ ↓ RX  48.2Mb/s                                       ⣀⣠⣤⣴⣶⣾⣿⣷⣶⣦⣤⡀    ││ ↓ RX 2s             31.4Mb    │
│                                       ⢀⣀⣠⣤⣴⣶⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣦⣄⣀ ││   10/40s     28.9Mb/22.0Mb    │
│ ⣀⣀⣀⣀⣠⣤⣤⣤⣴⣶⣶⣶⣶⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿ ││   peak              48.2Mb    │
│ ⠉⠉⠉⠉⠉⠙⠛⠛⠛⠛⠻⠿⠿⠿⠿⠿⠿⠿⠿⠿⠿⠿⠿⠿⠿⠿⢿⣿⣿⣿⣿⣿⡿⠿⠿⠿⠿⠿⠿⠛⠛⠛⠛⠋⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉ ││ ↑ TX 2s             4.12Mb    │
│ ↑ TX  9.80Mb/s                     ⠈⠛⠿⠟⠋                                  ││ 4210 pkt/s  87 flows          │
╰──────────────────────────────────────────────────────────────────────╯╰───────────────────────────────╯
╭ hosts 23 ──────────────────────────────────────────────────────────────────────────────────────────────╮
│  local          remote                    geo / asn              last 32s           2s     10s   total │
│▌ myserver     ⇄ mirror.example.org        DE AS24940 Hetzner     ▂▃▅▆▇██▇▇█      21.3Mb  19.8Mb  1.2GB │
│  myserver     ⇄ 203.0.113.42              US AS15169 Google LLC  ▁▁▂▂▃▃▂▂▃▅      6.40Mb  5.11Mb   310MB │
│  myserver     ⇄ cdn.example.net           PL AS13335 Cloudflare  ····▁▃█▅▂▁      1.02Mb   880Kb  41.0MB │
│  myserver     ⇄ 198.51.100.7              NL AS1136 KPN          █▇▅▃▂▁····        12Kb   402Kb  9.71MB │
╰────────────────────────────────────────────────────────────────────────────── bars 2s log · bits ──╯
 h help  ⏎ host  i iface  x scope  f filter  l search  t lines  p ports  s/d aggr  G geo  P pause  q quit
```
<sub>(illustrative: the real thing has more colour gradients than markdown can hold)</sub>

## Features

- 🦀 **Rust, no libpcap.** It reads packets from a raw Linux `AF_PACKET` socket. The kernel tells
  us whether each packet was sent or received, which works on `any`, `tun` and `wg` interfaces too.
- ⌨️ **iftop keys**: `n N s d S D p t 1 2 3 < > o b B L T P l f j k q` behave as you expect.
  One line per host pair is the default, so you don't need to press `t` every time.
- 📈 **btop-style display**: a mirrored braille RX/TX graph, gradient sparklines per host,
  bars that show the RX/TX split, an adjustable update rate (`+`/`-`) and adjustable averaging
  windows (`w`).
- 🌍 **GeoIP + ASN** from MaxMind `.mmdb` files (GeoLite2 City/ASN, or ones patched with your own networks).
- 🔬 **Host panel** (Enter or click): live ping with a loss graph, a traceroute where every hop
  gets geo and ASN, the host's traffic graph, and its individual flows.
- 🧹 **Filters that are applied to history:** a BPF-ish filter language (`f`), a quick screen
  search (`/`), and an *external only* scope (`x`). Change them and the whole view, graph
  included, updates immediately.
- 🔌 **Interface picker** (`i`, or `[` / `]`) with live rates per interface.
- 🐁 Mouse support: wheel to scroll, click to select, click again for the host panel.

## Install

```sh
cargo build --release
sudo install -m755 target/release/iptop /usr/local/bin/
sudo setcap cap_net_raw,cap_net_admin=eip /usr/local/bin/iptop   # optional: run without sudo
```

When iptop runs as root, traceroute sends ICMP probes, which get through more firewalls.
Otherwise it falls back to UDP `traceroute` or `tracepath`.

## Usage

```sh
sudo iptop                          # default-route interface
sudo iptop -i any -x                # every interface, external peers only
sudo iptop -x -I 10.0.0.0/8 -I 198.51.100.7   # hide traffic between your own networks/hosts
sudo iptop -f 'tcp and not (port 22 or port 443)'
sudo iptop -P --lines two           # ports + classic iftop two-line view
sudo iptop -u 500 -w 1,5,20         # update twice a second, 1/5/20 s averages
```

| option | |
|---|---|
| `-i IFACE` | interface (`any` = all) |
| `-n` / `-N` | no DNS / no port names |
| `-p` | promiscuous mode |
| `-P` | show ports |
| `-b` / `-B` | no bars / show bytes instead of bits |
| `-F CIDR` | local network (for the direction of promiscuous traffic) |
| `-f EXPR` | flow filter (see below) |
| `-x` | external only |
| `-I CIDR` | extra networks/hosts to treat as internal (repeatable) |
| `-o 2s\|10s\|40s\|source\|destination` | initial sort |
| `--lines one\|two\|sent\|recv` | line display |
| `-u MS` | update interval |
| `-w A,B,C` | averaging windows in seconds |
| `--geoip PATH` | `.mmdb` file or directory |

## Keys

| iftop classics | | iptop extras | |
|---|---|---|---|
| `n` / `N` | DNS / port names | `⏎` / click | **host panel**: geo, ASN, ping, traceroute, flows |
| `s` / `d` | hide local / remote host | `i`, `[` `]` | interface picker / prev / next |
| `S` `D` `p` | local / remote / all ports | `x` | scope: all → external → internal |
| `t` | two-line / one-line / sent / recv | `F` | clear filter + search |
| `1 2 3 < >` | sort by window / src / dst | `G` | geo/ASN column |
| `o` | freeze order | `g` | traffic graph |
| `b` / `B` | bars / bar window | `u` | bits ↔ bytes |
| `L` | log / linear bars | `r` | reset stats |
| `T` | cumulative totals | `+` / `-` | update interval 100 ms … 10 s |
| `P` | pause | `w` | windows 1/5/20 · 2/10/40 · 5/30/120 · 10/60/300 s |
| `l` or `/` | screen search | `j k ↑ ↓ PgUp PgDn` | move selection |
| `f` | edit filter | `h` / `?` | help |
| `q` | quit | | |

### Filter language

```
host|net CIDR          either side          src|dst [host|net] CIDR   local / remote side
port N                 either side          src port N, dst port N
tcp udp icmp sctp gre esp   proto N   ip   ip6
external               has a non-internal end      internal    both ends internal
not ! and && or || ( )                      juxtaposition means "and"
```

For example: `tcp and not (port 22 or port 443)`, `dst net 8.8.0.0/16 or udp`, `external and port 53`.

**Internal** means RFC1918, CGNAT, loopback, link-local, ULA, broadcast and multicast, plus anything you
pass with `-I`.

## GeoIP

iptop reads any MaxMind-format database: GeoLite2 City/Country/ASN, DB-IP lite, or your own
patched copy. It merges what it finds, so City + ASN gives `Warsaw, Poland (PL) · AS8501 …`.

Search order: `--geoip PATH` → `~/geoip-*/mmdb/` → `~/geoip-*/` (also under `$SUDO_USER`'s home) →
`~/.local/share/iptop/` → `/usr/share/GeoIP` → `/var/lib/GeoIP` → `/usr/local/share/GeoIP`.

## Hacking

```
src/capture.rs   AF_PACKET socket + IPv4/IPv6 parsing
src/stats.rs     per-flow ring buffers of 1-second slots
src/filter.rs    scope + filter language
src/app.rs       state, aggregation into rows, keys
src/ui.rs        ratatui rendering (braille graphs, table, popups, host panel)
src/probe.rs     ping / traceroute runners
src/geo.rs       mmdb lookups
src/resolver.rs  async reverse DNS
```

`cargo test` runs the parser, filter and probe tests.

## Credits

Inspired by [iftop](https://pdw.ex-parrot.com/iftop/) by Paul Warren and Chris Lightfoot, and
[btop](https://github.com/aristocratos/btop). No packets were harmed in the making of this tool;
they were only counted.

License: GPL-2.0-or-later.
