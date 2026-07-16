//! BOLA adaptive bitrate algorithm and factory.

mod algorithm;
mod factory;

pub use algorithm::{
    Bola, BolaDecision, BolaParams, DEFAULT_BUFFER_MAX_S, DEFAULT_SEGMENT_DURATION_S, QualityLevel,
};
pub use factory::BolaAbrFactory;
