//! Version-pin resolution shared by the `jdk` CLI and the shim: selector
//! parsing, pin-file cascade, store paths and the exit-code contract.
//!
//! This crate is the shim's dependency firewall: std only, filesystem I/O at
//! most, nothing heavier than what one shim invocation needs.
//!
//! # No API stability guarantee
//!
//! This crate is an implementation detail of the `jdk` CLI, published only so
//! `cargo install jdk` can resolve it. Every item is free to change or
//! disappear in any release, including a patch one — the version number tracks
//! the CLI, not this API. Depend on it and expect breakage.

pub mod cascade;
pub mod config;
pub mod exit;
pub mod pin;
pub mod selector;
pub mod store;
mod text;
pub mod version;
