# Contributing

## What you need

- Rust
- Node.js

Nothing else. There is no `just`, no `make`: `cargo xtask` is the task runner, so
cargo is enough.

## Setting up

```bash
npm install           # once
(cd sdk && npm install)
```

## Tasks

```bash
cargo xtask check-light  # checks that need no build (a few seconds)
cargo xtask check        # every check
cargo xtask fmt          # format
cargo xtask lint         # clippy
cargo xtask test         # tests
cargo xtask schema       # JSON Schema and the sample themes
cargo xtask sdk          # the SDK's type-level guarantees
cargo xtask abi          # stable ID compatibility
cargo xtask gen          # generate the spec section and SDK types
```

`check-light` is enough while working on the specification.

## What a build costs

An everyday `cargo build --release` uses thin LTO and finishes on a modest
machine. Building tasks are held to `jobs = 2` in `.cargo/config.toml`; with
more memory, raise it with `CARGO_BUILD_JOBS=8`.

The `dist` profile is for distribution and performance measurement:

```bash
cargo build --profile dist
```

> [!WARNING]
> `dist` uses fat LTO, which takes **several GB of memory** while linking, and
> `target/` grows to several GB as well. On a machine with limited memory,
> close other applications first.

`cargo clean` removes the build artifacts.

## Where the rules live

[`spec/`](spec/) is the single source of truth, and
[`spec/README.md`](spec/README.md) carries the rules this repository is written
by — including that code is English and the specification is Japanese.
