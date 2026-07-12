//! Manifest processing: inheritance, timeline expansion, and URL resolution.

mod addressing;
mod alignment;
mod availability;
mod base_url;
mod end_numbers;
mod fetch;
mod inheritance;
mod period;
mod sidx;
mod template;
mod timeline;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use addressing::*;
pub(crate) use alignment::*;
pub(crate) use availability::*;
pub(crate) use base_url::*;
pub(crate) use end_numbers::*;
pub(crate) use fetch::*;
pub(crate) use period::*;
pub(crate) use sidx::*;
pub(crate) use template::*;
pub(crate) use timeline::*;
pub(crate) use types::*;
