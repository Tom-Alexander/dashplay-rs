//! DASH-IF IOP validation derived from the DASH-IF Conformance Software schematron rules.
//!
//! Source: `tests/conformance/schematron/schematron.sch` (DASH-IF Conformance-Software).

pub mod iop_validate;

pub use iop_validate::validate_iop;
