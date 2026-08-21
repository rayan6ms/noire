//! Allocation regression test for the production model callback.

use std::{alloc::System, error::Error};

use noire_model::DenoiserFactory;
use noire_model_fastenhancer::{FASTENHANCER_FRAME_SAMPLES, FastEnhancerFactory};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn frame_processing_does_not_allocate() -> Result<(), Box<dyn Error>> {
    let factory = FastEnhancerFactory::new()?;
    let mut model = factory.create()?;
    let input = [0.0; FASTENHANCER_FRAME_SAMPLES];
    let mut output = [0.0; FASTENHANCER_FRAME_SAMPLES];
    model.process_frame(&input, &mut output)?;

    let region = Region::new(GLOBAL);
    for _ in 0..64 {
        model.process_frame(&input, &mut output)?;
    }
    let change = region.change();
    assert_eq!(change.allocations, 0);
    assert_eq!(change.deallocations, 0);
    assert_eq!(change.reallocations, 0);
    Ok(())
}
