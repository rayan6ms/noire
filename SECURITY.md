# Security policy

Please report suspected vulnerabilities through GitHub's private security
advisory flow for this repository. Include the affected version, a minimal
reproduction, and the expected impact when possible. Do not open a public issue
for an unpatched vulnerability.

## Unsafe Rust policy

First-party Rust is safe by default: the workspace forbids unsafe code, except
for the private native boundary in
`crates/noire-model-fastenhancer-sys/src/lib.rs`. CI scans every Rust source
under `crates/` and compares it with `.github/unsafe-allowlist.json`. The
allowlist records both the exact unsafe-bearing lines and the SHA-256 of the
complete audited source file, so a new unsafe construct or any boundary change
requires an explicit review.

The approved boundary contains seven unsafe constructs:

- `unsafe extern "C"` declares the five functions exported by the vendored
  FastEnhancer C runtime. Their names and ABI types mirror `exports.h`, and
  `build.rs` compiles and links that implementation in the same crate.
- `unsafe impl Send for State` is valid because the state is independently
  heap-owned, the native implementation has no thread-affine handle, safe Rust
  never exposes the pointer, and processing/reset require exclusive `&mut`
  access. `State` is not `Sync`.
- `fe_init` receives a live byte slice whose length has been checked to fit the
  C ABI. The runtime validates and copies the artifact; a null result is
  rejected before a `State` is exposed.
- `fe_get_hop_size` receives the live pointer just returned by initialization.
  An incompatible frame size causes construction to fail and destroys the
  state through `Drop`.
- `fe_process` receives one live, exclusively borrowed state and distinct
  fixed-size 512-sample input/output arrays. Native failure is converted to an
  error and the output is zeroed.
- `fe_reset` receives a live, exclusively borrowed state.
- `fe_destroy` runs once from `Drop` for the non-null pointer owned by `State`.

The native sources are pinned in
`crates/noire-model-fastenhancer-sys/vendor/fastenhancer/README.noire.md`, retain
their upstream license, and are built from the explicit source list in
`build.rs`. Changes to the FFI declarations, wrapper invariants, native source
list, or vendored runtime must be reviewed as one boundary change. After that
review, update this rationale and regenerate the exact allowlist hash; never
weaken or bypass the scanner to make CI pass.

## Dependency policy

`cargo-deny` is the policy gate for licenses, sources, yanked packages in the
resolved dependency graph, and the documented set of accepted unmaintained
GPUI transitive dependencies. `cargo-audit` independently rejects known
vulnerabilities across the complete lockfile while reporting informational
maintenance warnings for review.
