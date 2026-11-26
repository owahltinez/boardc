# boardc Contributor Guidelines

## Verification Gate
```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --release
```

## Engineering Rules
- Read `ARCHITECTURE.md` before changing IR, geometry, or I/O code.
- Keep modules focused (~500 lines max; split cleanly if a module grows past that).
- Minimal solutions, maximal rigor; no speculative abstractions or dead code.
- Comments: one line max, only for non-obvious invariants or compromises. Never multi-line comments.
- Never chase KiCad bug-compatibility. Verify physical and electrical invariants independently.
