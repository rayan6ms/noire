# Vendored FastEnhancer native runtime

These C sources are copied without behavioral modification from
`ryyr-ry/fastenhancer-web` commit
`1bfc497df7a5aae8e1f22835e8b97c71baf4a83b`. Noire compiles only the Base 48 kHz
configuration and the inference sources named in `build.rs`.

The upstream MIT license is preserved in `LICENSE`. The Rust FFI ownership and
array-shape boundary lives in `noire-model-fastenhancer-sys`; the rest of Noire
depends only on its safe API.
