mod tree_walker;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use zz_frontend::ast::Program;
use zz_frontend::span::Span;

use crate::env::Env;
use crate::runtime::Flow;
use crate::value::{FuncValue, Value};

pub use crate::runtime::{EvalError, NativeEntry, NativeFn, RuntimeState};

pub struct Interp {
    pub env: Rc<RefCell<Env>>,
    pub funcs: HashMap<String, FuncValue>,
    /// Native registry, reference-counted: registrations happen at load
    /// time, after which the map is effectively frozen and freely shared
    /// with worker threads (`task.spawn` clones the `Arc`, not the 400+
    /// entry map). Late mutation (REPL imports) uses copy-on-write via
    /// [`Arc::make_mut`].
    pub natives: Arc<HashMap<String, NativeEntry>>,
    /// Struct layouts, reference-counted for copy-on-write sharing with
    /// worker threads (`task.spawn` clones the `Rc`, not the map) and
    /// server snapshots. Struct definitions are rare after load, so
    /// writes use `Rc::make_mut` (clones only on actual sharing) while
    /// every spawn/read pays a single atomic inc — or nothing at all.
    pub structs: Arc<HashMap<String, Vec<String>>>,
    pub args: Vec<String>,
    pub defer_stacks: Vec<Vec<Value>>,
    /// Mutation counter for [`Interp::funcs`], bumped on every insert.
    /// `task.spawn` caches a detached snapshot of the function table and
    /// reuses it while this version is unchanged, so spawn cost in a loop
    /// drops from a full re-snapshot to a detach-clone. Runtime-defined
    /// functions (`MakeFunc`) bump the version and transparently
    /// invalidate the cache.
    pub funcs_version: u64,
    /// Detached snapshot of [`Interp::funcs`] at [`Interp::funcs_version`],
    /// populated on first spawn. Never mutated after insert; workers each
    /// receive a fresh detach-clone, so no state is shared across threads.
    pub spawn_funcs_cache: Option<(u64, HashMap<String, FuncValue>)>,
    /// Cached keep-set for worker env snapshots (see
    /// [`SpawnKeepCache`](crate::value::SpawnKeepCache)): the *set* of
    /// visible names rarely changes between spawns from one site (loop
    /// iterations reuse the same scopes), while *values* always do — so the
    /// filter decision is cached but every kept value is cloned fresh per
    /// spawn.
    pub spawn_keep_cache: crate::value::SpawnKeepCache,
    /// Reachability cache per spawn-site chunk (see
    /// [`ReachCacheEntry`](crate::value::ReachCacheEntry)): loop spawns
    /// reuse one chunk `Arc`, so steady state skips the op walk +
    /// per-candidate table scan (~1µs/spawn). Cleared on table version
    /// change or past 64 sites.
    pub reach_cache: HashMap<usize, crate::value::ReachCacheEntry>,
    /// Green-thread task mode: this interpreter belongs to an executor task.
    /// Blocking natives yield instead of parking, and interpreted
    /// (tree-walker) calls are rejected — interpreter frames live on the
    /// Rust call stack and cannot be suspended. Compiled code is unaffected
    /// (every function has a chunk in the unified pipeline).
    pub task_mode: bool,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives: Arc::new(HashMap::new()),
            structs: Arc::new(HashMap::new()),
            args: Vec::new(),
            defer_stacks: Vec::new(),
            funcs_version: 0,
            spawn_funcs_cache: None,
            spawn_keep_cache: None,
            reach_cache: HashMap::new(),
            task_mode: false,
        }
    }

    pub fn with_natives(natives: HashMap<String, NativeEntry>) -> Self {
        Interp {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives: Arc::new(natives),
            structs: Arc::new(HashMap::new()),
            args: Vec::new(),
            defer_stacks: Vec::new(),
            funcs_version: 0,
            spawn_funcs_cache: None,
            spawn_keep_cache: None,
            reach_cache: HashMap::new(),
            task_mode: false,
        }
    }

    /// [`with_natives`](Self::with_natives) without copying the registry:
    /// the worker shares the parent's map. Sound because registrations
    /// only happen at load time (see field docs).
    pub fn with_natives_shared(natives: Arc<HashMap<String, NativeEntry>>) -> Self {
        Interp {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives,
            structs: Arc::new(HashMap::new()),
            args: Vec::new(),
            defer_stacks: Vec::new(),
            funcs_version: 0,
            spawn_funcs_cache: None,
            spawn_keep_cache: None,
            reach_cache: HashMap::new(),
            task_mode: false,
        }
    }

    /// Compile and execute a raw AST program (no type information).
    pub fn run(&mut self, program: &Program) -> Result<Value, EvalError> {
        let chunk = Arc::new(crate::vm::Compiler::compile_program(program));
        let mut vm = crate::vm::Vm::new();
        match vm.run_chunk(&chunk, self)? {
            Flow::Value(v) => Ok(v),
            Flow::Return(_) => Err(EvalError::new(
                "`return` outside of a function",
                Span::new(0, 0),
            )),
            Flow::Break(span) => Err(EvalError::new("`break` outside of a loop", span)),
            Flow::Continue(span) => Err(EvalError::new("`continue` outside of a loop", span)),
            // Main-thread entry points never yield (no executor TLS); a
            // Yield here means a task escaped its executor — loud bug.
            Flow::Yield(_) => Err(EvalError::new(
                "internal error: green-thread yield escaped its executor",
                Span::new(0, 0),
            )),
        }
    }

    /// Compile and execute a typed program (HIR).
    ///
    /// Uses the same bytecode compiler as [`run`] but threads the resolved
    /// type map and struct definitions from the HIR into the compiler. This
    /// is the unified pipeline entry point consumed by `zz run`.
    pub fn run_typed(
        &mut self,
        program: &Program,
        types: Arc<HashMap<Span, zz_checker::Type>>,
        structs: HashMap<String, zz_checker::StructSig>,
    ) -> Result<Value, EvalError> {
        let native_names: Arc<std::collections::HashSet<String>> =
            Arc::new(self.natives.keys().cloned().collect());
        let chunk = Arc::new(crate::vm::Compiler::compile_program_typed(
            program,
            types,
            structs,
            native_names,
        ));
        let mut vm = crate::vm::Vm::new();
        match vm.run_chunk(&chunk, self)? {
            Flow::Value(v) => Ok(v),
            Flow::Return(_) => Err(EvalError::new(
                "`return` outside of a function",
                Span::new(0, 0),
            )),
            Flow::Break(span) => Err(EvalError::new("`break` outside of a loop", span)),
            Flow::Continue(span) => Err(EvalError::new("`continue` outside of a loop", span)),
            Flow::Yield(_) => Err(EvalError::new(
                "internal error: green-thread yield escaped its executor",
                Span::new(0, 0),
            )),
        }
    }

    pub fn run_tree_walker(&mut self, program: &Program) -> Result<Value, EvalError> {
        let mut result = Value::Unit;
        for stmt in &program.stmts {
            result = self.run_stmt(stmt)?.into_value()?;
        }
        Ok(result)
    }
}
