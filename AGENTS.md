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

`cargo xtask` is the repository's task runner.

Use the project's `cargo xtask` commands rather than manually reproducing their behavior with unrelated commands.

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

If implementation reveals that a task needs to be split into additional work, update `NEXT.md` accordingly rather than marking the original task complete prematurely.

`NEXT.md` describes the current remaining work; it should not become a historical record of completed tasks.

## Validation

The goal is to **maximize development speed without reducing correctness or quality**.

Do not trade correctness, test coverage, specification compliance, or validation quality for faster iteration.

At the same time, avoid unnecessary validation overhead during implementation.

### Efficient Validation Strategy

Validation should be performed at **meaningful checkpoints**, not mechanically after every individual edit.

During implementation:

* Group related changes and implement them together.
* Avoid repeatedly running expensive checks when the code has only undergone small, related edits.
* Prefer targeted or lightweight validation when it provides sufficient feedback.
* Use compiler errors, code inspection, and focused checks to catch obvious issues early.
* Run more expensive validation once a coherent implementation unit is complete.
* Do not interrupt a productive implementation flow merely to run an unrelated expensive check.
* Never skip a validation step that is necessary to establish correctness.

The objective is not to run fewer checks at all costs.

The objective is to run the **right checks at the right time**.

### Validation Checkpoints

Use validation at these checkpoints:

#### During implementation

Use the cheapest check that meaningfully reduces uncertainty.

Examples:

* Specification-only changes → `cargo xtask check-light`
* Formatting changes → `cargo xtask fmt`
* Small isolated changes → targeted checks or tests when available
* Related implementation changes → validate after the related batch is complete
* Changes affecting generated code → regenerate and validate as required

#### Before completion

A completed task **MUST receive appropriate final validation**.

Do not consider a task complete merely because the code compiles or a targeted test passes.

For a complete change:

1. Verify the implementation against the relevant specification.
2. Verify affected interfaces, invariants, and error cases.
3. Run the relevant targeted tests.
4. Run the appropriate project-wide checks.
5. Confirm generated artifacts are up to date when applicable.
6. Confirm formatting and linting requirements are satisfied.

### Expensive Checks

Do **not** run expensive project-wide checks after every small edit.

In particular:

* Do not run `cargo xtask test` after every individual edit.
* Do not run all `cargo xtask` checks repeatedly during the same implementation cycle without a reason.
* Do not rerun checks whose relevant inputs have not materially changed.
* When several related changes are being made, finish the coherent group before running expensive validation.

However, expensive checks **MUST still be run when required for final validation**.

Reducing validation frequency during implementation must not reduce final validation quality.

### Handling Failures

When validation fails:

1. Read and understand the failure.
2. Identify the root cause.
3. Fix the underlying issue rather than masking the failure.
4. Rerun the smallest relevant check to confirm the fix.
5. Continue with the remaining validation required for the task.

Do not repeatedly rerun unrelated checks while diagnosing a localized failure.

Once the implementation is stable, perform the required final validation again.

### Available Validation Commands

Use the repository's task runner for checks:

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

When working on the **specification**, `cargo xtask check-light` is sufficient during iteration.

For a complete change, run the appropriate full checks before considering the work complete.

Do not replace these commands with `make`, `just`, or ad-hoc equivalent commands unless explicitly requested.

## Specification and Generated Code

`cargo xtask gen` generates the spec section and SDK types.

When changing material that is generated by the specification workflow:

1. Read the relevant files under `spec/`.
2. Make the source-of-truth change in the appropriate specification.
3. Run `cargo xtask gen` when generation is required.
4. Validate the resulting changes.

Do not manually edit generated output when the correct source is the specification.

## Language Rules

Follow the language conventions documented in `spec/README.md`.

In particular:

* Code is written in **English**.
* The specification is written in **Japanese**.

Do not translate or rewrite specifications into English merely to make implementation easier.

## Build Considerations

Normal release builds use thin LTO.

The repository limits build parallelism to `jobs = 2` in `.cargo/config.toml`.

If the machine has sufficient memory and a higher parallelism is intentionally desired, `CARGO_BUILD_JOBS` may be increased, for example:

```bash
CARGO_BUILD_JOBS=8 cargo build --release
```

The distribution/performance profile is:

```bash
cargo build --profile dist
```

Be aware that `dist` uses **fat LTO**, which can require several GB of memory during linking and can make `target/` several GB larger.

Avoid unnecessarily running `dist` builds.

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

If a requested implementation requires a specification change, make that relationship explicit.

## Quality Requirements

Speed of execution is important, but **correctness takes priority over speed**.

The agent should:

* Understand the requested behavior before implementing it.
* Prefer simple, maintainable solutions over clever shortcuts.
* Preserve existing invariants and compatibility requirements.
* Consider error handling and edge cases relevant to the changed behavior.
* Avoid introducing technical debt merely to make the current task faster.
* Avoid unnecessary refactoring that increases the scope of the change.
* Verify behavior rather than assuming that successful compilation implies correctness.
* Use tests as a correctness tool, not merely as a final formality.

When a change is non-trivial, spend enough time reasoning about its design before making implementation changes. Do not compensate for poor reasoning by relying on repeated test runs.

## Priority

When determining intended behavior, use this order:

1. Explicit user requirements
2. `spec/` and its rules
3. `CONTRIBUTING.md`
4. Existing project architecture and conventions
5. Existing implementation behavior
6. Assumptions

The existing implementation is **not** the source of truth when it conflicts with the specification.
