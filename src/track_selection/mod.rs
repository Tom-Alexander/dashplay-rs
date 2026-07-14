//! Deterministic selection and description of DASH adaptation-set tracks.

pub(crate) mod descriptors;
mod info;
mod kind;
mod preselection;
mod select;
mod sub_representation;
pub(crate) mod switching;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use info::TrackInfo;
pub use kind::{TrackDescriptor, TrackKind, TrackPreference, TrackSelection};
pub(crate) use select::{SelectedAdaptationSet, select_adaptation_sets};
pub use sub_representation::SubTrackInfo;
