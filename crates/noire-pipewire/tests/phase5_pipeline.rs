//! Phase-5 live `RNNoise` fixture and reference-host performance acceptance.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

use std::{
    error::Error,
    fs, thread,
    time::{Duration, Instant},
};

use noire_dsp::{
    ChannelMap, ChannelPosition, ChannelSelection, MODEL_FRAME_SAMPLES, SAMPLE_RATE_HZ,
};
use noire_model::DenoiserFactory;
use noire_model_rnnoise::RnnoiseFactory;
use noire_pipewire::{
    BYPASS_RING_CAPACITY, CaptureSink, InputGeneration, LiveState, create_live_channel,
};

const PERFORMANCE_MODEL_FRAMES: usize = 60_000;
const MODEL_FRAMES_PER_AUDIO_HOUR: usize = 360_000;

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

#[test]
#[ignore = "Phase-7 accelerated or realtime 8/24-hour release soak"]
fn release_audio_time_soak_keeps_memory_queues_and_fault_counters_bounded()
-> Result<(), Box<dyn Error>> {
    let configured_model_frames = std::env::var("NOIRE_PHASE7_SOAK_MODEL_FRAMES").ok();
    let configured_hours = std::env::var("NOIRE_PHASE7_SOAK_HOURS").ok();
    let model_frames = soak_model_frames(
        configured_model_frames.as_deref(),
        configured_hours.as_deref(),
    )?;
    let pace_realtime = std::env::var("NOIRE_PHASE7_SOAK_REALTIME").is_ok_and(|value| value == "1");
    let rss_before_kib = resident_kib()?;
    let mut peak_rss_kib = rss_before_kib;
    let (mut sink, mut output, control, telemetry) = create_live_channel(rnnoise()?)?;
    let input = fixture("speech", MODEL_FRAME_SAMPLES);
    let mut rendered = [0.0; MODEL_FRAME_SAMPLES];
    let mut generation = InputGeneration::INITIAL;

    for _ in 0..20 {
        sink.write(generation, &input);
        let _ = output.fill(&mut rendered)?;
    }
    let started = Instant::now();
    for frame in 0..model_frames {
        if frame > 0 && frame.is_multiple_of(MODEL_FRAMES_PER_AUDIO_HOUR) {
            generation = generation.next();
            sink.reset(generation);
            eprintln!(
                "phase7-soak: equivalent_audio_hours={} rss_kib={}",
                frame / MODEL_FRAMES_PER_AUDIO_HOUR,
                resident_kib()?
            );
        }
        if frame.is_multiple_of(1_000) {
            control.set_enabled((frame / 1_000) % 2 == 0);
            control.set_strength((frame % 10_000) as f32 / 10_000.0);
        }
        sink.write(generation, &input);
        let report = output.fill(&mut rendered)?;
        assert_eq!(usize::from(report.frames), MODEL_FRAME_SAMPLES);
        std::hint::black_box(rendered[127]);
        if frame.is_multiple_of(60_000) {
            peak_rss_kib = peak_rss_kib.max(resident_kib()?);
        }
        if pace_realtime {
            let completed_frames = u64::try_from(frame.saturating_add(1)).unwrap_or(u64::MAX);
            let due = started + Duration::from_millis(completed_frames.saturating_mul(10));
            thread::sleep(due.saturating_duration_since(Instant::now()));
        }
    }
    let wall = started.elapsed();
    let rss_after_kib = resident_kib()?;
    peak_rss_kib = peak_rss_kib.max(rss_after_kib);
    let snapshot = telemetry.snapshot();
    let rss_growth_kib = peak_rss_kib.saturating_sub(rss_before_kib);
    let expected_resets = model_frames.saturating_sub(1) / MODEL_FRAMES_PER_AUDIO_HOUR;
    let expected_resets = u64::try_from(expected_resets).unwrap_or(u64::MAX);

    assert_eq!(
        snapshot.model_frames,
        u64::try_from(model_frames)
            .unwrap_or(u64::MAX)
            .saturating_add(20)
    );
    assert_eq!(snapshot.model_errors, 0);
    assert_eq!(snapshot.state, noire_pipewire::LiveState::Running);
    assert_eq!(snapshot.model_resets, expected_resets);
    assert_eq!(snapshot.deadline_misses, 0);
    assert_eq!(snapshot.sanitized_samples, 0);
    assert_eq!(snapshot.hard_ceiling_samples, 0);
    assert_eq!(snapshot.transport.underflows, 0);
    assert_eq!(snapshot.transport.overflows, 0);
    assert_eq!(snapshot.transport.dropped_frames, 0);
    assert_eq!(snapshot.transport.missing_frames, 0);
    assert_eq!(snapshot.transport.oversized_requests, 0);
    assert_eq!(snapshot.transport.sanitized_samples, 0);
    assert_eq!(snapshot.transport.generation_resets, expected_resets);
    let queue_capacity = u64::try_from(BYPASS_RING_CAPACITY).unwrap_or(u64::MAX);
    assert!(snapshot.transport.current_frames <= queue_capacity);
    assert!(snapshot.transport.high_water_frames <= queue_capacity);
    assert!(
        snapshot.transport.discarded_stale_frames <= expected_resets.saturating_mul(queue_capacity)
    );
    assert!(
        snapshot.transport.startup_silence_frames
            <= expected_resets
                .saturating_add(1)
                .saturating_mul(queue_capacity)
    );
    assert!(rss_growth_kib < 5 * 1_024, "RSS grew {rss_growth_kib} KiB");
    println!(
        "NOIRE_PHASE7_SOAK pacing={} model_frames={model_frames} equivalent_audio_seconds={} wall_ms={} generations={} model_resets={} transport_resets={} discarded_stale={} startup_silence={} queue_high_water={} queue_capacity={} model_errors={} deadline_misses={} underflows={} overflows={} dropped={} missing={} oversized={} sanitized={} hard_ceiling={} rss_before_kib={rss_before_kib} rss_after_kib={rss_after_kib} rss_peak_kib={peak_rss_kib} rss_growth_kib={rss_growth_kib}",
        if pace_realtime {
            "realtime"
        } else {
            "accelerated"
        },
        model_frames / 100,
        wall.as_millis(),
        generation.get(),
        snapshot.model_resets,
        snapshot.transport.generation_resets,
        snapshot.transport.discarded_stale_frames,
        snapshot.transport.startup_silence_frames,
        snapshot.transport.high_water_frames,
        queue_capacity,
        snapshot.model_errors,
        snapshot.deadline_misses,
        snapshot.transport.underflows,
        snapshot.transport.overflows,
        snapshot.transport.dropped_frames,
        snapshot.transport.missing_frames,
        snapshot.transport.oversized_requests,
        snapshot
            .sanitized_samples
            .saturating_add(snapshot.transport.sanitized_samples),
        snapshot.hard_ceiling_samples,
    );
    Ok(())
}

