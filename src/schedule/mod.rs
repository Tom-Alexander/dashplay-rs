//! Segment scheduling and fetch orchestration.

mod adaptation_stream;
mod segment_decrypt;
mod segment_emit;
mod segment_fetch;

pub(crate) use adaptation_stream::{AdaptationStreamContext, run_adaptation_stream};
