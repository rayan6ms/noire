//! Release timing gate for the opt-in multi-frame enhancement prototype.

use std::hint::black_box;
use std::time::{Duration, Instant};
use std::{fmt, process};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use noire_model::DenoiserFactory;
use noire_model_rnnoise::{EnhancedRnnoiseConfig, EnhancedRnnoiseFactory, RNNOISE_FRAME_SAMPLES};

const P99_LIMIT: Duration = Duration::from_micros(500);
const TIMING_SAMPLES: usize = 20_000;

fn enhanced_frame(criterion: &mut Criterion) {
    let factory = or_exit(
        EnhancedRnnoiseFactory::new(EnhancedRnnoiseConfig::default()),
        "enhanced factory creation",
    );
    let input = signal_frame();
    let mut samples = measure(&factory, &input);
    samples.sort_unstable();
    let percentile = |numerator: usize| samples[(TIMING_SAMPLES * numerator).div_ceil(100) - 1];
    let p50 = percentile(50);
    let p95 = percentile(95);
    let p99 = percentile(99);
    eprintln!(
        "Noire enhanced frame over {TIMING_SAMPLES} frames: p50={p50:?} p95={p95:?} p99={p99:?}"
    );
    assert!(
        p99 <= P99_LIMIT,
        "enhanced p99 {p99:?} exceeds {P99_LIMIT:?}"
    );

    let mut model = or_exit(factory.create(), "enhanced model creation");
    let mut output = [0.0; RNNOISE_FRAME_SAMPLES];
    let mut group = criterion.benchmark_group("enhanced_rnnoise");
    group.throughput(Throughput::Elements(RNNOISE_FRAME_SAMPLES as u64));
    group.bench_function("full_multiframe_model_frame", |bencher| {
        bencher.iter(|| black_box(model.process_frame(black_box(&input), black_box(&mut output))));
    });
    group.finish();
}

fn measure(
    factory: &EnhancedRnnoiseFactory,
    input: &[f32; RNNOISE_FRAME_SAMPLES],
) -> Vec<Duration> {
    let mut model = or_exit(factory.create(), "timing model creation");
    let mut output = [0.0; RNNOISE_FRAME_SAMPLES];
    for _ in 0..100 {
        or_exit(model.process_frame(input, &mut output), "timing warmup");
    }
    let mut samples = Vec::with_capacity(TIMING_SAMPLES);
    for _ in 0..TIMING_SAMPLES {
        let started = Instant::now();
        or_exit(model.process_frame(input, &mut output), "timed inference");
        samples.push(started.elapsed());
    }
    samples
}

fn or_exit<T, E: fmt::Display>(result: Result<T, E>, operation: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{operation} failed: {error}");
            process::exit(1);
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn signal_frame() -> [f32; RNNOISE_FRAME_SAMPLES] {
    let mut frame = [0.0; RNNOISE_FRAME_SAMPLES];
    for (index, sample) in frame.iter_mut().enumerate() {
        let phase = 2.0 * core::f32::consts::PI * 440.0 * index as f32 / 48_000.0;
        *sample = phase.sin() * 0.25;
    }
    frame
}

criterion_group!(benches, enhanced_frame);
criterion_main!(benches);
