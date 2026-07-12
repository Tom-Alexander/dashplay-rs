//! Segment scheduling and fetch orchestration.

mod adaptation_stream;
mod fetch;

pub(crate) use adaptation_stream::{AdaptationStreamContext, run_adaptation_stream};
