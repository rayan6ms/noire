# Security policy

Report suspected vulnerabilities through a
[private security advisory](https://github.com/rayan6ms/noire/security/advisories/new).
Please do not open a public issue before a fix is available.

Noire is pre-release. Security fixes currently target `main`; supported release
branches and response expectations will be documented before 1.0.

## Dependencies

Dependency changes are focused, reviewed updates rather than unattended bulk
updates. Each change must review the lockfile diff and pass the workspace, MSRV,
license, source, and fresh RustSec advisory checks. Native dependencies also
require review of build scripts, bundled code, unsafe/FFI surface, and notices.

An advisory exception requires a documented reason, owner, affected surface,
expiry or removal condition, and a tracking issue. There are no exceptions now.

## Unsafe Rust

The unsafe allowlist is currently empty: workspace manifests forbid unsafe code.
If a platform or model adapter later proves that unsafe Rust is unavoidable, the
change must be limited to a named module in `noire-pipewire` or
`noire-model-rnnoise`, explain every block with a `SAFETY` invariant, add focused
tests, and update this allowlist in the same review. Core, DSP, configuration,
IPC, UI, CLI, daemon, and test-support code remain safe Rust.
