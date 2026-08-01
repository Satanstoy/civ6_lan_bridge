use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use civ6_lan_router::RouterStats;

#[derive(Clone, Default)]
pub struct RelayMetrics {
    received_packets: Arc<AtomicU64>,
    sent_packets: Arc<AtomicU64>,
    dropped_packets: Arc<AtomicU64>,
    duplicated_packets: Arc<AtomicU64>,
    authentication_failures: Arc<AtomicU64>,
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
    seen_packets: Arc<Mutex<HashMap<u64, Instant>>>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct RelayMetricsSnapshot {
    pub sent_packets: u64,
    pub received_packets: u64,
    pub dropped_packets: u64,
    pub duplicated_packets: u64,
    pub reordered_packets: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_peers: usize,
    pub active_rooms: usize,
    pub active_hosts: usize,
    pub authentication_failures: u64,
}

impl RelayMetrics {
    pub fn record_received(&self, packet: &[u8]) {
        self.received_packets.fetch_add(1, Ordering::Relaxed);
        self.bytes_in
            .fetch_add(packet.len() as u64, Ordering::Relaxed);

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        packet.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let now = Instant::now();
        let duplicate = self
            .seen_packets
            .lock()
            .map(|mut seen| {
                seen.retain(|_, timestamp| {
                    now.duration_since(*timestamp) <= Duration::from_secs(60)
                });
                seen.insert(fingerprint, now).is_some()
            })
            .unwrap_or(false);
        if duplicate {
            self.duplicated_packets.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_sent(&self, packet_len: usize) {
        self.sent_packets.fetch_add(1, Ordering::Relaxed);
        self.bytes_out
            .fetch_add(packet_len as u64, Ordering::Relaxed);
    }

    pub fn record_drop(&self, authentication_failure: bool) {
        self.dropped_packets.fetch_add(1, Ordering::Relaxed);
        if authentication_failure {
            self.record_authentication_failure();
        }
    }

    pub fn record_authentication_failure(&self) {
        self.authentication_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self, router: RouterStats) -> RelayMetricsSnapshot {
        RelayMetricsSnapshot {
            sent_packets: self.sent_packets.load(Ordering::Relaxed),
            received_packets: self.received_packets.load(Ordering::Relaxed),
            dropped_packets: self.dropped_packets.load(Ordering::Relaxed),
            duplicated_packets: self.duplicated_packets.load(Ordering::Relaxed),
            // Relay protocol v1 has no sequence number. The e2e runner checks
            // the ordering of opaque test payloads and reports the result.
            reordered_packets: 0,
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            active_peers: router.active_peers,
            active_rooms: router.active_rooms,
            active_hosts: router.active_hosts,
            authentication_failures: self.authentication_failures.load(Ordering::Relaxed),
        }
    }
}
