//! Asynchronous reverse DNS with a small worker pool and a cache.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

enum Entry {
    Pending,
    Done(Option<String>),
}

pub struct Resolver {
    cache: Arc<Mutex<HashMap<IpAddr, Entry>>>,
    tx: Mutex<Sender<IpAddr>>,
}

impl Resolver {
    pub fn new(workers: usize) -> Self {
        let (tx, rx) = channel::<IpAddr>();
        let rx = Arc::new(Mutex::new(rx));
        let cache: Arc<Mutex<HashMap<IpAddr, Entry>>> = Arc::new(Mutex::new(HashMap::new()));
        for i in 0..workers {
            let rx = rx.clone();
            let cache = cache.clone();
            let _ = std::thread::Builder::new()
                .name(format!("dns{i}"))
                .spawn(move || loop {
                    let ip = match rx.lock().unwrap().recv() {
                        Ok(ip) => ip,
                        Err(_) => break,
                    };
                    let name = dns_lookup::lookup_addr(&ip)
                        .ok()
                        .filter(|n| n != &ip.to_string() && !n.is_empty());
                    cache.lock().unwrap().insert(ip, Entry::Done(name));
                });
        }
        Resolver { cache, tx: Mutex::new(tx) }
    }

    /// Returns the cached name, or None (queuing a lookup if not yet asked).
    pub fn lookup(&self, ip: &IpAddr) -> Option<String> {
        let mut c = self.cache.lock().unwrap();
        match c.get(ip) {
            Some(Entry::Done(n)) => n.clone(),
            Some(Entry::Pending) => None,
            None => {
                c.insert(*ip, Entry::Pending);
                let _ = self.tx.lock().unwrap().send(*ip);
                None
            }
        }
    }
}
