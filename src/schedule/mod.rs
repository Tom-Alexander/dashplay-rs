//! Segment scheduling and fetch orchestration.

mod adaptation_stream;
mod buffer_target;
mod segment_decrypt;
mod segment_emit;
mod segment_fetch;
mod segment_plan;

pub(crate) use adaptation_stream::{AdaptationStreamContext, run_adaptation_stream};
pub(crate) use buffer_target::BufferTarget;
