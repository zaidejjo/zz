//! Phase 6: bytecode compiler and stack-based virtual machine.
//!
//! The compiler lowers the AST into a flat sequence of [`Op`] instructions
//! stored in a [`Chunk`]. The [`Vm`] executes chunks on a shared value stack
//! with an explicit call-frame stack, so function calls do not recurse in
//! Rust.
//!
//! Native bytecode covers the full language: literals, variables, paths,
//! arithmetic, logical operators, calls (incl. method resolution), `if`,
//! blocks, fmt strings, declarations, assignments (incl. index/field
//! write-back), `return`, functions, structs, `for`/`while` loops with
//! `break`/`continue`, arrays, dicts, indexing, slicing, ranges, closures,
//! variants, `match`, `if let`, and `?`.
//!
//! The Phase 1 tree-walker survives only behind `Interp::run_tree_walker`,
//! used by differential tests to cross-check the VM.

pub mod capture;
pub mod chunk;
pub mod compiler;
pub mod op;
pub mod runtime;

#[cfg(test)]
mod tests;

pub use chunk::Chunk;
pub use compiler::Compiler;
pub use op::Op;
pub use runtime::Vm;
