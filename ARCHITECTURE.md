# Architecture

`dashplayrs` is a pure Rust implementation of an MPEG-DASH player.

Its architecture is heavily inspired by the concepts used in `dash.js`, but it is **not** a port of the JavaScript implementation.

Instead, the project reinterprets those concepts using idiomatic Rust principles:

- explicit ownership
- immutable data
- composition over inheritance
- strong typing
- enums instead of stringly-typed state
- async only where required
- minimal runtime assumptions

The goal is to provide a modular playback pipeline that can be embedded into native applications, servers, WebAssembly environments, or custom media frameworks.

---

# Design Goals

The architecture prioritizes:

1. Correctness
2. Standards compliance
3. Predictable behaviour
4. Modularity
5. Testability
6. Performance

Every component should have a single, well-defined responsibility.

The library should avoid hidden behaviour and implicit state changes.

---

# High-Level Pipeline

The player consists of a sequence of independent stages.

```text
                   MPD
                    │
                    ▼
             Manifest Parser
                    │
                    ▼
             Manifest Model
                    │
                    ▼
          Manifest Processor
                    │
                    ▼
          Media Selection
                    │
                    ▼
         Adaptation Logic (ABR)
                    │
                    ▼
          Segment Scheduler
                    │
                    ▼
         Segment Resolution
                    │
                    ▼
            HTTP Download
                    │
                    ▼
          Segment Parser
                    │
                    ▼
          Sample Output
```

Each stage owns only the responsibilities required for that stage.

Communication between stages occurs through strongly typed values rather than shared mutable state.

---

# Core Components

## Manifest Parser

Responsible for reading MPD XML.

Responsibilities:

- parse XML
- validate syntax
- resolve namespaces
- parse timing information
- build immutable Rust structures

The parser should perform minimal interpretation.

It should not contain playback logic.

---

## Manifest Model

The parsed MPD is represented as immutable data.

Example hierarchy:

```text
Manifest
 ├── Period
 │     ├── AdaptationSet
 │     │      ├── Representation
 │     │      └── Representation
 │     └── EventStream
 └── Timing
```

These types should closely represent the DASH specification.

Derived playback information belongs elsewhere.

---

## Manifest Processor

The processor converts specification-oriented structures into playback-oriented structures.

Examples:

- inherit attributes
- resolve BaseURLs
- flatten SegmentTemplate inheritance
- compute effective timelines
- calculate presentation timing

After processing, downstream components should not need to understand MPD inheritance rules.

---

## Timeline Engine

The timeline engine converts DASH timing information into presentation timestamps.

Responsibilities include:

- SegmentTimeline expansion
- live edge calculations
- availability windows
- period transitions
- segment numbering

This component contains the majority of DASH timing logic.

---

## Track Selection

Track selection determines which AdaptationSets and Representations are active.

Selection is based on:

- codecs
- language
- roles
- accessibility
- user preference

Track selection should be deterministic.

---

## Adaptive Bitrate (ABR)

The ABR subsystem determines which Representation should be downloaded.

Inputs include:

- measured throughput
- buffer occupancy
- playback rate
- dropped frames
- user constraints

ABR should be extensible.

Multiple decision strategies should be supported.

Example:

```text
Throughput
      │
Buffer Occupancy
      │
Dropped Frames
      │
Playback Rate
      │
      ▼
Representation Selection
```

ABR should never perform downloads itself.

---

## Scheduler

The scheduler determines **what should be downloaded next**.

Responsibilities:

- maintain buffer targets
- determine required segments
- avoid duplicate downloads
- handle seeks
- recover from errors
- coordinate initialization segments

The scheduler does not perform HTTP requests.

Instead it produces download requests.

---

## Segment Resolver

The resolver converts logical segment identifiers into concrete URLs.

Responsibilities include:

- SegmentTemplate
- SegmentList
- SegmentBase
- BaseURL resolution
- template expansion

The scheduler should never construct URLs directly.

---

## Networking

Networking is intentionally abstract.

The player should not depend on a specific HTTP library.

Instead, networking is provided through a client interface.

This allows users to integrate:

- reqwest
- hyper
- surf
- browser fetch
- embedded networking stacks

Retry logic belongs here.

---

## Segment Parser

Segment parsing converts downloaded media into media samples.

Depending on the container, this may include:

- ISOBMFF
- CMAF
- future container formats

