//! Phase-5 live `RNNoise` fixture and reference-host performance acceptance.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

use std::{error::Error, fs, time::Instant};

use noire_dsp::{
    ChannelMap, ChannelPosition, ChannelSelection, MODEL_FRAME_SAMPLES, SAMPLE_RATE_HZ,
};
use noire_model::DenoiserFactory;
use noire_model_rnnoise::RnnoiseFactory;
use noire_pipewire::{CaptureSink, InputGeneration, LiveState, create_live_channel};

const PERFORMANCE_MODEL_FRAMES: usize = 60_000;

fn rnnoise() -> Result<Box<dyn noire_model::Denoiser>, Box<dyn Error>> {
    Ok(RnnoiseFactory::new()?.create()?)
}

fn fixture(name: &str, frames: usize) -> Vec<f32> {
    let mut seed = 0x4d59_5df4_d0f3_3173_u64;
    (0..frames)
        .map(|index| {
            let time = index as f32 / SAMPLE_RATE_HZ as f32;
            match name {
                "speech" => {
                    let envelope = (2.0 * core::f32::consts::PI * 3.5 * time).sin().abs();
                    envelope
                        * (0.16 * (2.0 * core::f32::consts::PI * 180.0 * time).sin()
                            + 0.07 * (2.0 * core::f32::consts::PI * 720.0 * time).sin())
                }
                "music" => {
                    0.09 * (2.0 * core::f32::consts::PI * 220.0 * time).sin()
                        + 0.07 * (2.0 * core::f32::consts::PI * 329.63 * time).sin()
                        + 0.05 * (2.0 * core::f32::consts::PI * 440.0 * time).sin()
                }
                "keyboard" => {
                    if index.is_multiple_of(2_113) {
                        0.9
                    } else {
                        0.0
                    }
                }
                "fan" => {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let noise = f32::from((seed >> 40) as u16) / f32::from(u16::MAX) - 0.5;
                    0.08 * noise + 0.04 * (2.0 * core::f32::consts::PI * 120.0 * time).sin()
                }
                "clipping" => {
                    (1.8 * (2.0 * core::f32::consts::PI * 997.0 * time).sin()).clamp(-1.0, 1.0)
                }
                _ => 0.0,
            }
        })
        .collect()
}

#[test]
fn real_model_fixture_matrix_is_finite_bounded_and_metered() -> Result<(), Box<dyn Error>> {
    for name in ["silence", "speech", "music", "keyboard", "fan", "clipping"] {
        let (mut sink, mut output, control, telemetry) = create_live_channel(rnnoise()?)?;
        control.set_strength(1.0);
        let samples = fixture(name, MODEL_FRAME_SAMPLES * 20);
        let mut rendered = Vec::with_capacity(samples.len());
        for chunk in samples.chunks(MODEL_FRAME_SAMPLES) {
            sink.write(InputGeneration::INITIAL, chunk);
            let mut block = [0.0; MODEL_FRAME_SAMPLES];
            let _ = output.fill(&mut block)?;
            rendered.extend_from_slice(&block);
        }
        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.state, LiveState::Running, "fixture {name}");
        assert_eq!(snapshot.model_frames, 20, "fixture {name}");
        assert_eq!(snapshot.model_errors, 0, "fixture {name}");
        assert_eq!(snapshot.transport.underflows, 0, "fixture {name}");
        assert_eq!(snapshot.transport.overflows, 0, "fixture {name}");
        assert!(
            rendered.iter().all(|sample| sample.is_finite()),
            "fixture {name}"
        );
        assert!(
            rendered.iter().all(|sample| (-1.0..=1.0).contains(sample)),
            "fixture {name}"
        );
    }

    let (mut sink, _output, _control, telemetry) = create_live_channel(rnnoise()?)?;
    let mut invalid = [0.0; MODEL_FRAME_SAMPLES];
    invalid[17] = f32::NAN;
    invalid[81] = f32::INFINITY;
    sink.write(InputGeneration::INITIAL, &invalid);
    let invalid_snapshot = telemetry.snapshot();
    assert_eq!(invalid_snapshot.model_errors, 0);
    assert!(invalid_snapshot.sanitized_samples >= 2);

    let map = ChannelMap::new(
        &[ChannelPosition::FrontLeft, ChannelPosition::FrontRight],
        ChannelSelection::MixAll,
    )?;
    let stereo: Vec<f32> = (0..MODEL_FRAME_SAMPLES)
        .flat_map(|index| {
            let phase = 2.0 * core::f32::consts::PI * 440.0 * index as f32 / SAMPLE_RATE_HZ as f32;
            [phase.sin() * 0.2, phase.sin() * 0.1]
        })
        .collect();
    let mut mono = [0.0; MODEL_FRAME_SAMPLES];
    let report = map.process(&stereo, &mut mono)?;
    assert_eq!(report.frames, MODEL_FRAME_SAMPLES);
    sink.write(InputGeneration::INITIAL, &mono);
    assert_eq!(telemetry.snapshot().model_errors, 0);
    Ok(())
}

