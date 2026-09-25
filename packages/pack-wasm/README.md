# @vgames/pack-wasm

`crates/vgames-pack` compiled to WebAssembly (`--features wasm`, no zstd: the browser packer stores
chunks raw). Used by the admin-web upload worker (Agent 3) to plan, hash and build the manifest in
the browser. Owner: Agent 2.

```sh
cargo install wasm-pack      # once
pnpm --filter @vgames/pack-wasm build
```

The output in `pkg/` is generated and git-ignored. API and usage: see the module docs in
`crates/vgames-pack/src/wasm.rs` (`WasmPacker`, `Blake3Hasher`) and, re-exported from `vgames-core`,
`keyfileInfo`, `fingerprint` and `UnlockedKey` (decrypt a publisher key file and sign the manifest digest
inside the worker; call `free()` when done).
