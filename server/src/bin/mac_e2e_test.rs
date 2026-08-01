//! Repeatable, authenticated transport-level test runner for the macOS client.
//!
//! This runner talks to the normal control API and the normal UDP relay. It
//! deliberately sends shared `RelayMessage` envelopes; it does not introduce
//! a test-only wire protocol and it never treats a synthetic packet as proof
//! that Civ VI displayed a room.

use std::{
    env, fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use civ6_lan_client_core::{
    ClientConfig, ControlClient, RelayClient, RelayMetricsResponse, TestSessionManifest,
};
use civ6_lan_protocol::{
    relay::{RelayMessage, MAX_RELAY_DATAGRAM_SIZE, RELAY_PROTOCOL_VERSION},
    Civ6UdpPort, DiscoveryRequestId, HostSessionId, PeerId, RoomCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{net::UdpSocket, time::timeout};
use uuid::Uuid;

const TEST_TOKEN_ENV: &str = "CIV6_CONTROL_BEARER_TOKEN";
const CONTROL_URL_ENV: &str = "CIV6_TEST_CONTROL_URL";
const RELAY_ADDR_ENV: &str = "CIV6_TEST_RELAY_ADDR";
const MANIFEST_ENV: &str = "CIV6_TEST_MANIFEST";
const REPORT_ENV: &str = "CIV6_TEST_REPORT";
const SERVER_LOG_ENV: &str = "CIV6_TEST_SERVER_LOG";
const DEFAULT_CONTROL_URL: &str = "http://127.0.0.1:18080";
const DEFAULT_RELAY_ADDR: &str = "127.0.0.1:32000";
const TEST_ROOM_ONE: &str = "MACA42";
const TEST_ROOM_TWO: &str = "MACB42";

#[derive(Debug, Serialize)]
struct TestReport {
    status: String,
    session_id: String,
    server_commit: String,
    relay_endpoint: String,
    handshake: String,
    udp_echo: String,
    room_fanout: String,
    room_isolation: String,
    packet_metrics: PacketMetrics,
    mac_client_id: Option<String>,
    civ6_discovery: String,
    evidence_files: Vec<String>,
    checks: Value,
    errors: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
struct PacketMetrics {
    sent_packets: u64,
    received_packets: u64,
    dropped_packets: u64,
    duplicated_packets: u64,
    reordered_packets: u64,
    bytes_in: u64,
    bytes_out: u64,
    echo_rtt_ms: Option<f64>,
    fanout_rtt_ms: Option<f64>,
    active_peers: usize,
    active_rooms: usize,
    active_hosts: usize,
    authentication_failures: u64,
}

#[derive(Debug, Deserialize)]
struct MacClientResult {
    session_id: Option<String>,
    client_id: Option<String>,
    civ6_discovery: Option<String>,
    evidence_files: Option<Vec<String>>,
}

#[derive(Debug)]
struct PeerEndpoint {
    peer_id: PeerId,
    virtual_ip: Ipv4Addr,
}

struct TestContext {
    client: ControlClient,
    room_one: RoomCode,
    room_two: RoomCode,
    peers_one: Vec<PeerEndpoint>,
    peers_two: Vec<PeerEndpoint>,
    hosts_one: Vec<HostSessionId>,
    hosts_two: Vec<HostSessionId>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report_path = env::var(REPORT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("server-test-report.json"));
    let result = run_suite().await;
    match result {
        Ok(report) => {
            write_report(&report_path, &report)?;
            println!("server-test-report: {}", report_path.display());
            if report.status != "fail" {
                Ok(())
            } else {
                Err(format!("mac e2e test finished with status {}", report.status).into())
            }
        }
        Err(error) => {
            let report = TestReport {
                status: "fail".to_owned(),
                session_id: String::new(),
                server_commit: server_commit(),
                relay_endpoint: env::var(RELAY_ADDR_ENV)
                    .unwrap_or_else(|_| DEFAULT_RELAY_ADDR.to_owned()),
                handshake: "fail".to_owned(),
                udp_echo: "not_run".to_owned(),
                room_fanout: "not_run".to_owned(),
                room_isolation: "not_run".to_owned(),
                packet_metrics: PacketMetrics::default(),
                mac_client_id: None,
                civ6_discovery: "not_tested".to_owned(),
                evidence_files: evidence_files(),
                checks: json!({}),
                errors: vec![error.to_string()],
            };
            write_report(&report_path, &report)?;
            eprintln!("mac e2e setup failed: {error}");
            Err(error)
        }
    }
}

async fn run_suite() -> Result<TestReport, Box<dyn std::error::Error>> {
    let token = env::var(TEST_TOKEN_ENV)
        .map_err(|_| format!("{TEST_TOKEN_ENV} must be set; do not put it in Git"))?;
    let control_url = env::var(CONTROL_URL_ENV).unwrap_or_else(|_| DEFAULT_CONTROL_URL.to_owned());
    let relay_addr: SocketAddr = env::var(RELAY_ADDR_ENV)
        .unwrap_or_else(|_| DEFAULT_RELAY_ADDR.to_owned())
        .parse()?;
    let room_one = RoomCode::parse(
        env::var("CIV6_TEST_ROOM_ONE").unwrap_or_else(|_| TEST_ROOM_ONE.to_owned()),
    )?;
    let room_two = RoomCode::parse(
        env::var("CIV6_TEST_ROOM_TWO").unwrap_or_else(|_| TEST_ROOM_TWO.to_owned()),
    )?;
    let session_id = Uuid::new_v4().to_string();
    let config = ClientConfig {
        control_url: control_url.clone(),
        bearer_token: token.clone(),
        relay_server: relay_addr,
        relay_port: relay_addr.port(),
    };
    let client = ControlClient::new(config);
    let mut errors = Vec::new();
    let mut checks = serde_json::Map::new();

    let health = client.health_live().await;
    let health_ok = health.is_ok();
    checks.insert("control_health".to_owned(), check(health_ok));
    if let Err(error) = health {
        errors.push(format!("control health: {error}"));
    }

    let bad_client = ControlClient::new(ClientConfig {
        control_url: control_url.clone(),
        bearer_token: "wrong-test-token".to_owned(),
        relay_server: relay_addr,
        relay_port: relay_addr.port(),
    });
    let unauthorized = bad_client
        .create_room(Some(RoomCode::parse("MACBAD").unwrap()))
        .await;
    let auth_check_ok = unauthorized.is_err();
    checks.insert("bearer_authentication".to_owned(), check(auth_check_ok));
    if !auth_check_ok {
        errors.push("a request with a wrong bearer token unexpectedly succeeded".to_owned());
    }

    let created_one = client.create_room(Some(room_one.clone())).await?;
    let created_two = client.create_room(Some(room_two.clone())).await?;
    let mut context = TestContext {
        client: client.clone(),
        room_one: room_one.clone(),
        room_two: room_two.clone(),
        peers_one: Vec::new(),
        peers_two: Vec::new(),
        hosts_one: Vec::new(),
        hosts_two: Vec::new(),
    };

    for (room, target) in [
        (&room_one, &mut context.peers_one),
        (&room_two, &mut context.peers_two),
    ] {
        let addresses = if room == &room_one {
            [
                Ipv4Addr::new(127, 0, 0, 2),
                Ipv4Addr::new(127, 0, 0, 3),
                Ipv4Addr::new(127, 0, 0, 4),
            ]
        } else {
            [
                Ipv4Addr::new(127, 0, 0, 5),
                Ipv4Addr::new(127, 0, 0, 6),
                Ipv4Addr::new(127, 0, 0, 7),
            ]
        };
        let count = if room == &room_one { 3 } else { 2 };
        for address in addresses.into_iter().take(count) {
            target.push(PeerEndpoint {
                peer_id: context.client.join_room(room, None, None).await?.peer_id,
                virtual_ip: address,
            });
        }
    }

    let host_b = context
        .client
        .register_host(&context.room_one, context.peers_one[1].peer_id)
        .await?;
    let host_c = context
        .client
        .register_host(&context.room_one, context.peers_one[2].peer_id)
        .await?;
    let host_e = context
        .client
        .register_host(&context.room_two, context.peers_two[1].peer_id)
        .await?;
    context.hosts_one = vec![host_b.host_session_id, host_c.host_session_id];
    context.hosts_two = vec![host_e.host_session_id];
    eprintln!("mac-e2e phase=control_setup complete");

    let expires_at = unix_now().saturating_add(30 * 60);
    let manifest_path = env::var(MANIFEST_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/civ6-lan-bridge-mac-session.json"));
    let manifest = TestSessionManifest {
        session_id: session_id.clone(),
        room_id: created_one.room_id,
        room_code: room_one.clone(),
        client_id: context.peers_one[0].peer_id,
        client_virtual_ip: civ6_lan_protocol::VirtualIp::new(context.peers_one[0].virtual_ip),
        relay_host: env::var("CIV6_TEST_RELAY_HOST")
            .unwrap_or_else(|_| relay_addr.ip().to_string()),
        relay_port: relay_addr.port(),
        control_endpoint: control_url,
        protocol_version: RELAY_PROTOCOL_VERSION,
        token,
        expires_at,
        test_mode: true,
    };
    write_json_file(&manifest_path, &manifest)?;

    let relay_port = relay_addr.port();
    let relay_a = RelayClient::bind(
        SocketAddr::new(IpAddr::V4(context.peers_one[0].virtual_ip), relay_port),
        relay_addr,
    )
    .await?;
    let relay_b = RelayClient::bind(
        SocketAddr::new(IpAddr::V4(context.peers_one[1].virtual_ip), relay_port),
        relay_addr,
    )
    .await?;
    let relay_c = RelayClient::bind(
        SocketAddr::new(IpAddr::V4(context.peers_one[2].virtual_ip), relay_port),
        relay_addr,
    )
    .await?;
    let relay_d = RelayClient::bind(
        SocketAddr::new(IpAddr::V4(context.peers_two[0].virtual_ip), relay_port),
        relay_addr,
    )
    .await?;
    let relay_e = RelayClient::bind(
        SocketAddr::new(IpAddr::V4(context.peers_two[1].virtual_ip), relay_port),
        relay_addr,
    )
    .await?;

    let echo_started = Instant::now();
    eprintln!("mac-e2e phase=relay_probe start");
    let echo = relay_a.probe(Duration::from_secs(3)).await;
    let echo_rtt_ms = echo
        .ok()
        .map(|_| echo_started.elapsed().as_secs_f64() * 1000.0);
    checks.insert(
        "udp_authenticated_echo".to_owned(),
        check(echo_rtt_ms.is_some()),
    );
    if echo_rtt_ms.is_none() {
        errors.push("authenticated UDP relay probe did not receive an ACK".to_owned());
    }

    let request_id = DiscoveryRequestId::new();
    let payload = sequence_payload(1);
    let fanout_started = Instant::now();
    eprintln!("mac-e2e phase=discovery_fanout start");
    relay_a
        .send(&RelayMessage::DiscoveryRequest {
            request_id,
            destination_port: Civ6UdpPort(62_900),
            payload: payload.clone(),
        })
        .await?;
    let to_b = receive_message(&relay_b, Duration::from_secs(3)).await;
    let to_c = receive_message(&relay_c, Duration::from_secs(3)).await;
    let fanout_ok = matches!(
        (&to_b, &to_c),
        (
            Ok(RelayMessage::DiscoveryToHost { request_id: b, source_virtual_ip, .. }),
            Ok(RelayMessage::DiscoveryToHost { request_id: c, source_virtual_ip: source_c, .. })
        ) if b == &request_id && c == &request_id
            && source_virtual_ip == &civ6_lan_protocol::VirtualIp::new(context.peers_one[0].virtual_ip)
            && source_c == &civ6_lan_protocol::VirtualIp::new(context.peers_one[0].virtual_ip)
    );
    let fanout_rtt_ms = fanout_ok.then(|| fanout_started.elapsed().as_secs_f64() * 1000.0);
    let self_repeat_ok = expect_no_message(&relay_a, Duration::from_millis(150)).await;
    checks.insert(
        "same_room_discovery_fanout".to_owned(),
        check(fanout_ok && self_repeat_ok),
    );
    if !fanout_ok {
        errors.push(format!(
            "same-room fan-out did not reach both hosts: B={to_b:?}, C={to_c:?}"
        ));
    }
    if !self_repeat_ok {
        errors.push("discovery request was looped back to its source client".to_owned());
    }

    let response_b = RelayMessage::DiscoveryResponse {
        request_id,
        host_session_id: host_b.host_session_id,
        source_port: Civ6UdpPort(62_900),
        payload: b"host-b".to_vec(),
    };
    let response_c = RelayMessage::DiscoveryResponse {
        request_id,
        host_session_id: host_c.host_session_id,
        source_port: Civ6UdpPort(62_901),
        payload: b"host-c".to_vec(),
    };
    relay_b.send(&response_b).await?;
    relay_c.send(&response_c).await?;
    let response_a_one = receive_message(&relay_a, Duration::from_secs(3)).await;
    let response_a_two = receive_message(&relay_a, Duration::from_secs(3)).await;
    let responses_ok = matches!(
        (&response_a_one, &response_a_two),
        (
            Ok(RelayMessage::DiscoveryToClient { request_id: first, host_virtual_ip: first_ip, .. }),
            Ok(RelayMessage::DiscoveryToClient { request_id: second, host_virtual_ip: second_ip, .. })
        ) if first == &request_id && second == &request_id
            && [*first_ip, *second_ip].contains(&civ6_lan_protocol::VirtualIp::new(context.peers_one[1].virtual_ip))
            && [*first_ip, *second_ip].contains(&civ6_lan_protocol::VirtualIp::new(context.peers_one[2].virtual_ip))
    );
    checks.insert("host_discovery_responses".to_owned(), check(responses_ok));
    if !responses_ok {
        errors.push(format!("host discovery responses were not routed back to A: {response_a_one:?}, {response_a_two:?}"));
    }

    // The production host TTL is intentionally short. Keep the synthetic
    // hosts alive while the remainder of this observable test suite runs;
    // the dedicated TTL check below verifies expiry separately.
    eprintln!("mac-e2e phase=heartbeat_hosts start");
    context
        .client
        .heartbeat_host(
            &context.room_one,
            context.peers_one[1].peer_id,
            host_b.host_session_id,
        )
        .await?;
    context
        .client
        .heartbeat_host(
            &context.room_one,
            context.peers_one[2].peer_id,
            host_c.host_session_id,
        )
        .await?;
    context
        .client
        .heartbeat_host(
            &context.room_two,
            context.peers_two[1].peer_id,
            host_e.host_session_id,
        )
        .await?;

    eprintln!("mac-e2e phase=gameplay start");
    let gameplay = context
        .client
        .create_gameplay_session(
            &context.room_one,
            context.peers_one[0].peer_id,
            host_b.host_session_id,
        )
        .await?;
    relay_a
        .send(&RelayMessage::GameplayPacket {
            session_id: gameplay.gameplay_session_id,
            source_port: Civ6UdpPort(62_056),
            payload: sequence_payload(10),
        })
        .await?;
    let gameplay_at_b = receive_message(&relay_b, Duration::from_secs(3)).await;
    relay_b
        .send(&RelayMessage::GameplayPacket {
            session_id: gameplay.gameplay_session_id,
            source_port: Civ6UdpPort(62_056),
            payload: sequence_payload(11),
        })
        .await?;
    let gameplay_at_a = receive_message(&relay_a, Duration::from_secs(3)).await;
    let gameplay_ok = matches!(gameplay_at_b, Ok(RelayMessage::GameplayToPeer { .. }))
        && matches!(gameplay_at_a, Ok(RelayMessage::GameplayToPeer { .. }))
        && expect_no_message(&relay_c, Duration::from_millis(150)).await;
    checks.insert("bidirectional_gameplay".to_owned(), check(gameplay_ok));
    if !gameplay_ok {
        errors.push(format!("gameplay route did not remain bidirectional and room-scoped: B={gameplay_at_b:?}, A={gameplay_at_a:?}"));
    }

    let isolation_request = DiscoveryRequestId::new();
    relay_a
        .send(&RelayMessage::DiscoveryRequest {
            request_id: isolation_request,
            destination_port: Civ6UdpPort(62_902),
            payload: b"room-one-only".to_vec(),
        })
        .await?;
    let room_two_isolated = expect_no_message(&relay_d, Duration::from_millis(250)).await
        && expect_no_message(&relay_e, Duration::from_millis(250)).await;
    let room_one_still_fanout = receive_message(&relay_b, Duration::from_secs(3))
        .await
        .is_ok()
        && receive_message(&relay_c, Duration::from_secs(3))
            .await
            .is_ok();
    let isolation_ok = room_two_isolated && room_one_still_fanout;
    checks.insert("room_isolation".to_owned(), check(isolation_ok));
    if !isolation_ok {
        errors.push(
            "a discovery datagram crossed room boundaries or was not fan-out within room one"
                .to_owned(),
        );
    }

    let before_spoof = client.relay_metrics().await?;
    relay_a
        .send(&RelayMessage::DiscoveryResponse {
            request_id: isolation_request,
            host_session_id: host_b.host_session_id,
            source_port: Civ6UdpPort(62_900),
            payload: b"spoofed-host".to_vec(),
        })
        .await?;
    let spoof_no_response = expect_no_message(&relay_a, Duration::from_millis(250)).await;
    let after_spoof = client.relay_metrics().await?;
    let spoof_ok = spoof_no_response
        && after_spoof.authentication_failures > before_spoof.authentication_failures;
    checks.insert("source_identity_spoofing".to_owned(), check(spoof_ok));
    if !spoof_ok {
        errors.push("spoofed host identity was not rejected and counted".to_owned());
    }

    let sequence_order_ok = run_sequence_order_test(&relay_a, &relay_b).await?;
    checks.insert(
        "sequence_order_observation".to_owned(),
        check(sequence_order_ok),
    );
    let duplicate_ok = run_duplicate_test(&relay_a, &relay_b).await?;
    checks.insert(
        "duplicate_datagram_observation".to_owned(),
        check(duplicate_ok),
    );
    if !sequence_order_ok {
        errors.push("opaque test sequence arrived out of order".to_owned());
    }
    if !duplicate_ok {
        errors.push(
            "duplicate relay datagram was not observable as one-for-one forwarding".to_owned(),
        );
    }

    let before_oversized = client.relay_metrics().await?;
    let raw = UdpSocket::bind(SocketAddr::new(
        IpAddr::V4(context.peers_one[0].virtual_ip),
        0,
    ))
    .await?;
    raw.send_to(&vec![0u8; MAX_RELAY_DATAGRAM_SIZE + 1], relay_addr)
        .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after_oversized = client.relay_metrics().await?;
    let size_ok = after_oversized.dropped_packets > before_oversized.dropped_packets;
    checks.insert("maximum_datagram_size".to_owned(), check(size_ok));
    if !size_ok {
        errors.push("oversized relay datagram was not counted as dropped".to_owned());
    }

    let ttl_seconds = host_c.expires_in_seconds;
    eprintln!(
        "mac-e2e phase=ttl_expiration wait_seconds={}",
        ttl_seconds + 2
    );
    tokio::time::sleep(Duration::from_secs(ttl_seconds.saturating_add(2))).await;
    let ttl_status = client.room_status(&context.room_one).await?;
    let ttl_ok = ttl_status.host_count == 0;
    checks.insert("host_ttl_expiration".to_owned(), check(ttl_ok));
    if !ttl_ok {
        errors.push(format!(
            "host TTL did not expire: host_count={}",
            ttl_status.host_count
        ));
    }

    let disconnected_relay = RelayClient::bind(
        SocketAddr::new(IpAddr::V4(context.peers_one[0].virtual_ip), 0),
        SocketAddr::new(relay_addr.ip(), relay_addr.port().saturating_add(1)),
    )
    .await?;
    let disconnected = disconnected_relay.probe(Duration::from_millis(250)).await;
    let disconnect_ok = disconnected.is_err();
    checks.insert("relay_disconnect_error".to_owned(), check(disconnect_ok));
    if !disconnect_ok {
        errors.push("a disconnected relay endpoint did not return a clear client error".to_owned());
    }

    let server_metrics = client.relay_metrics().await?;
    let packet_metrics = packet_metrics(
        server_metrics,
        echo_rtt_ms,
        fanout_rtt_ms,
        sequence_order_ok,
    );
    let mac_result = wait_for_mac_result().await?;
    let mut evidence = evidence_files();
    let (mac_client_id, civ6_discovery) = if let Some(result) = mac_result {
        let _mac_session_id = result.session_id;
        if let Some(files) = result.evidence_files {
            evidence.extend(files);
        }
        (
            result.client_id,
            result
                .civ6_discovery
                .unwrap_or_else(|| "not_tested".to_owned()),
        )
    } else {
        (None, "not_tested".to_owned())
    };

    cleanup(&context).await;
    let core_checks_ok = health_ok
        && auth_check_ok
        && echo_rtt_ms.is_some()
        && fanout_ok
        && self_repeat_ok
        && responses_ok
        && gameplay_ok
        && isolation_ok
        && spoof_ok
        && sequence_order_ok
        && duplicate_ok
        && size_ok
        && ttl_ok
        && disconnect_ok;
    let status = if !core_checks_ok {
        "fail"
    } else if civ6_discovery == "pass" {
        "pass"
    } else {
        "partial"
    };
    let _ = created_two;

    Ok(TestReport {
        status: status.to_owned(),
        session_id,
        server_commit: server_commit(),
        relay_endpoint: relay_addr.to_string(),
        handshake: if health_ok && echo_rtt_ms.is_some() {
            "pass"
        } else {
            "fail"
        }
        .to_owned(),
        udp_echo: if echo_rtt_ms.is_some() {
            "pass"
        } else {
            "fail"
        }
        .to_owned(),
        room_fanout: if fanout_ok && responses_ok {
            "pass"
        } else {
            "fail"
        }
        .to_owned(),
        room_isolation: if isolation_ok { "pass" } else { "fail" }.to_owned(),
        packet_metrics,
        mac_client_id,
        civ6_discovery,
        evidence_files: evidence,
        checks: Value::Object(checks),
        errors,
    })
}

fn packet_metrics(
    server: RelayMetricsResponse,
    echo_rtt_ms: Option<f64>,
    fanout_rtt_ms: Option<f64>,
    sequence_order_ok: bool,
) -> PacketMetrics {
    PacketMetrics {
        sent_packets: server.sent_packets,
        received_packets: server.received_packets,
        dropped_packets: server.dropped_packets,
        duplicated_packets: server.duplicated_packets,
        reordered_packets: u64::from(!sequence_order_ok),
        bytes_in: server.bytes_in,
        bytes_out: server.bytes_out,
        echo_rtt_ms,
        fanout_rtt_ms,
        active_peers: server.active_peers,
        active_rooms: server.active_rooms,
        active_hosts: server.active_hosts,
        authentication_failures: server.authentication_failures,
    }
}

async fn run_sequence_order_test(
    source: &RelayClient,
    host: &RelayClient,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut ids = Vec::new();
    for sequence in 1..=3u8 {
        let request_id = DiscoveryRequestId::new();
        ids.push(request_id);
        source
            .send(&RelayMessage::DiscoveryRequest {
                request_id,
                destination_port: Civ6UdpPort(62_903),
                payload: sequence_payload(sequence),
            })
            .await?;
    }
    let mut received = Vec::new();
    for _ in 0..3 {
        if let Ok(RelayMessage::DiscoveryToHost {
            request_id,
            payload,
            ..
        }) = receive_message(host, Duration::from_secs(3)).await
        {
            received.push((request_id, payload));
        }
    }
    Ok(received.iter().map(|(id, _)| *id).eq(ids))
}

async fn run_duplicate_test(
    source: &RelayClient,
    host: &RelayClient,
) -> Result<bool, Box<dyn std::error::Error>> {
    let request_id = DiscoveryRequestId::new();
    let message = RelayMessage::DiscoveryRequest {
        request_id,
        destination_port: Civ6UdpPort(62_904),
        payload: b"same-envelope-twice".to_vec(),
    };
    source.send(&message).await?;
    source.send(&message).await?;
    let first = receive_message(host, Duration::from_secs(3)).await?;
    let second = receive_message(host, Duration::from_secs(3)).await?;
    Ok(first == second)
}

fn sequence_payload(sequence: u8) -> Vec<u8> {
    vec![sequence, 0xc6, 0x6c, 0x62]
}

async fn receive_message(
    client: &RelayClient,
    wait: Duration,
) -> Result<RelayMessage, Box<dyn std::error::Error>> {
    Ok(timeout(wait, client.receive()).await??)
}

async fn expect_no_message(client: &RelayClient, wait: Duration) -> bool {
    timeout(wait, client.receive()).await.is_err()
}

async fn wait_for_mac_result() -> Result<Option<MacClientResult>, Box<dyn std::error::Error>> {
    let Some(path) = env::var_os("CIV6_TEST_MAC_RESULT") else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let wait_seconds = env::var("CIV6_TEST_WAIT_FOR_MAC_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    loop {
        if path.is_file() {
            let content = fs::read_to_string(&path)?;
            return Ok(Some(serde_json::from_str(&content)?));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn cleanup(context: &TestContext) {
    for (room, hosts) in [
        (&context.room_one, &context.hosts_one),
        (&context.room_two, &context.hosts_two),
    ] {
        for host in hosts {
            let _ = context.client.delete_host(room, *host).await;
        }
    }
    for (room, peers) in [
        (&context.room_one, &context.peers_one),
        (&context.room_two, &context.peers_two),
    ] {
        for peer in peers {
            let _ = context.client.delete_peer(room, peer.peer_id).await;
        }
        let _ = context.client.delete_room(room).await;
    }
}

fn check(pass: bool) -> Value {
    json!({ "status": if pass { "pass" } else { "fail" } })
}

fn server_commit() -> String {
    env::var("CIV6_BUILD_COMMIT").unwrap_or_else(|_| "unknown".to_owned())
}

fn evidence_files() -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(path) = env::var("CIV6_TEST_REDACTED_MANIFEST") {
        files.push(path);
    }
    if let Ok(path) = env::var(SERVER_LOG_ENV) {
        files.push(path);
    }
    if let Ok(path) = env::var("CIV6_TEST_MAC_LOG") {
        files.push(path);
    }
    files
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn write_report(path: &Path, report: &TestReport) -> Result<(), Box<dyn std::error::Error>> {
    write_json_file(path, report)
}