fn soak_model_frames(
    configured_model_frames: Option<&str>,
    configured_hours: Option<&str>,
) -> Result<usize, Box<dyn Error>> {
    if configured_model_frames.is_some() && configured_hours.is_some() {
        return Err("configure soak model frames or hours, not both".into());
    }
    if let Some(value) = configured_model_frames {
        let frames = value.parse::<usize>()?;
        if frames == 0 {
            return Err("soak model frames must be greater than zero".into());
        }
        return Ok(frames);
    }
    let hours = configured_hours.unwrap_or("24").parse::<usize>()?;
    if !matches!(hours, 8 | 24) {
        return Err("Phase-7 soak hours must be exactly 8 or 24".into());
    }
    hours
        .checked_mul(MODEL_FRAMES_PER_AUDIO_HOUR)
        .ok_or_else(|| "soak model-frame count overflowed".into())
}

#[test]
fn soak_duration_configuration_is_exact_and_rejects_ambiguity() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        soak_model_frames(None, Some("8"))?,
        8 * MODEL_FRAMES_PER_AUDIO_HOUR
    );
    assert_eq!(
        soak_model_frames(None, Some("24"))?,
        24 * MODEL_FRAMES_PER_AUDIO_HOUR
    );
    assert_eq!(soak_model_frames(Some("100"), None)?, 100);
    assert!(soak_model_frames(None, Some("7")).is_err());
    assert!(soak_model_frames(Some("100"), Some("8")).is_err());
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
