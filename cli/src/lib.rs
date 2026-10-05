//! TerraKit CLI library.
//!
//! Everything except argument parsing lives here, and the binary is a thin shell
//! over it. That split is what makes the query logic and the normalizer testable
//! in isolation, and it means the cross-SDK conformance suite can call the same
//! `search()` the CLI does rather than shelling out to it and parsing stdout.

pub mod commands;
pub mod exit;
pub mod normalize;
pub mod package;
pub mod query;
pub mod verify;
