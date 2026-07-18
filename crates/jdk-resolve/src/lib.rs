//! Version-pin resolution shared by the `jdk` CLI and the shim: selector
//! parsing, pin-file cascade, store paths and the exit-code contract.
//!
//! This crate is the shim's dependency firewall: std only, filesystem I/O at
//! most, nothing heavier than what one shim invocation needs.

pub mod cascade;
pub mod config;
pub mod exit;
pub mod pin;
pub mod selector;
pub mod store;
pub mod version;
