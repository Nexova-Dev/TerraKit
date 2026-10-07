//! TerraKit CLI library.
//!
//! Everything except argument parsing lives here, and the binary is a thin shell
//! over it. That split is what makes the query logic and the normalizer testable
//! in isolation, and it means the cross-SDK conformance suite can call the same
//! `search()` the CLI does rather than shelling out to it and parsing stdout.

pub mod bundle;
pub mod cache;
pub mod commands;
pub mod download;
pub mod exit;
pub mod github;
pub mod install;
pub mod normalize;
pub mod package;
pub mod query;
pub mod registry;
pub mod signature;
pub mod verify;
