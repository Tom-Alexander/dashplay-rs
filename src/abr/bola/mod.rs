//! BOLA adaptive bitrate algorithm and factory.

mod algorithm;
mod factory;

pub use algorithm::{Bola, BolaDecision, QualityLevel};
pub use factory::BolaAbrFactory;
