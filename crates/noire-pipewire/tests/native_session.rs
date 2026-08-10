//! End-to-end capture assertions for a disposable native `PipeWire` graph.

#![cfg(feature = "native-test")]

use std::{
    error::Error,
    fs, thread,
    time::{Duration, Instant},
};

use noire_pipewire::{
    CANONICAL_CAPTURE_FORMAT, CaptureStreamState, NativeCaptureStream, NegotiatedFormatEvent,
    PipewireConnection, SYNTHETIC_SOURCE_RATE, SyntheticSource,
};

const SOURCE_NAME: &str = "noire.integration.source.44100";
const SESSION_TIMEOUT: Duration = Duration::from_secs(10);
const RSS_GROWTH_LIMIT_KIB: u64 = 16 * 1024;

#[test]
#[ignore = "requires a disposable native PipeWire session"]
fn captures_deterministic_44100_source_as_canonical_48000() -> Result<(), Box<dyn Error>> {
    let connection = PipewireConnection::connect_default()?;
    let source = SyntheticSource::connect(&connection, SOURCE_NAME)?;
    connection.request_roundtrip()?;

    wait_until(&connection, SESSION_TIMEOUT, || {
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|candidate| candidate.node_name == SOURCE_NAME)
    })?;
    assert_eq!(source.sample_rate(), SYNTHETIC_SOURCE_RATE);

    let capture = NativeCaptureStream::connect(&connection, source.node_name())?;
    let mut negotiated = None;
    wait_until(&connection, SESSION_TIMEOUT, || {
        if let Some(event) = capture.take_negotiated_format() {
            negotiated = Some(event);
        }
        negotiated.is_some() && capture.state() == CaptureStreamState::Streaming
    })?;
    assert_eq!(
        negotiated,
        Some(NegotiatedFormatEvent::Accepted(CANONICAL_CAPTURE_FORMAT))
    );

    let duration = soak_duration();
    let steady_rss_kib = resident_set_kib().unwrap_or_default();
    let mut peak_rss_kib = steady_rss_kib;
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        if let Some(rss_kib) = resident_set_kib() {
            peak_rss_kib = peak_rss_kib.max(rss_kib);
        }
        assert_ne!(capture.state(), CaptureStreamState::Error);
        if let Some(format_event) = capture.take_negotiated_format() {
            assert_eq!(
                format_event,
                NegotiatedFormatEvent::Accepted(CANONICAL_CAPTURE_FORMAT)
            );
        }
        assert!(source.take_error().is_none());
        assert!(connection.take_failure().is_none());
    }

    let capture_snapshot = capture.telemetry().snapshot();
    let source_snapshot = source.telemetry();
    assert!(capture_snapshot.counters.callbacks > 0);
    assert!(capture_snapshot.counters.frames >= minimum_capture_frames(duration));
    assert_eq!(capture_snapshot.counters.malformed_chunks, 0);
    assert_eq!(capture_snapshot.counters.oversized_chunks, 0);
    assert_eq!(capture_snapshot.counters.non_finite_samples, 0);
    assert_eq!(capture_snapshot.counters.subnormal_samples, 0);
    assert!(capture_snapshot.peak > 0.05);
    assert!(capture_snapshot.peak < 0.25);
    assert!(source_snapshot.callbacks() > 0);
    assert!(source_snapshot.frames() > 0);
    assert_eq!(source_snapshot.missing_data(), 0);
    let rss_growth_kib = peak_rss_kib.saturating_sub(steady_rss_kib);
    assert!(rss_growth_kib <= RSS_GROWTH_LIMIT_KIB);

    println!(
        "NOIRE_PIPEWIRE_RESULT duration_ms={} source_rate={} capture_rate={} callbacks={} frames={} empty={} malformed={} oversized={} peak={:.6} rss_growth_kib={}",
        duration.as_millis(),
        SYNTHETIC_SOURCE_RATE,
        CANONICAL_CAPTURE_FORMAT.sample_rate,
        capture_snapshot.counters.callbacks,
        capture_snapshot.counters.frames,
        capture_snapshot.counters.empty_buffers,
        capture_snapshot.counters.malformed_chunks,
        capture_snapshot.counters.oversized_chunks,
        capture_snapshot.peak,
        rss_growth_kib,
    );
    Ok(())
}

fn wait_until(
    connection: &PipewireConnection,
    timeout: Duration,
    mut condition: impl FnMut() -> bool,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        if condition() {
            return Ok(());
        }
        thread::yield_now();
    }
    Err("native PipeWire session condition timed out")
}

fn soak_duration() -> Duration {
    std::env::var("NOIRE_PIPEWIRE_TEST_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map_or(Duration::from_secs(5), Duration::from_secs)
}

fn minimum_capture_frames(duration: Duration) -> u64 {
    let expected = duration
        .as_secs()
        .saturating_mul(u64::from(CANONICAL_CAPTURE_FORMAT.sample_rate));
    expected.saturating_mul(9) / 10
}

fn resident_set_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_ascii_whitespace().nth(1)?.parse().ok()
}
