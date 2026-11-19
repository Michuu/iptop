//! Packet capture via a Linux AF_PACKET/SOCK_DGRAM socket (no libpcap).
//! The kernel strips the link layer and tells us the packet type, so we know
//! directly whether a packet was sent or received by this host.

use crate::filter::Cidr;
use crate::stats::{FlowKey, SharedStats};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::ffi::CString;
use std::mem::{size_of, zeroed};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const ETH_P_ALL: u16 = 0x0003;
const ETH_P_IP: u16 = 0x0800;
const ETH_P_IPV6: u16 = 0x86dd;
const PACKET_OTHERHOST: u8 = 3;
const PACKET_OUTGOING: u8 = 4;

pub struct Capture {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(iface: &str, promisc: bool, local_nets: Vec<Cidr>, stats: SharedStats) -> Result<Capture> {
        let fd = open_socket(iface, promisc)?;
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                capture_loop(fd, &s2, &local_nets, &stats);
                unsafe { libc::close(fd) };
            })?;
        Ok(Capture { stop, handle: Some(handle) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn open_socket(iface: &str, promisc: bool) -> Result<i32> {
    unsafe {
        let proto_be = ETH_P_ALL.to_be();
        let fd = libc::socket(libc::AF_PACKET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, proto_be as i32);
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EPERM) {
                bail!(
                    "permission denied opening a packet socket.\n\
                     Run as root, or once: sudo setcap cap_net_raw,cap_net_admin=eip $(which iptop)"
                );
            }
            return Err(e.into());
        }
        let ifindex = if iface == "any" {
            0
        } else {
            let c = CString::new(iface)?;
            let i = libc::if_nametoindex(c.as_ptr());
            if i == 0 {
                libc::close(fd);
                bail!("unknown interface '{iface}'");
            }
            i as i32
        };
        let mut sll: libc::sockaddr_ll = zeroed();
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = proto_be;
        sll.sll_ifindex = ifindex;
        if libc::bind(fd, &sll as *const _ as *const libc::sockaddr, size_of::<libc::sockaddr_ll>() as u32) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            bail!("bind to {iface}: {e}");
        }
        let tv = libc::timeval { tv_sec: 0, tv_usec: 200_000 };
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const _ as *const libc::c_void,
            size_of::<libc::timeval>() as u32,
        );
        // Bigger kernel buffer helps on busy links.
        let rcvbuf: libc::c_int = 4 << 20;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &rcvbuf as *const _ as *const libc::c_void,
            size_of::<libc::c_int>() as u32,
        );
        if promisc && ifindex != 0 {
            let mut mr: libc::packet_mreq = zeroed();
            mr.mr_ifindex = ifindex;
            mr.mr_type = libc::PACKET_MR_PROMISC as u16;
            libc::setsockopt(
                fd,
                libc::SOL_PACKET,
                libc::PACKET_ADD_MEMBERSHIP,
                &mr as *const _ as *const libc::c_void,
                size_of::<libc::packet_mreq>() as u32,
            );
        }
        Ok(fd)
    }
}

fn capture_loop(fd: i32, stop: &AtomicBool, local_nets: &[Cidr], stats: &SharedStats) {
    let mut buf = [0u8; 256];
    let mut batch: HashMap<FlowKey, [u64; 2]> = HashMap::new();
    let mut pkts = 0u64;
    let mut last_flush = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let mut sll: libc::sockaddr_ll = unsafe { zeroed() };
        let mut alen = size_of::<libc::sockaddr_ll>() as libc::socklen_t;
        let n = unsafe {
            libc::recvfrom(
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                libc::MSG_TRUNC,
                &mut sll as *mut _ as *mut libc::sockaddr,
                &mut alen,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EINTR) => {}
                _ => {
                    stats.lock().unwrap().error = Some(format!("capture: {e}"));
                    break;
                }
            }
        } else {
            let copied = (n as usize).min(buf.len());
            if let Some(p) = parse(&buf[..copied], u16::from_be(sll.sll_protocol)) {
                pkts += 1;
                let sent = match sll.sll_pkttype {
                    PACKET_OUTGOING => true,
                    PACKET_OTHERHOST => {
                        if local_nets.iter().any(|c| c.contains(&p.src)) {
                            true
                        } else if local_nets.iter().any(|c| c.contains(&p.dst)) {
                            false
                        } else {
                            p.src < p.dst
                        }
                    }
                    _ => false,
                };
                let (key, idx) = if sent {
                    (FlowKey { proto: p.proto, local: p.src, lport: p.sport, remote: p.dst, rport: p.dport }, 0)
                } else {
                    (FlowKey { proto: p.proto, local: p.dst, lport: p.dport, remote: p.src, rport: p.sport }, 1)
                };
                batch.entry(key).or_insert([0, 0])[idx] += p.len as u64;
            }
        }
        if last_flush.elapsed() >= Duration::from_millis(100) {
            if !batch.is_empty() || pkts > 0 {
                stats.lock().unwrap().ingest(&batch, pkts);
                batch.clear();
                pkts = 0;
            }
            last_flush = Instant::now();
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Pkt {
    pub src: IpAddr,
    pub dst: IpAddr,
    pub proto: u8,
    pub sport: u16,
    pub dport: u16,
    pub len: u32,
}

fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}

