# Contributing

## What you need

* Rust
* Node.js

Nothing else is required. There is no `just` or `make`.

`cargo xtask` provides repository-specific tasks, while standard Cargo commands
such as `cargo check` and `cargo test` are used for normal Rust development.

## Setting up

```bash
npm install           # once
(cd sdk && npm install)
```

## Development workflow

Use the cheapest validation that provides meaningful feedback.

For normal Rust development, prefer standard Cargo commands:

```bash
cargo check
cargo check -p <package>

cargo test <test_name>
cargo test -p <package>
```

For changes spanning multiple crates:

```bash
cargo check --workspace
cargo test --workspace
```

Do not run the entire workspace test suite after every small edit. Group related
changes together and validate them at meaningful checkpoints.

`cargo check` is preferred over `cargo build` when only compilation needs to be
verified. It avoids producing binaries and provides fast compiler feedback.

When a change is localized, prefer checking or testing the affected package or
specific test instead of the entire workspace.

## Repository tasks

`cargo xtask` is used for repository-specific validation, generation, and
operations that are not adequately represented by a standard Cargo command.

```bash
cargo xtask check-fast   # fast workspace-wide cargo check
cargo xtask check-light  # checks that need no build
cargo xtask check        # complete project validation
cargo xtask fmt          # format
cargo xtask lint         # clippy
cargo xtask test         # workspace tests
cargo xtask schema       # JSON Schema and the sample themes
cargo xtask sdk           # SDK's type-level guarantees
cargo xtask abi           # stable ID compatibility
cargo xtask gen           # generate the spec section and SDK types
```

Several Cargo-backed tasks accept additional arguments. For example:

```bash
cargo xtask check-fast -p <package>
cargo xtask test -p <package> <test_name>
cargo xtask lint --all-targets
```

Do not use `cargo xtask` merely because it exists. Use the standard Cargo
command when it is the simpler and faster tool for the job.

Conversely, do not replace repository-specific `cargo xtask` behavior with
unrelated ad-hoc commands when that validation is required.

### `check-light`

`check-light` performs validation that does not require compiling the Rust
workspace.

It is appropriate for specification-only work and other changes where Rust
compilation is not relevant.

```bash
cargo xtask check-light
```

### `check-fast`

`check-fast` is the fast workspace-wide Rust compilation check.

```bash
cargo xtask check-fast
```

For a localized change, prefer:

```bash
cargo check -p <package>
```

For a cross-package change, `check-fast` or `cargo check --workspace` provides
broader compiler feedback.

### `check`

`check` is the complete project validation task. It includes formatting, Clippy,
the workspace test suite, schema validation, SDK validation, generated-file
validation, and stable-ID compatibility checks.

```bash
cargo xtask check
```

It is intentionally expensive.

Do **not** run `cargo xtask check` after every individual edit. Run it at a
meaningful completion checkpoint or when the scope of the change requires full
project validation.

### `test`

The default test task runs the workspace test suite:

```bash
cargo xtask test
```

During development, prefer a targeted Cargo test:

```bash
cargo test -p <package> <test_name>
```

This keeps the feedback loop fast without reducing final test coverage.

## Recommended validation by change size

### Small localized change

```bash
cargo check -p <package>
cargo test -p <package> <relevant_test>
```

### Multiple related changes in one package

```bash
cargo check -p <package>
cargo test -p <package>
```

### Cross-package change

```bash
cargo check --workspace
cargo test --workspace
```

### Specification-only change

```bash
cargo xtask check-light
```

### Generated code or SDK change

Use the relevant repository-specific tasks:

```bash
cargo xtask gen
cargo xtask sdk
```

and then perform the appropriate Rust checks and tests.

### Final validation

Before considering a substantial task complete, perform the validation required
by its scope. For a full project-level change:

```bash
cargo xtask check
```

Do not compensate for insufficient reasoning by repeatedly running expensive
checks. The goal is to validate thoroughly at the right checkpoints, not to
run the same expensive command after every edit.

## What a build costs

An everyday development build uses incremental compilation. Normal development
should prefer `cargo check` or targeted tests over release builds.

An everyday `cargo build --release` uses thin LTO and can finish on a modest
machine. Building tasks are held to `jobs = 2` in `.cargo/config.toml`.

If the machine has enough CPU cores and memory, parallelism can be increased
for a specific command:

```bash
CARGO_BUILD_JOBS=8 cargo build --release
```

The `dist` profile is for distribution and performance measurement:

```bash
cargo build --profile dist
```

> [!WARNING]
> `dist` uses fat LTO, which takes **several GB of memory** while linking, and
> `target/` grows to several GB as well. On a machine with limited memory,
> close other applications first.

Avoid `dist` builds unless they are specifically required.

`cargo clean` removes the build artifacts and therefore also removes the
incremental build cache. Do not use it as a routine troubleshooting step.

## Where the rules live

[`spec/`](spec/) is the single source of truth, and
[`spec/README.md`](spec/README.md) carries the rules this repository is written
by — including that code is English and the specification is Japanese.

Contributors must inspect the relevant specification before implementing a
change.
