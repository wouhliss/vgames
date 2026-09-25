# Runtime-catalog test vectors (A5-T12)

For the launcher's runtime manager (Agent 2, A2-T16/T17). Verify with
`vgames_core::runtimes::verify_catalog(bytes, minisig, public_key, last_seen_version)`, then check each archive's
SHA-256 and size **before** extracting it. `tests/runtimes_vectors.rs` pins these outcomes.

| File | Use | Expected |
|---|---|---|
| `runtime-catalog-test.pub` | the test runtime-catalog public key (a real launcher compiles in the project's key instead) | — |
| `runtimes-v2.json` + `runtimes-v2.json.minisig` | the current catalog, version 2 | valid; `newer` when the last seen version is 1 |
| `runtimes-v1.json` + `runtimes-v1.json.minisig` | an older, genuinely signed catalog | valid with no history; `Rollback { got: 1, seen: 2 }` once version 2 was seen |
| `runtimes-v2-tampered.json` + `runtimes-v2.json.minisig` | one character of a pinned SHA-256 changed | `BadSignature` |
| `runtimes-v2.json` + `runtimes-v2.json.other-key.minisig` | signed by a key the launcher does not know | `BadSignature` |
| `test-runtime.tar.gz` | the archive pinned by the `umu-launcher` entry of v2 (165 bytes, one script) | SHA-256 and size match |
| `test-runtime-tampered.tar.gz` | the same archive with one byte flipped | SHA-256 mismatch: refuse before extracting |

The secret key that signed these was generated for the run and never written to disk. To rebuild everything with a
new key: `cargo test -p vgames-core --test runtimes_vectors -- --ignored`.
