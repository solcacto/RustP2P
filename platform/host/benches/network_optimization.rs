//! Criterion benchmarks for the optimized network layer: packet batching and
//! delta pose compression.

use criterion::{criterion_group, criterion_main, Criterion};
use host::avatar_state::AvatarState;
use host::net;

fn bench_packet_batching(c: &mut Criterion) {
    // Ten small pose payloads, batched into one datagram.
    let messages: Vec<Vec<u8>> = (0..10).map(|i| (i as u32).to_le_bytes().to_vec()).collect();
    c.bench_function("batch_10_messages", |b| {
        b.iter(|| {
            let packet = net::build_batch(&messages, 1);
            let (_, msgs) = net::split_batch(&packet).unwrap();
            criterion::black_box(msgs.len());
        })
    });
}

fn bench_delta_compression(c: &mut Criterion) {
    let prev = AvatarState { x: 0.0, y: 0.0, z: 0.0, rot_y: 0.0 };
    let cur = AvatarState { x: 1.23, y: 0.0, z: -0.5, rot_y: 0.31 };

    let full = net::encode_pose(None, &cur);
    let delta = net::encode_pose(Some(&prev), &cur);
    assert_eq!(full.len(), net::POSE_FULL_LEN);
    assert_eq!(delta.len(), net::POSE_DELTA_LEN);
    let ratio = 100.0 - delta.len() as f64 / full.len() as f64 * 100.0;

    c.bench_function("compress_avatar_pose", |b| {
        b.iter(|| {
            let payload = net::encode_pose(Some(&prev), &cur);
            criterion::black_box(payload.len());
        })
    });
    println!(
        "delta pose: {} bytes vs full {} bytes ({:.0}% smaller)",
        delta.len(),
        full.len(),
        ratio
    );
}

criterion_group!(benches, bench_packet_batching, bench_delta_compression);
criterion_main!(benches);
