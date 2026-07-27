//! # wake_ecma_minify — JavaScript Minifier
//!
//! Progressive minification passes for the wake bundler.
//!
//! ## Architecture
//!
//! All passes produce **side-tables** consumed by `wake_ecma_codegen` at emit time.
//! The AST itself is never mutated (it lives in a frozen bump arena).
//!
//! ## Passes
//!
//! | Module | Phase | Description |
//! |--------|-------|-------------|
//! | [`mangle`] | ✅ M4 | Scope-safe identifier renaming (a→b, c→d) |
//! | [`const_eval`] | 🏗️ 2.1 | Compile-time expression evaluation engine |
//! | [`purity`] | 🏗️ 1.3 | Function purity / side-effect analysis |
//! | [`analyze`] | 🏗️ 2.3 | Variable usage (reference counting, scope tracking) |
//! | [`simplify`] | 🏗️ 2.1 | Expression simplification planner |
//! | [`ctx`] | 🏗️ 4.1 | Aggregated minification context |

pub mod analyze;
pub mod const_eval;
pub mod ctx;
pub mod dce;
pub mod hoist;
pub mod mangle;
pub mod prop_mangle;
pub mod purity;
pub mod simplify;
pub mod statements;

pub use analyze::*;
pub use const_eval::*;
pub use ctx::*;
pub use dce::*;
pub use hoist::*;
pub use mangle::*;
pub use prop_mangle::*;
pub use purity::*;
pub use simplify::*;
pub use statements::*;

#[cfg(test)]
mod tests;
