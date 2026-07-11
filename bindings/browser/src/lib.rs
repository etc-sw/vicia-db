//! Browser package shim for Vicia DB.
//!
//! This is the durable package boundary used by local Vetch development and,
//! later, the public `@vicia-db/browser` release. The core crate still carries
//! its compatibility package name (`minigraf`); consumers only see Vicia.

pub use vicia_db::*;