Parsing should avoid unnecessary allocations.

---

## Metrics

Metrics collect playback information.

Examples:

- throughput
- download time
- buffer level
- startup delay
- rebuffer events
- bitrate switches

Metrics should not influence playback directly.

Instead they provide data to components such as ABR.

---

## Player Controller

The controller coordinates the pipeline.

Responsibilities include:

- playback lifecycle
- state transitions
- external API
- event forwarding

The controller should contain very little playback logic itself.

Instead it orchestrates other components.

---

# Ownership Model

Ownership should flow in one direction.

```text
Player
 ├── Scheduler
 ├── ABR
 ├── Timeline
 ├── Network
 └── Metrics
```

Avoid cyclic ownership.

Avoid global state.

Avoid shared mutable data wherever practical.

---

# State Machines

Every significant subsystem should expose explicit state.

For example:

```rust
enum PlaybackState {
    Idle,
    LoadingManifest,
    Buffering,
    Playing,
    Seeking,
    Ended,
    Error,
}
```

Enums are preferred over boolean flags.

Invalid state combinations should be impossible to represent.

---

# Async Model

Only I/O is asynchronous.

Parsing remains synchronous.

Scheduling remains synchronous.

ABR remains synchronous.

This separation makes components easier to test.

The library never owns the async runtime.

---

# Error Handling

Errors should propagate naturally through the pipeline.

Each subsystem should define its own error type.

Top-level APIs should expose meaningful errors without leaking implementation details.

Panics are considered bugs.

---

# Concurrency

Concurrency is explicit.

The library does not spawn hidden background tasks.

- `MediaPlayer::start` prepares playback state only. Call `PlayerOutputs::run` on the current
  async task, or `PlayerOutputs::spawn` when a separate Tokio task is desired.
- The stream controller fetches audio and video adaptation sets concurrently via cooperative
  `join` within the caller's task — no additional spawned tasks.
- `Player::start_tracks` is the high-level convenience API: it calls `PlayerOutputs::spawn` and
  returns the resulting `JoinHandle` as `join`.

If parallelism beyond cooperative async is desired, it should be visible to the caller.

---

# Event Model

Rather than an EventBus, the player emits typed events.

Example:

```rust
enum PlayerEvent {
    ManifestLoaded,
    BufferUpdated,
    BitrateChanged,
    PlaybackStarted,
    PlaybackEnded,
    Error(PlayerError),
}
```

This avoids string-based event names.

---

# Mapping from dash.js

Many concepts correspond directly.

| dash.js | dashplayrs |
|----------|------------|
| MediaPlayer | Player |
| StreamProcessor | Scheduler + Timeline |
| DashAdapter | Manifest Processor |
| DashMetrics | Metrics |
| RulesController | ABR |
| ScheduleController | Scheduler |
| FragmentLoader | Network Client |
| FragmentModel | Scheduler State |
| EventBus | Typed Events |
| FactoryMaker | Constructors / Builders |

The architecture intentionally avoids reproducing JavaScript inheritance.

---

# Memory Strategy

Large media buffers should avoid copying.

Preferred types include:

- `Bytes`
- `Arc<[u8]>`
- slices

Avoid cloning downloaded segments.

Manifest structures should be immutable after parsing.

---

# Testing Strategy

Testing occurs at several layers.

## Unit Tests

Individual algorithms.

Examples:

- SegmentTemplate expansion
- timeline calculations
- URL resolution

## Integration Tests

Entire playback pipeline.

Examples:

- VOD playback
- live playback
- seeking
- adaptation

## Conformance Tests

Use DASH-IF test vectors whenever possible.

Bug reports should result in regression tests.

---

# Future Extensions

The architecture is intended to support future features without major redesign.

Examples include:

- Low-Latency DASH
- DRM (Widevine, PlayReady, FairPlay)
- CMCD
- CMSD
- Multi-period playback
- Trick-play tracks
- Thumbnail tracks
- Analytics
- Custom ABR algorithms

These features should be implemented by extending existing components rather than introducing parallel architectures.

---

# Guiding Principles

When making architectural decisions, prefer:

- explicitness over magic
- composition over inheritance
- immutable data over mutable state
- compile-time guarantees over runtime checks
- simple abstractions over generic frameworks
- predictable APIs over clever implementations

If there is a conflict between copying `dash.js` and writing idiomatic Rust, choose the idiomatic Rust solution.
