# Gumicord

The fastest and customizable discord unofficial client

> [!WARNING]
> According to Discord's Terms of Service, custom clients are not allowed!
> Please keep in mind that using Gumicord carries the risk of getting your account banned!

## Features

- Easy to install
- Basically discord implements
- Faster than the official client
- Custom themes and plugins

## Installing / Uninstalling

> [!CAUTION]
> That section is currently under preparation.

## Build

If you want to build Gumicord, you need them:
- Rust
- Node.js

### Commands

```bash
npm install           # Only the first
(cd sdk && npm install)

cargo xtask check-light  # Inspection only, without a build (a few seconds)
cargo xtask check        # All Tests (Including Builds)
cargo xtask fmt          # Beautify
cargo xtask lint         # Clippy
cargo xtask test         # Test
cargo xtask schema       # Validating JSON Schema and Official Samples
cargo xtask sdk          # Verify the SDK's type-level guarantees
cargo xtask abi          # Backward Compatibility Testing for Stable IDs
```

### Resource Usage During Builds

A routine `cargo build --release` run uses thin LTO and will complete even on modest machines.

Use the `dist` profile for distribution and performance measurements.

```bash
cargo build --profile dist
```

> [!WARNING]
> Since `dist` uses fat LTO, it consumes **several GB of memory** during linking, and the `target/` directory will also grow to several GB. On machines with limited memory, please close other applications before running this command.

You can delete unnecessary build artifacts with `cargo clean`. You can safely delete spike artifacts with `rm -rf spike/*/target` (the code remains, and the measurement results are already recorded in [`spec/adr/`](spec/adr/)).

## Disclaimer

"Discord" is a trademark of Discord Inc. and is used herein solely for illustrative purposes. Its inclusion does not imply any affiliation with or endorsement by Discord Inc.

## License

MIT License — [LICENSE](LICENSE)
