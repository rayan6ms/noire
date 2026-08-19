//! Builds the vendored `FastEnhancer-B` 48 kHz C inference runtime.

use std::path::Path;

fn main() {
    const ENGINE: &str = "vendor/fastenhancer/src/engine";
    let engine = Path::new(ENGINE);
    let common = engine.join("common");
    let configs = engine.join("configs");
    let sources = [
        "exports.c",
        "fastenhancer.c",
        "pipeline.c",
        "common/activations.c",
        "common/attention.c",
        "common/compression.c",
        "common/conv.c",
        "common/fft.c",
        "common/gru.c",
        "common/stft.c",
    ];

    let mut build = cc::Build::new();
    build
        .define("FE_USE_BASE_48K", None)
        .include(engine)
        .include(common)
        .include(configs)
        .warnings(true)
        .extra_warnings(true);
    for source in sources {
        build.file(engine.join(source));
    }
    build.compile("fastenhancer_base_48k");

    println!("cargo:rustc-link-lib=m");
    println!("cargo:rerun-if-changed={ENGINE}");
}
