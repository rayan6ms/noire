//! Allocation instrumentation for the portable capture callback processor.

use std::alloc::System;

use noire_pipewire::{
    CaptureProcessor, CaptureSink, CaptureTelemetry, ChunkMetadata, InputGeneration,
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

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
fn warmed_capture_processing_has_zero_allocator_calls() {
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
}
