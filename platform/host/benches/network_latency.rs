//! Criterion benchmarks for the P2P networking layer: UDP round-trip latency
//! and socket throughput between two loopback `PeerConnection`s.

use criterion::{criterion_group, criterion_main, Criterion};
use host::peer_connection::PeerConnection;

fn bench_udp_round_trip(c: &mut Criterion) {
    let peer = PeerConnection::udp_only("peer", "127.0.0.1:0").unwrap();
    let target = PeerConnection::udp_only("target", "127.0.0.1:0").unwrap();
    let target_addr = target.local_addr().unwrap();
    let peer_addr = peer.local_addr().unwrap();
    let mut buf = [0u8; 1024];

    c.bench_function("udp_round_trip", |b| {
        b.iter(|| {
            peer.send_udp(target_addr, b"ping").unwrap();
            let (_, _) = target.recv_udp(&mut buf).unwrap();
            target.send_udp(peer_addr, b"pong").unwrap();
            let (_, _) = peer.recv_udp(&mut buf).unwrap();
        })
    });
}

fn bench_udp_throughput(c: &mut Criterion) {
    let peer = PeerConnection::udp_only("peer", "127.0.0.1:0").unwrap();
    let target = PeerConnection::udp_only("target", "127.0.0.1:0").unwrap();
    let target_addr = target.local_addr().unwrap();
    let payload = vec![0x42u8; 128];
    let mut buf = [0u8; 4096];

    c.bench_function("udp_send_recv_128b", |b| {
        b.iter(|| {
            peer.send_udp(target_addr, &payload).unwrap();
            let (_, _) = target.recv_udp(&mut buf).unwrap();
        })
    });
}

criterion_group!(benches, bench_udp_round_trip, bench_udp_throughput);
criterion_main!(benches);
