//! Internal allocation prelude: the single place the heap types' origin is
//! named. Glob-imported (`use crate::prelude::*;`) by every module that
//! allocates. `alloc` is always linked (std implies it), so no std/alloc split
//! is needed here. The `prelude` path segment exempts the glob from the
//! workspace `wildcard_imports` deny.
#[cfg(feature = "alloc")]
pub(crate) use alloc::vec;
#[cfg(feature = "alloc")]
pub(crate) use alloc::vec::Vec;
