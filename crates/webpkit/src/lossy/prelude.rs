//! Internal allocation prelude: the single place the heap types' origin is
//! named, glob-imported (`use crate::lossy::prelude::*;`) by every module that
//! allocates. The `prelude` path segment exempts the glob from the workspace
//! `wildcard_imports` deny.
#[cfg(feature = "alloc")]
pub(crate) use alloc::{boxed::Box, vec, vec::Vec};
