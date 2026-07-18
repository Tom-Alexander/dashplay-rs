//! Widevine CDM (Content Decryption Module) implementation.
//!
//! Vendored from [widevine-rs](https://codeberg.org/ThetaDev/widevine-rs) (GPL-3.0).

#![allow(
    clippy::upper_case_acronyms,
    non_camel_case_types,
    clippy::too_many_lines
)]

mod device;
mod error;
mod key;
mod pssh;
mod session;

pub use device::Device;
pub use error::Error;
pub use key::{Key, KeySet, KeyType};
pub use pssh::Pssh;
pub use session::{Cdm, CdmLicenseRequest, CdmSession, LicenseType, ServiceCertificate};
