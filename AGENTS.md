# AGENTS.md

## Project Overview

This is a **Specification-Driven Development (SDD) Rust project**.

The `spec/` directory is the **single source of truth** for this repository.

Before writing, modifying, or designing code, you **MUST inspect the relevant specifications in `spec/` first**.

## Specification-First Workflow

For every task:

1. Inspect `spec/`.
2. Read `spec/README.md`.
3. Identify and read the specification sections relevant to the task.
4. Understand the requirements, constraints, interfaces, invariants, error cases, and examples.
5. Only then inspect the existing implementation.
6. Implement the requested change according to the specification.
7. Validate the implementation at appropriate checkpoints.
8. Perform the required final validation before considering the task complete.

Do **not** start coding before checking the specification.

Do not infer requirements from existing code when the specification defines the behavior.

If the implementation conflicts with the specification, the specification takes precedence.

If the specification is ambiguous, incomplete, or contradictory, ask for clarification before making a significant design decision.

## Repository Rules

Follow `CONTRIBUTING.md` in addition to this file.

### Required Tooling

The project requires only:

* Rust
* Node.js

Do **not** introduce or rely on `just`, `make`, or another task runner.

`cargo xtask` is the repository's task runner for **project-specific operations**.

However, `cargo xtask` is **not a replacement for standard Cargo development commands**.

Use standard Cargo commands such as `cargo check` and targeted `cargo test`
when they are the fastest appropriate way to validate an ordinary Rust change.

Use `cargo xtask` when the repository-specific behavior of an xtask is
required.

Do not manually reproduce a repository-specific xtask operation with unrelated
commands when that operation is required.

## Setup

The expected setup is:

```bash
npm install
(cd sdk && npm install)
```

## Next Tasks

`NEXT.md` contains the tasks and work items that should be worked on next.

Before deciding what to work on next, **MUST read `NEXT.md`**.

When completing a task listed in `NEXT.md`:

1. Implement and validate the task according to the specification.
2. Confirm that the task is actually complete.
3. **Remove the completed task from `NEXT.md`.**

Do not leave completed tasks in `NEXT.md`.

If implementation reveals that a task needs to be split into additional work, update
`NEXT.md` accordingly rather than marking the original task complete prematurely.

`NEXT.md` describes the current remaining work; it should not become a historical
record of completed tasks.

## Validation

The goal is to **maximize development speed without reducing correctness or quality**.

Do not trade correctness, test coverage, specification compliance, or validation
quality for faster iteration.

At the same time, avoid unnecessary validation overhead during implementation.

The correct strategy is **incremental validation**:

* use cheap checks for fast feedback;
* target the affected package or test when possible;
* group related edits together;
* run expensive project-wide validation at meaningful checkpoints;
* perform comprehensive final validation when the scope requires it.

Do **not** mechanically run the full test suite after every edit.

### Validation Priority

For ordinary Rust implementation work, prefer this progression:

```text
cargo check -p <affected-package>
        ↓
targeted cargo test
        ↓
cargo check --workspace
        ↓
broader/package-level tests
        ↓
project-specific xtask validation
        ↓
full project validation
```

This is a guideline, not a mandatory sequence. Skip steps that do not provide
meaningful additional information.

### During Implementation

#### Small isolated Rust changes

Prefer:

```bash
cargo check -p <affected-package>
```

If behavior changed, run the relevant test:

```bash
cargo test -p <affected-package> <test_name>
```

If the package is not known or package-level selection is impractical:

```bash
cargo check
```

Do **not** run `cargo xtask check` merely to verify that a small Rust change
compiles.

#### Multiple related changes

Group related edits into one coherent implementation unit.

After the group is complete, run the narrowest useful validation, for example:

```bash
cargo check -p <affected-package>
cargo test -p <affected-package>
```

Do not repeatedly rerun the same expensive check after every edit.

#### Cross-package changes

When multiple crates or workspace interfaces are affected:

```bash
cargo check --workspace
```

Then run the relevant tests.

`cargo xtask check-fast` is also available as the repository's named
workspace-wide fast compilation check:

```bash
cargo xtask check-fast
```

#### Specification-only changes

When only specifications are changed and no generated or implementation
artifacts need compilation:

```bash
cargo xtask check-light
```

#### Formatting-only changes

Use:

```bash
cargo xtask fmt
```

or the appropriate standard Cargo formatting command when only formatting needs
to be checked.

Do not run the full test suite solely because formatting changed.

#### Generated code changes

When generated code is affected:

1. Change the source of truth under `spec/`.
2. Run:

   ```bash
   cargo xtask gen
   ```
3. Validate the resulting changes with the appropriate targeted or project-wide
   checks.

Do not manually edit generated output when the specification workflow should
generate it.

### Expensive Checks

The following are intentionally expensive:

```bash
cargo xtask check
cargo xtask test
cargo test --workspace
cargo clippy --workspace
```

Do **not** run these after every individual edit.

In particular:

* Do not run `cargo xtask check` after every edit.
* Do not run `cargo xtask test` after every edit.
* Do not run `cargo test --workspace` after every edit.
* Do not repeatedly run workspace-wide Clippy during local iteration.
* Do not run all xtask checks repeatedly without a reason.
* Do not rerun a check when none of its relevant inputs have materially changed.

When several related changes are being made, finish the coherent group before
running expensive validation.

### Avoid Redundant Compilation

