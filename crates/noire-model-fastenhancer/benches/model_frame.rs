//! Criterion and reference-host percentile measurements for `FastEnhancer-B`.

use std::{
    fmt,
    hint::black_box,
    process,
    time::{Duration, Instant},
};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use noire_model::DenoiserFactory;
use noire_model_fastenhancer::{FASTENHANCER_FRAME_SAMPLES, FastEnhancerFactory};

const P99_LIMIT: Duration = Duration::from_millis(4);
const TIMING_SAMPLES: usize = 5_000;

fn model_frame(criterion: &mut Criterion) {
    let factory = or_exit(FastEnhancerFactory::new(), "benchmark factory creation");
    let input = signal_frame();
    let p99 = measure_p99(&factory, &input);
    eprintln!("Noire FastEnhancer-B model p99 over {TIMING_SAMPLES} frames: {p99:?}");
    assert!(
        p99 <= P99_LIMIT,
        "FastEnhancer-B model p99 {p99:?} exceeds {P99_LIMIT:?}"
    );

    let mut model = or_exit(factory.create(), "benchmark model creation");
    let mut output = [0.0; FASTENHANCER_FRAME_SAMPLES];
    let mut group = criterion.benchmark_group("fastenhancer_b_48khz");
    group.throughput(Throughput::Elements(FASTENHANCER_FRAME_SAMPLES as u64));
    group.bench_function("full_model_frame", |bencher| {
        bencher.iter(|| black_box(model.process_frame(black_box(&input), black_box(&mut output))));
    });
    group.finish();
}

fn measure_p99(
    factory: &FastEnhancerFactory,
    input: &[f32; FASTENHANCER_FRAME_SAMPLES],
) -> Duration {
    let mut model = or_exit(factory.create(), "timing model creation");
    let mut output = [0.0; FASTENHANCER_FRAME_SAMPLES];
    for _ in 0..100 {
        or_exit(model.process_frame(input, &mut output), "timing warmup");
    }

    let mut samples = Vec::with_capacity(TIMING_SAMPLES);
    for _ in 0..TIMING_SAMPLES {
        let started = Instant::now();
        or_exit(model.process_frame(input, &mut output), "timed inference");
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    samples[(TIMING_SAMPLES * 99).div_ceil(100) - 1]
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
fn signal_frame() -> [f32; FASTENHANCER_FRAME_SAMPLES] {
    let mut frame = [0.0; FASTENHANCER_FRAME_SAMPLES];
    for (index, sample) in frame.iter_mut().enumerate() {
        let phase = 2.0 * core::f32::consts::PI * 440.0 * index as f32 / 48_000.0;
        *sample = phase.sin() * 0.25;
    }
    frame
}

criterion_group!(benches, model_frame);
criterion_main!(benches);