#[test]
#[ignore = "reference-host release performance and ten-minute-equivalent RSS run"]
fn live_rnnoise_meets_cpu_deadline_callback_and_rss_gates() -> Result<(), Box<dyn Error>> {
    let rss_before_kib = resident_kib()?;
    let (mut sink, mut output, control, telemetry) = create_live_channel(rnnoise()?)?;
    control.set_diagnostic_timing(true);
    let input = fixture("speech", MODEL_FRAME_SAMPLES);
    let mut rendered = [0.0; MODEL_FRAME_SAMPLES];

    for _ in 0..20 {
        sink.write(InputGeneration::INITIAL, &input);
        let _ = output.fill(&mut rendered)?;
    }
    let started = Instant::now();
    for _ in 0..PERFORMANCE_MODEL_FRAMES {
        sink.write(InputGeneration::INITIAL, &input);
        let _ = output.fill(&mut rendered)?;
        std::hint::black_box(rendered[127]);
    }
    let wall = started.elapsed();
    let rss_after_kib = resident_kib()?;
    let snapshot = telemetry.snapshot();
    let audio_ns = u64::try_from(PERFORMANCE_MODEL_FRAMES)
        .unwrap_or(u64::MAX)
        .saturating_mul(10_000_000);
    let active_cpu_percent = snapshot.callback_timing.total_ns as f64 * 100.0 / audio_ns as f64;
    let model_p99_ns = snapshot.model_timing.percentile_ns(99);
    let callback_p99_ns = snapshot.callback_timing.percentile_ns(99);
    let rss_growth_kib = rss_after_kib.saturating_sub(rss_before_kib);

    assert!(
        active_cpu_percent < 5.0,
        "active CPU was {active_cpu_percent}%"
    );
    assert!(model_p99_ns <= 750_000, "model p99 was {model_p99_ns} ns");
    assert!(
        callback_p99_ns < 2_670_000,
        "callback p99 was {callback_p99_ns} ns"
    );
    assert_eq!(snapshot.deadline_misses, 0);
    assert_eq!(snapshot.model_errors, 0);
    assert_eq!(snapshot.transport.underflows, 0);
    assert_eq!(snapshot.transport.overflows, 0);
    assert!(rss_after_kib < 50 * 1_024, "RSS was {rss_after_kib} KiB");
    assert!(rss_growth_kib < 5 * 1_024, "RSS grew {rss_growth_kib} KiB");
    println!(
        "NOIRE_PHASE5_PERFORMANCE model_frames={PERFORMANCE_MODEL_FRAMES} audio_seconds={} wall_ms={} active_cpu_percent={active_cpu_percent:.3} model_p99_ns={model_p99_ns} model_max_ns={} callback_p99_ns={callback_p99_ns} callback_max_ns={} rss_before_kib={rss_before_kib} rss_after_kib={rss_after_kib} rss_growth_kib={rss_growth_kib}",
        PERFORMANCE_MODEL_FRAMES / 100,
        wall.as_millis(),
        snapshot.model_timing.maximum_ns,
        snapshot.callback_timing.maximum_ns,
    );
    Ok(())
}

fn resident_kib() -> Result<u64, Box<dyn Error>> {
    let status = fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .ok_or("VmRSS missing from /proc/self/status")?;
    let value = line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or("VmRSS value missing")?
        .parse()?;
    Ok(value)
}