Do not run multiple commands solely because they all compile the same code.

For example, after a small implementation change, prefer:

```bash
cargo check -p <affected-package>
cargo test -p <affected-package> <relevant_test>
```

rather than immediately running:

```bash
cargo xtask check
cargo xtask test
cargo test --workspace
```

`cargo clippy` and `cargo test` perform compilation as part of their work, so
an additional `cargo check` immediately beforehand is not required unless
the fast compiler feedback itself is useful during iteration.

The goal is to reduce **redundant work**, not to reduce necessary validation.

### Before Completion

A completed task **MUST receive appropriate final validation**.

Do not consider a task complete merely because `cargo check` succeeds or a
single targeted test passes.

Before completion:

1. Verify the implementation against the relevant specification.
2. Verify affected interfaces, invariants, and error cases.
3. Run relevant targeted tests.
4. Run appropriate project-wide checks when the change requires them.
5. Confirm generated artifacts are up to date when applicable.
6. Confirm formatting and linting requirements are satisfied.
7. Run `cargo xtask check` for substantial changes that require full project
   validation.

The final validation must be proportional to the scope and risk of the change.

A trivial localized change does not automatically require every expensive
project-wide command.

A substantial, cross-cutting, public-interface, schema, ABI, or generated-code
change should receive comprehensive project-wide validation.

### Handling Failures

When validation fails:

1. Read and understand the failure.
2. Identify the root cause.
3. Fix the underlying issue rather than masking the failure.
4. Rerun the smallest relevant check to confirm the fix.
5. Continue with the remaining validation required for the task.
6. Once stable, perform the required final validation again.

Do not repeatedly rerun unrelated checks while diagnosing a localized failure.

## Available Commands

### Fast Rust feedback

```bash
cargo check
cargo check -p <package>
cargo check --workspace
```

Repository-specific equivalent:

```bash
cargo xtask check-fast
```

### Targeted tests

```bash
cargo test <test_name>
cargo test -p <package> <test_name>
cargo test -p <package>
```

### Repository-specific tasks

```bash
cargo xtask check-light
cargo xtask check
cargo xtask fmt
cargo xtask lint
cargo xtask test
cargo xtask schema
cargo xtask sdk
cargo xtask abi
cargo xtask gen
```

`cargo xtask` should be preferred when the task performs repository-specific
validation, generation, schema checking, ABI checking, or other behavior that
standard Cargo commands do not provide.

## Specification and Generated Code

`cargo xtask gen` generates the spec section and SDK types.

When changing material that is generated by the specification workflow:

1. Read the relevant files under `spec/`.
2. Make the source-of-truth change in the appropriate specification.
3. Run `cargo xtask gen` when generation is required.
4. Validate the resulting changes.

Do not manually edit generated output when the correct source is the
specification.

## Language Rules

Follow the language conventions documented in `spec/README.md`.

In particular:

* Code is written in **English**.
* The specification is written in **Japanese**.

Do not translate or rewrite specifications into English merely to make
implementation easier.

## Build Considerations

Normal development should use incremental debug compilation.

Prefer `cargo check` or targeted tests over release builds during development.

Normal release builds use thin LTO.

The repository limits build parallelism to `jobs = 2` in `.cargo/config.toml`.

If the machine has sufficient memory and higher parallelism is intentionally
desired, `CARGO_BUILD_JOBS` may be increased, for example:

```bash
CARGO_BUILD_JOBS=8 cargo build --release
```

Do not increase build parallelism blindly on low-memory machines.

The distribution/performance profile is:

```bash
cargo build --profile dist
```

Be aware that `dist` uses **fat LTO**, which can require several GB of memory
during linking and can make `target/` several GB larger.

Avoid unnecessarily running `dist` builds.

`cargo clean` removes build artifacts and incremental compilation state. Do not
use it routinely, because it destroys useful build caches.

## Change Discipline

Prefer the smallest change that satisfies the specification and requested task.

Do not perform unrelated refactors.

Do not silently change:

* Specifications
* Public APIs
* Stable IDs
* Schemas
* SDK guarantees
* Generated files

If a requested implementation requires a specification change, make that
relationship explicit.

## Quality Requirements

Speed of execution is important, but **correctness takes priority over speed**.

The agent should:

* Understand the requested behavior before implementing it.
* Prefer simple, maintainable solutions over clever shortcuts.
* Preserve existing invariants and compatibility requirements.
* Consider error handling and edge cases relevant to the changed behavior.
* Avoid introducing technical debt merely to make the current task faster.
* Avoid unnecessary refactoring that increases the scope of the change.
* Verify behavior rather than assuming that successful compilation implies
  correctness.
* Use tests as a correctness tool, not merely as a final formality.
* Prefer targeted validation during iteration.
* Prefer comprehensive validation at meaningful completion checkpoints.
* Avoid repeating expensive commands when their relevant inputs have not changed.
* Optimize validation by reducing redundant work rather than by skipping
  necessary checks.

When a change is non-trivial, spend enough time reasoning about its design
before making implementation changes. Do not compensate for poor reasoning by
relying on repeated test runs.

## Priority

When determining intended behavior, use this order:

1. Explicit user requirements
2. `spec/` and its rules
3. `CONTRIBUTING.md`
4. Existing project architecture and conventions
5. Existing implementation behavior
6. Assumptions

The existing implementation is **not** the source of truth when it conflicts
with the specification.
