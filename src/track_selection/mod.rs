//! Deterministic selection and description of DASH adaptation-set tracks.

pub(crate) mod descriptors;
mod info;
mod kind;
mod select;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use info::TrackInfo;
pub use kind::{TrackDescriptor, TrackKind, TrackPreference, TrackSelection};
pub(crate) use select::{SelectedAdaptationSet, select_adaptation_sets};
