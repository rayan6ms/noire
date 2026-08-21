# FastEnhancer-B 48 kHz model artifact

`fe_base_48k.bin` is the production FastEnhancer Base 48 kHz weight artifact.

- SHA-256: `a3f475e6ae0cfbe337a411f4f2d01b0cdc49a3fbf1eed02ad46dd355074d0071`
- Bytes: `406636`
- Runtime source: `ryyr-ry/fastenhancer-web` commit
  `1bfc497df7a5aae8e1f22835e8b97c71baf4a83b`
- Model origin: `aask1357/fastenhancer`, release `onnx-48khz-v1`, commit
  `8d2d41931b1de316f10da15583f431761bf903ad`
- License: MIT

The corresponding official ONNX artifact has SHA-256
`70e23bba3d41e80d30ebc5eba39d9df64f0e0315f31c772022bb17576c4d96bf`.
Noire evaluated both that official graph and the native artifact before selecting
the native runtime. The frozen results and shipping decision are recorded in
`tests/quality/fastenhancer-b-v1.toml`.
