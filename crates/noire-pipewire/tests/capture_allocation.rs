//! Allocation instrumentation for the portable capture callback processor.

use std::alloc::System;

use noire_dsp::MODEL_FRAME_SAMPLES;
use noire_model::{
    Denoiser, DenoiserFactory, FrameStats, ModelDescriptor, ModelDescriptorSpec, ProcessError,
    finalize_process_output, prepare_process_frame,
};
use noire_model_fastenhancer::FastEnhancerFactory;
use noire_pipewire::{
    BYPASS_STARTUP_QUANTA, CaptureProcessor, CaptureSink, CaptureTelemetry, ChunkMetadata,
    InputGeneration, create_bypass_channel, create_live_channel,
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
const BYPASS_QUANTUM: usize = 128;
const PHASE5_CALLBACK_INVOCATIONS: usize = 10_000_000;

struct AllocationModel {
    descriptor: ModelDescriptor,
}

impl AllocationModel {
    fn new() -> Result<Self, noire_model::DescriptorError> {
        Ok(Self {
            descriptor: ModelDescriptor::new(ModelDescriptorSpec {
                id: "test.allocation.live",
                name: "Allocation test model",
                version: "1",
                license: "MIT",
                sample_rate_hz: 48_000,
                channels: 1,
                frame_samples: MODEL_FRAME_SAMPLES,
                hop_samples: MODEL_FRAME_SAMPLES,
                lookahead_samples: 0,
                delay_samples: MODEL_FRAME_SAMPLES,
            })?,
        })
    }
}

impl Denoiser for AllocationModel {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn reset(&mut self) {}

    fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<FrameStats, ProcessError> {
        prepare_process_frame(&self.descriptor, input, output)?;
        output.copy_from_slice(input);
        finalize_process_output(output, FrameStats::silence())
    }
}

#[derive(Debug, Default)]
struct CountingSink {
    samples: u64,
    checksum: f32,
}

impl CaptureSink for CountingSink {
    fn reset(&mut self, _generation: InputGeneration) {
        self.samples = 0;
        self.checksum = 0.0;
    }

    fn write(&mut self, _generation: InputGeneration, samples: &[f32]) {
        self.samples = self
            .samples
            .saturating_add(u64::try_from(samples.len()).unwrap_or(u64::MAX));
        self.checksum += samples.iter().copied().sum::<f32>();
    }
}

#[test]
fn warmed_capture_and_bypass_callbacks_have_zero_allocator_calls()
-> Result<(), Box<dyn std::error::Error>> {
    let samples = [0.125_f32; 128];
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_ne_bytes())
        .collect();
    let metadata = ChunkMetadata {
        size_bytes: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        stride_bytes: 4,
        ..ChunkMetadata::default()
    };
    let mut processor = CaptureProcessor::new(CountingSink::default(), CaptureTelemetry::default());
    assert!(processor.process_mapped(Some(&bytes), metadata).is_ok());

    let region = Region::new(GLOBAL);
    for _ in 0..1_024 {
        assert!(processor.process_mapped(Some(&bytes), metadata).is_ok());
    }
    let change = region.change();

    assert_eq!(change.allocations, 0);
    assert_eq!(change.reallocations, 0);
    assert_eq!(change.deallocations, 0);
    assert_eq!(processor.sink().samples, 128 * 1_025);
    assert!(processor.sink().checksum.is_finite());

    let (mut producer, mut output, _control, telemetry) = create_bypass_channel();
    let initial = [0.125_f32; MODEL_FRAME_SAMPLES + BYPASS_STARTUP_QUANTA * BYPASS_QUANTUM];
    let refill = [0.125_f32; BYPASS_QUANTUM];
    let mut destination = [0.0_f32; BYPASS_QUANTUM];

    producer.write(InputGeneration::INITIAL, &initial);
    assert!(output.fill(&mut destination).is_ok());

    let region = Region::new(GLOBAL);
    for _ in 0..1_024 {
        producer.write(InputGeneration::INITIAL, &refill);
        assert!(output.fill(&mut destination).is_ok());
    }
    let change = region.change();

    assert_eq!(change.allocations, 0);
    assert_eq!(change.reallocations, 0);
    assert_eq!(change.deallocations, 0);
    assert_eq!(telemetry.snapshot().underflows, 0);
    assert_eq!(telemetry.snapshot().overflows, 0);
    assert!(destination.iter().all(|sample| sample.is_finite()));

    let fastenhancer = FastEnhancerFactory::new()?.create()?;
    let (mut sink, mut output, _control, telemetry) = create_live_channel(fastenhancer)?;
    let input = [0.125_f32; MODEL_FRAME_SAMPLES];
    let mut rendered = [0.0_f32; MODEL_FRAME_SAMPLES];
    for _ in 0..20 {
        sink.write(InputGeneration::INITIAL, &input);
        assert!(output.fill(&mut rendered).is_ok());
    }

    let region = Region::new(GLOBAL);
    for _ in 0..1_024 {
        sink.write(InputGeneration::INITIAL, &input);
        assert!(output.fill(&mut rendered).is_ok());
    }
    let change = region.change();
    assert_eq!(change.allocations, 0);
    assert_eq!(change.reallocations, 0);
    assert_eq!(change.deallocations, 0);
    let snapshot = telemetry.snapshot();
    assert_eq!(snapshot.model_errors, 0);
    assert_eq!(snapshot.hard_ceiling_samples, 0);
    assert_eq!(snapshot.transport.underflows, 0);
    assert_eq!(snapshot.transport.overflows, 0);
    Ok(())
}

#[test]
#[ignore = "10 million callback allocation acceptance; run explicitly in release mode"]
fn ten_million_live_callback_invocations_have_zero_allocator_calls()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut sink, mut output, _control, telemetry) =
        create_live_channel(Box::new(AllocationModel::new()?))?;
    let input = [0.125_f32];
    let mut rendered = [0.0_f32; MODEL_FRAME_SAMPLES];

    for _ in 0..MODEL_FRAME_SAMPLES {
        sink.write(InputGeneration::INITIAL, &input);
    }
    let _ = output.fill(&mut rendered)?;

    let region = Region::new(GLOBAL);
    for invocation in 0..PHASE5_CALLBACK_INVOCATIONS {
        sink.write(InputGeneration::INITIAL, &input);
        if (invocation + 1).is_multiple_of(MODEL_FRAME_SAMPLES) {
            let _ = output.fill(&mut rendered)?;
            std::hint::black_box(rendered[invocation % MODEL_FRAME_SAMPLES]);
        }
    }
    let change = region.change();
    assert_eq!(change.allocations, 0);
    assert_eq!(change.reallocations, 0);
    assert_eq!(change.deallocations, 0);
    let snapshot = telemetry.snapshot();
    assert_eq!(snapshot.model_errors, 0);
    assert_eq!(snapshot.transport.underflows, 0);
    assert_eq!(snapshot.transport.overflows, 0);
    println!(
        "NOIRE_PHASE5_ALLOCATION callbacks={PHASE5_CALLBACK_INVOCATIONS} model_frames={} allocations={} reallocations={} deallocations={}",
        snapshot.model_frames, change.allocations, change.reallocations, change.deallocations
    );
    Ok(())
}
