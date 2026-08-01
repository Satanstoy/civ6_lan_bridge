use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use civ6_lan_protocol::PeerId;
use civ6_lan_router::RouterStats;

const MAX_SEEN_PACKETS: usize = 65_536;
const MAX_SEQUENCE_TRACKERS: usize = 65_536;
const SEEN_PACKET_TTL: Duration = Duration::from_secs(60);
const SEEN_PACKET_CLEANUP_INTERVAL: Duration = Duration::from_secs(1);

struct SeenPacketCache {
    entries: HashMap<u64, Instant>,
    last_cleanup: Instant,
}

#[derive(Default)]
struct SequenceTracker {
    last_seen: HashMap<PeerId, u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceDisposition {
    Legacy,
    New,
    Duplicate,
    Reordered,
}

impl Default for SeenPacketCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            last_cleanup: Instant::now(),
        }
    }
}

#[derive(Clone, Default)]
pub struct RelayMetrics {
    received_packets: Arc<AtomicU64>,
    sent_packets: Arc<AtomicU64>,
    dropped_packets: Arc<AtomicU64>,
    duplicated_packets: Arc<AtomicU64>,
    authentication_failures: Arc<AtomicU64>,
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
    seen_packets: Arc<Mutex<SeenPacketCache>>,
    sequence_tracker: Arc<Mutex<SequenceTracker>>,
    reordered_packets: Arc<AtomicU64>,
    sequence_duplicates: Arc<AtomicU64>,
    rate_limited_packets: Arc<AtomicU64>,
    oversized_packets: Arc<AtomicU64>,
    probe_successes: Arc<AtomicU64>,
    probe_failures: Arc<AtomicU64>,
    last_probe_at_ms: Arc<AtomicU64>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct RelayMetricsSnapshot {
    pub sent_packets: u64,
    pub received_packets: u64,
    pub dropped_packets: u64,
    pub duplicated_packets: u64,
    pub reordered_packets: u64,
    pub sequence_duplicates: u64,
    pub rate_limited_packets: u64,
    pub oversized_packets: u64,
    pub probe_successes: u64,
    pub probe_failures: u64,
    pub last_probe_at_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_peers: usize,
    pub suspended_peers: usize,
    pub active_rooms: usize,
    pub active_hosts: usize,
    pub active_gameplay_sessions: usize,
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
                if now.duration_since(seen.last_cleanup) >= SEEN_PACKET_CLEANUP_INTERVAL {
                    seen.entries
                        .retain(|_, timestamp| now.duration_since(*timestamp) <= SEEN_PACKET_TTL);
                    seen.last_cleanup = now;
                }
                let duplicate = seen.entries.contains_key(&fingerprint);
                if !duplicate && seen.entries.len() >= MAX_SEEN_PACKETS {
                    seen.entries.clear();
                }
                seen.entries.insert(fingerprint, now);
                duplicate
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

    pub fn record_sequence(&self, peer_id: PeerId, sequence: u64) -> SequenceDisposition {
        if sequence == 0 {
            return SequenceDisposition::Legacy;
        }
        let Ok(mut tracker) = self.sequence_tracker.lock() else {
            return SequenceDisposition::New;
        };
        let Some(previous) = tracker.last_seen.get_mut(&peer_id) else {
            if tracker.last_seen.len() >= MAX_SEQUENCE_TRACKERS {
                tracker.last_seen.clear();
            }
            tracker.last_seen.insert(peer_id, sequence);
            return SequenceDisposition::New;
        };
        if sequence == *previous {
            self.sequence_duplicates.fetch_add(1, Ordering::Relaxed);
            SequenceDisposition::Duplicate
        } else if sequence < *previous {
            self.reordered_packets.fetch_add(1, Ordering::Relaxed);
            SequenceDisposition::Reordered
        } else {
            *previous = sequence;
            SequenceDisposition::New
        }
    }

    pub fn record_rate_limited(&self) {
        self.rate_limited_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_oversized(&self) {
        self.oversized_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_probe(&self, success: bool) {
        if success {
            self.probe_successes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.probe_failures.fetch_add(1, Ordering::Relaxed);
        }
        self.last_probe_at_ms
            .store(unix_time_ms(), Ordering::Relaxed);
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
            reordered_packets: self.reordered_packets.load(Ordering::Relaxed),
            sequence_duplicates: self.sequence_duplicates.load(Ordering::Relaxed),
            rate_limited_packets: self.rate_limited_packets.load(Ordering::Relaxed),
            oversized_packets: self.oversized_packets.load(Ordering::Relaxed),
            probe_successes: self.probe_successes.load(Ordering::Relaxed),
            probe_failures: self.probe_failures.load(Ordering::Relaxed),
            last_probe_at_ms: self.last_probe_at_ms.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            active_peers: router.active_peers,
            suspended_peers: router.suspended_peers,
            active_rooms: router.active_rooms,
            active_hosts: router.active_hosts,
            active_gameplay_sessions: router.active_gameplay_sessions,
            authentication_failures: self.authentication_failures.load(Ordering::Relaxed),
        }
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{RelayMetrics, SequenceTracker, MAX_SEEN_PACKETS};
    use civ6_lan_protocol::PeerId;
    use std::collections::HashMap;

    #[test]
    fn duplicate_fingerprint_cache_has_a_hard_bound() {
        let metrics = RelayMetrics::default();
        for packet_id in 0..(MAX_SEEN_PACKETS + 1_024) {
            metrics.record_received(&packet_id.to_le_bytes());
        }

        let seen = metrics.seen_packets.lock().unwrap();
        assert!(seen.entries.len() <= MAX_SEEN_PACKETS);
    }

    #[test]
    fn sequence_tracker_distinguishes_duplicate_and_reordered_packets() {
        let metrics = RelayMetrics {
            sequence_tracker: std::sync::Arc::new(std::sync::Mutex::new(SequenceTracker {
                last_seen: HashMap::new(),
            })),
            ..RelayMetrics::default()
        };
        let peer = PeerId::new();
        metrics.record_sequence(peer, 2);
        metrics.record_sequence(peer, 2);
        metrics.record_sequence(peer, 1);
        let snapshot = metrics.snapshot(Default::default());
        assert_eq!(snapshot.sequence_duplicates, 1);
        assert_eq!(snapshot.reordered_packets, 1);
    }
}
