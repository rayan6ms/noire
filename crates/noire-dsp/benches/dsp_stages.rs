//! Criterion measurements for each bounded DSP stage.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use noire_dsp::{
    ChannelMap, ChannelPosition, ChannelSelection, DcBlocker, DryDelay, EqualPowerMixer,
    FrameAssembler, MODEL_FRAME_SAMPLES, Meter, MixReport, sanitize_buffer,
};

fn dsp_stages(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("dsp_stages");
    group.throughput(Throughput::Elements(MODEL_FRAME_SAMPLES as u64));

    let source = signal_frame();
    let mut sanitized = source;
    group.bench_function(
        BenchmarkId::new("sanitize", MODEL_FRAME_SAMPLES),
        |bencher| {
            bencher.iter(|| {
                sanitized.copy_from_slice(black_box(&source));
                black_box(sanitize_buffer(&mut sanitized));
            });
        },
    );

    let Ok(map) = ChannelMap::new(
        &[ChannelPosition::FrontLeft, ChannelPosition::FrontRight],
        ChannelSelection::MixAll,
    ) else {
        eprintln!("benchmark channel-map construction failed");
        std::process::exit(1);
    };
    let mut stereo = [0.0; MODEL_FRAME_SAMPLES * 2];
    for (frame, sample) in stereo.chunks_exact_mut(2).zip(source) {
        frame.fill(sample);
    }
    let mut mono = [0.0; MODEL_FRAME_SAMPLES];
    group.bench_function(
        BenchmarkId::new("downmix", MODEL_FRAME_SAMPLES),
        |bencher| {
            bencher.iter(|| black_box(map.process(black_box(&stereo), black_box(&mut mono))));
        },
    );

    let mut dc = DcBlocker::new();
    let mut dc_frame = source;
    group.bench_function(
        BenchmarkId::new("dc_blocker", MODEL_FRAME_SAMPLES),
        |bencher| {
            bencher.iter(|| black_box(dc.process(black_box(&mut dc_frame))));
        },
    );

    let mut assembler = FrameAssembler::new();
    group.bench_function(
        BenchmarkId::new("frame_assembler", MODEL_FRAME_SAMPLES),
        |bencher| {
            bencher.iter(|| {
                black_box(assembler.push(black_box(&source), |frame| {
                    black_box(frame);
                }))
            });
        },
    );

    let Ok(mut delay) = DryDelay::new(MODEL_FRAME_SAMPLES) else {
        eprintln!("benchmark dry-delay construction failed");
        std::process::exit(1);
    };
    let mut delayed = [0.0; MODEL_FRAME_SAMPLES];
    group.bench_function(
        BenchmarkId::new("dry_delay", MODEL_FRAME_SAMPLES),
        |bencher| {
            bencher.iter(|| black_box(delay.process(black_box(&source), black_box(&mut delayed))));
        },
    );

    let mut mixed = [0.0; MODEL_FRAME_SAMPLES];
    group.bench_function(
        BenchmarkId::new("wet_dry_mix", MODEL_FRAME_SAMPLES),
        |bencher| {
            bencher.iter(|| {
                let mut report = MixReport::default();
                for ((dry, wet), output) in source.iter().zip(delayed.iter()).zip(mixed.iter_mut())
                {
                    *output = EqualPowerMixer::mix(*dry, *wet, 0.75, &mut report);
                }
                black_box(report);
            });
        },
    );

    let mut meter = Meter::new();
    group.bench_function(BenchmarkId::new("meter", MODEL_FRAME_SAMPLES), |bencher| {
        bencher.iter(|| {
            meter.observe(black_box(&source));
            black_box(meter.take_snapshot())
        });
    });
    group.finish();
}

#[allow(clippy::cast_precision_loss)]
fn signal_frame() -> [f32; MODEL_FRAME_SAMPLES] {
    let mut frame = [0.0; MODEL_FRAME_SAMPLES];
    for (index, sample) in frame.iter_mut().enumerate() {
        let phase = 2.0 * core::f32::consts::PI * 731.0 * index as f32 / 48_000.0;
        *sample = phase.sin() * 0.25;
    }
    frame
}

criterion_group!(benches, dsp_stages);
criterion_main!(benches);
