# AGENTS.md

# dashplay

`dashplay` is a pure Rust MPEG-DASH player library.

This repository should follow Rust community best practices and produce production-quality, maintainable code.

For architectural decisions, see `ARCHITECTURE.md`.

---

# Development Principles

Prioritize:

1. Correctness
2. Standards compliance
3. Reliability
4. Maintainability
5. Performance
6. API ergonomics

Prefer simple, explicit solutions over clever abstractions.

When uncertain, choose the solution that would be considered idiomatic Rust by experienced Rust developers.

---

# Rust Guidelines

## General

- Use stable Rust only.
- Follow Rust API Guidelines.
- Keep dependencies minimal.
- Prefer standard library solutions where practical.
- Avoid unnecessary abstractions.
- Avoid premature optimization.
- Write code that is easy to understand and maintain.

---

## Safety

- Use safe Rust by default.
- Avoid `unsafe`.
- Any `unsafe` code requires:
  - a clear safety justification
  - comments explaining invariants
  - tests covering the behaviour

---

## Error Handling

Library code must never panic.

Avoid:

```rust
unwrap()
expect()
panic!()
```

except where clearly justified in tests or unreachable states.

Use:

- `thiserror` for library errors.
- Meaningful error variants.
- Error context preservation.

Errors should provide enough information for users to diagnose failures.

---

## Ownership and Borrowing

Prefer:

- borrowing over cloning
- slices over owned collections where possible
- iterators over indexing
- zero-copy approaches where practical

Avoid unnecessary allocations.

Do not introduce:

- `Arc`
- `Rc`
- `Mutex`
- `RefCell`

unless ownership requirements justify them.

---

## Async

Async should only be used for I/O operations.

Do not:

- block async tasks
- spawn hidden background tasks
- create internal runtimes

The application owns the async runtime.

Keep CPU-heavy processing synchronous unless there is a clear benefit.

---

# Public API Design

Public APIs should be:

- explicit
- predictable
- easy to discover
- documented

Avoid:

- hidden side effects
- global state
- singleton patterns
- excessive builder APIs

Use builders only when constructors become difficult to use.

---

# Traits

Use traits when:

- multiple implementations are expected
- abstraction provides real value
- dependency inversion is required

Avoid creating traits:

- only for mocking
- only to wrap a single implementation
- before a concrete need exists

Prefer generics when static dispatch is appropriate.

Prefer trait objects only when dynamic dispatch is required.

---

# Code Style

Before committing, run:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all
```

The repository should compile without warnings.

---

# Documentation

All public APIs require rustdoc.

Documentation should explain:

- what a type does
- when it should be used
- important invariants
- examples where appropriate

Prefer examples over long explanations.

---

# Testing

Every bug fix requires a regression test.

Tests should be:

- deterministic
- focused
- readable

Prefer testing real behaviour over implementation details.

## Unit Tests

Use unit tests for:

- parsers
- algorithms
- data transformations
- utility functions

## Integration Tests

Use integration tests for:

- complete playback flows
- manifest handling
- networking behaviour

## Standards Tests

Use DASH conformance vectors where applicable.

---

# Dependencies

Before adding a dependency, consider:

- Is this functionality already available in the standard library?
- Is the dependency actively maintained?
- Does it introduce unnecessary compile time?
- Does it fit the project goals?

Avoid dependencies that provide only small convenience wrappers.

---

# Logging

Use `tracing`.

Do not use:

```rust
println!
dbg!
```

in production code.

Logs should be:

- structured
- useful for debugging
- appropriately scoped

Avoid excessive logging in hot paths.

---

# Performance Guidelines

Performance matters, but correctness comes first.

Prefer:

- avoiding unnecessary allocations
- avoiding unnecessary copies
- efficient parsing
- efficient buffering

Measure before optimizing.

Do not sacrifice readability without evidence.

---

# Naming

Use conventional Rust naming:

| Item | Convention |
|---|---|
| Types | `PascalCase` |
| Functions | `snake_case` |
| Variables | `snake_case` |
| Constants | `SCREAMING_SNAKE_CASE` |

Names should describe intent.

Avoid abbreviations unless they are standard domain terminology.

---

# Module Organisation

Keep modules focused.

Avoid:

- large files
- `utils.rs` dumping grounds
- circular dependencies

Prefer clear module boundaries:

```
module/
├── mod.rs
├── parser.rs
├── model.rs
└── error.rs
```

when a module grows complex.

---

# Git Changes

Changes should be:

- small
- focused
- reviewable

Avoid mixing:

- refactors
- formatting changes
- feature changes

in the same commit.

---

# Pull Request Checklist

Before submitting:

- [ ] `cargo fmt` passes
- [ ] `cargo clippy` passes
- [ ] tests pass
- [ ] public APIs have documentation
- [ ] new behaviour has tests
- [ ] no unnecessary dependencies added
- [ ] no panics introduced
- [ ] no unnecessary allocations introduced

---

# Standards

The implementation should follow:

- MPEG-DASH specifications
- DASH-IF interoperability guidelines
- CMAF specifications where applicable

When behaviour differs between implementations, standards compliance takes precedence.

---

# Design Philosophy

Prefer:

- explicit over implicit
- simple over clever
- typed over string-based
- immutable over mutable
- composition over inheritance
- compile-time guarantees over runtime checks

Write Rust, not JavaScript translated into Rust.
