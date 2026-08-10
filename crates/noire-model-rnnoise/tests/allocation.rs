//! Allocation instrumentation for warmed production DSP and `RNNoise` calls.

#![cfg(feature = "rnnoise")]

use std::alloc::System;
use std::error::Error;

use noire_dsp::{
    ChannelMap, ChannelPosition, ChannelSelection, ClickDetector, DcBlocker, DryDelay,
    EqualPowerMixer, FaultRamp, FrameAssembler, Meter, MixReport, sanitize_buffer,
};
use noire_model::DenoiserFactory;
use noire_model_rnnoise::{RNNOISE_FRAME_SAMPLES, RnnoiseFactory};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn warmed_dsp_and_model_processing_allocate_nothing() -> Result<(), Box<dyn Error>> {
    let factory = RnnoiseFactory::new()?;
    let mut model = factory.create()?;
    let map = ChannelMap::new(
        &[ChannelPosition::FrontLeft, ChannelPosition::FrontRight],
        ChannelSelection::MixAll,
    )?;
    let mut dc = DcBlocker::new();
    let mut assembler = FrameAssembler::new();
    let mut delay = DryDelay::new(RNNOISE_FRAME_SAMPLES)?;
    let mut meter = Meter::new();
    let mut fault_ramp = FaultRamp::new();
    let mut click_detector = ClickDetector::new();
    let input = signal_frame();
    let mut stereo = [0.0; RNNOISE_FRAME_SAMPLES * 2];
    for (frame, sample) in stereo.chunks_exact_mut(2).zip(input) {
        frame.fill(sample);
    }
    let mut mono = input;
    let mut delayed = [0.0; RNNOISE_FRAME_SAMPLES];
    let mut model_output = [0.0; RNNOISE_FRAME_SAMPLES];
    let mut mixed = [0.0; RNNOISE_FRAME_SAMPLES];
    let mut safe_output = [0.0; RNNOISE_FRAME_SAMPLES];

    model.process_frame(&input, &mut model_output)?;
    let region = Region::new(GLOBAL);
    for _ in 0..1_024 {
        map.process(&stereo, &mut mono)?;
        sanitize_buffer(&mut mono);
        dc.process(&mut mono);
        assembler.push(&mono, |_| {})?;
        delay.process(&mono, &mut delayed)?;
        model.process_frame(&mono, &mut model_output)?;
        let mut report = MixReport::default();
        for ((dry, wet), output) in delayed
            .iter()
            .zip(model_output.iter())
            .zip(mixed.iter_mut())
        {
            *output = EqualPowerMixer::mix(*dry, *wet, 0.75, &mut report);
        }
        fault_ramp.process(Some(&mixed), &mut safe_output)?;
        click_detector.observe(&mixed, &safe_output)?;
        meter.observe(&safe_output);
        meter.take_snapshot();
    }
    let change = region.change();

    assert_eq!(change.allocations, 0, "allocation calls: {change:?}");
    assert_eq!(change.reallocations, 0, "reallocation calls: {change:?}");
    assert_eq!(change.deallocations, 0, "deallocation calls: {change:?}");
    assert_eq!(click_detector.clicks(), 0);
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn signal_frame() -> [f32; RNNOISE_FRAME_SAMPLES] {
    let mut frame = [0.0; RNNOISE_FRAME_SAMPLES];
    for (index, sample) in frame.iter_mut().enumerate() {
        let phase = 2.0 * core::f32::consts::PI * 523.25 * index as f32 / 48_000.0;
        *sample = phase.sin() * 0.2;
    }
    frame
}
