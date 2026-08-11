//! Release panic/callback-boundary policy verification.

use std::{error::Error, fs, path::Path};

#[test]
fn release_profile_aborts_and_callback_sources_contain_no_unwind_adapter()
-> Result<(), Box<dyn Error>> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = fs::read_to_string(workspace.join("Cargo.toml"))?;
    let release = manifest
        .split("[profile.release]")
        .nth(1)
        .ok_or("release profile missing")?;
    assert!(
        release
            .lines()
            .any(|line| line.trim() == "panic = \"abort\"")
    );

    for relative in ["src/capture.rs", "src/source.rs", "src/live.rs"] {
        let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))?;
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        for forbidden in [
            "catch_unwind",
            "resume_unwind",
            "Mutex",
            "RwLock",
            "Condvar",
            "std::fs",
            "std::net",
            "println!",
            "eprintln!",
            "tracing::",
        ] {
            assert!(
                !production.contains(forbidden),
                "forbidden callback-boundary token {forbidden} found in {relative}"
            );
        }
    }
    Ok(())
}
