# Build / verification commands for media-manager

- **Format check:** `cargo fmt --all -- --check`
- **Lint:** `cargo clippy --all-targets -- -D warnings`
- **Test:** `cargo test --all-features`
- **Run CLI:** `cargo run -p mm-cli --`
- **Run GUI:** `cargo run -p mm-gui`

The workspace uses a single pinned toolchain via `rust-toolchain.toml` (stable).
`mm-core`/`mm-parse` target a low MSRV (1.85); the GUI crate floats with stable.