fn has_ports(proto: u8) -> bool {
    matches!(proto, 6 | 17 | 33 | 132 | 136)
}

/// Parse an IPv4/IPv6 packet (network layer first).
pub fn parse(b: &[u8], ethertype: u16) -> Option<Pkt> {
    let ver = b.first()? >> 4;
    let is4 = match ethertype {
        ETH_P_IP => true,
        ETH_P_IPV6 => false,
        0 => ver == 4,
        _ => return None,
    };
    match (is4, ver) {
        (true, 4) => {
            let ihl = ((b[0] & 0x0f) as usize) * 4;
            if ihl < 20 || b.len() < 20 {
                return None;
            }
            let len = be16(b, 2)? as u32;
            let frag_off = be16(b, 6)? & 0x1fff;
            let proto = b[9];
            let src = IpAddr::V4(Ipv4Addr::new(b[12], b[13], b[14], b[15]));
            let dst = IpAddr::V4(Ipv4Addr::new(b[16], b[17], b[18], b[19]));
            let (sport, dport) = if frag_off == 0 && has_ports(proto) {
                (be16(b, ihl).unwrap_or(0), be16(b, ihl + 2).unwrap_or(0))
            } else {
                (0, 0)
            };
            Some(Pkt { src, dst, proto, sport, dport, len })
        }
        (false, 6) => {
            if b.len() < 40 {
                return None;
            }
            let len = be16(b, 4)? as u32 + 40;
            let mut next = b[6];
            let s: [u8; 16] = b[8..24].try_into().ok()?;
            let d: [u8; 16] = b[24..40].try_into().ok()?;
            let mut off = 40usize;
            let mut fragmented = false;
            // Walk extension headers.
            for _ in 0..8 {
                match next {
                    0 | 43 | 60 => {
                        let n = *b.get(off)?;
                        let l = (*b.get(off + 1)? as usize + 1) * 8;
                        next = n;
                        off += l;
                    }
                    44 => {
                        let n = *b.get(off)?;
                        if be16(b, off + 2)? & 0xfff8 != 0 {
                            fragmented = true;
                        }
                        next = n;
                        off += 8;
                    }
                    51 => {
                        let n = *b.get(off)?;
                        let l = (*b.get(off + 1)? as usize + 2) * 4;
                        next = n;
                        off += l;
                    }
                    _ => break,
                }
            }
            let (sport, dport) = if !fragmented && has_ports(next) {
                (be16(b, off).unwrap_or(0), be16(b, off + 2).unwrap_or(0))
            } else {
                (0, 0)
            };
            Some(Pkt {
                src: IpAddr::V6(Ipv6Addr::from(s)),
                dst: IpAddr::V6(Ipv6Addr::from(d)),
                proto: next,
                sport,
                dport,
                len,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_tcp() {
        let mut p = vec![0u8; 40];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&1500u16.to_be_bytes());
        p[9] = 6;
        p[12..16].copy_from_slice(&[10, 0, 0, 1]);
        p[16..20].copy_from_slice(&[8, 8, 8, 8]);
        p[20..22].copy_from_slice(&443u16.to_be_bytes());
        p[22..24].copy_from_slice(&50000u16.to_be_bytes());
        let r = parse(&p, ETH_P_IP).unwrap();
        assert_eq!(r.len, 1500);
        assert_eq!(r.sport, 443);
        assert_eq!(r.dport, 50000);
        assert!(parse(&p, 0x0806).is_none());
    }

    #[test]
    fn ipv6_udp() {
        let mut p = vec![0u8; 48];
        p[0] = 0x60;
        p[4..6].copy_from_slice(&8u16.to_be_bytes());
        p[6] = 17;
        p[23] = 1;
        p[39] = 2;
        p[40..42].copy_from_slice(&53u16.to_be_bytes());
        p[42..44].copy_from_slice(&1234u16.to_be_bytes());
        let r = parse(&p, ETH_P_IPV6).unwrap();
        assert_eq!(r.len, 48);
        assert_eq!(r.proto, 17);
        assert_eq!((r.sport, r.dport), (53, 1234));
    }
}
