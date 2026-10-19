use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::fs_util::atomic_write_private_with_parent as atomic_write;
use crate::model_id;

pub(crate) const GEMINI_BASE_INSTRUCTIONS: &str =
    include_str!("../resources/gemini-antigravity-base-instructions.md");

pub(crate) fn is_gemini_upstream_model(upstream_model: &str) -> bool {
    let name = upstream_model.trim().rsplit('/').next().unwrap_or_default();
    name.eq_ignore_ascii_case("gemini")
        || name
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("gemini-"))
}

mod cli_probe;
mod gating;
mod normalize;
mod store;
mod sync;

use cli_probe::*;
pub(crate) use gating::*;
pub use normalize::*;
pub use store::*;
pub(crate) use sync::*;

#[cfg(test)]
mod tests;
