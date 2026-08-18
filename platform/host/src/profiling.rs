//! Profiling infrastructure: per-phase frame timing, a shared stats collector
//! for the renderer, and helpers for the performance report.
//!
//! The game loop is instrumented at every layer — input polling, network
//! polling, Wasm execution, and the rest of the frame (Bevy rendering) — so
//! `play_game` can log the breakdown per frame and `perf_report` can summarize
//! it over a run. Network RTT, packet/byte counters, and memory are collected
//! alongside.

use bevy::prelude::Resource;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Timing of a single frame's phases (milliseconds).
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameTiming {
    /// Total frame time.
    pub frame_ms: f64,
    /// Input polling.
    pub input_ms: f64,
    /// Network polling.
    pub network_ms: f64,
    /// Wasm guest tick.
    pub wasm_ms: f64,
}

/// Rolling collector of per-frame timings plus network RTT samples.
#[derive(Debug, Default)]
pub struct FrameStats {
    prev_frame_end: Option<Instant>,
    current: FrameTiming,
    samples: VecDeque<FrameTiming>,
    /// Total frames recorded.
    pub frames: u64,
    /// Round-trip-time samples (ms) collected over the run.
    pub rtt_samples: Vec<f64>,
}

impl FrameStats {
    /// Starts a new frame's timing window (called by the first system).
    ///
    /// The frame time is measured from the end of the *previous* frame's
    /// Update chain, so it includes rendering — the real per-frame cost.
    pub fn begin_frame(&mut self) {
        let now = Instant::now();
        if let Some(prev_end) = self.prev_frame_end {
            self.current.frame_ms = now.duration_since(prev_end).as_secs_f64() * 1000.0;
        }
        self.current.input_ms = 0.0;
        self.current.network_ms = 0.0;
        self.current.wasm_ms = 0.0;
    }

    /// Records the input-polling phase time (ms).
    pub fn add_input(&mut self, ms: f64) {
        self.current.input_ms = ms;
    }

    /// Records the network-polling phase time (ms).
    pub fn add_network(&mut self, ms: f64) {
        self.current.network_ms = ms;
    }

    /// Records the Wasm-execution phase time (ms).
    pub fn add_wasm(&mut self, ms: f64) {
        self.current.wasm_ms = ms;
    }

    /// Closes the frame (called by the last system) and pushes the sample.
    pub fn end_frame(&mut self) {
        self.prev_frame_end = Some(Instant::now());
        self.samples.push_back(self.current);
        if self.samples.len() > 5000 {
            self.samples.pop_front();
        }
        self.frames += 1;
    }

    /// Records an RTT sample (ms).
    pub fn record_rtt(&mut self, ms: f64) {
        self.rtt_samples.push(ms);
    }

    /// The recorded frame samples.
    pub fn samples(&self) -> impl Iterator<Item = &FrameTiming> {
        self.samples.iter()
    }

    /// Averages and percentiles for the current set of samples.
    pub fn summary(&self) -> TimingSummary {
        let samples: Vec<&FrameTiming> = self.samples.iter().collect();
        let avg = |f: fn(&FrameTiming) -> f64| -> f64 {
            if samples.is_empty() {
                return 0.0;
            }
            samples.iter().map(|s| f(s)).sum::<f64>() / samples.len() as f64
        };
        let pct = |f: fn(&FrameTiming) -> f64, p: f64| -> f64 {
            if samples.is_empty() {
                return 0.0;
            }
            let mut v: Vec<f64> = samples.iter().map(|s| f(s)).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let idx = ((v.len() - 1) as f64 * p).round() as usize;
            v[idx.min(v.len() - 1)]
        };
        let avg_frame = avg(|s| s.frame_ms);
        TimingSummary {
            frames: self.frames,
            avg_frame_ms: avg_frame,
            p50_frame_ms: pct(|s| s.frame_ms, 0.50),
            p95_frame_ms: pct(|s| s.frame_ms, 0.95),
            p99_frame_ms: pct(|s| s.frame_ms, 0.99),
            avg_input_ms: avg(|s| s.input_ms),
            avg_network_ms: avg(|s| s.network_ms),
            avg_wasm_ms: avg(|s| s.wasm_ms),
            avg_bevy_ms: (avg_frame - avg(|s| s.input_ms) - avg(|s| s.network_ms) - avg(|s| s.wasm_ms)).max(0.0),
            avg_rtt_ms: if self.rtt_samples.is_empty() {
                None
            } else {
                Some(self.rtt_samples.iter().sum::<f64>() / self.rtt_samples.len() as f64)
            },
        }
    }
}

/// Computed averages/percentiles for a run.
#[derive(Debug, Clone)]
pub struct TimingSummary {
    pub frames: u64,
    pub avg_frame_ms: f64,
    pub p50_frame_ms: f64,
    pub p95_frame_ms: f64,
    pub p99_frame_ms: f64,
    pub avg_input_ms: f64,
    pub avg_network_ms: f64,
    pub avg_wasm_ms: f64,
    pub avg_bevy_ms: f64,
    pub avg_rtt_ms: Option<f64>,
}

/// Bevy resource wrapping the shared stats collector so renderer systems and
/// the performance report observe the same data.
#[derive(Resource, Clone)]
pub struct SharedFrameStats(pub Arc<Mutex<FrameStats>>);

impl Default for SharedFrameStats {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(FrameStats::default())))
    }
}

/// Initializes the tracing subscriber honoring `RUST_LOG` (falls back to
/// `info`). Safe to call even if a subscriber is already installed.
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
