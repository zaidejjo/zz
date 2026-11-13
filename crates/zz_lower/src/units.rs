//! Per-module translation units for parallel native builds.
//!
//! Same lowering as [`super::Lowerer::lower`], but partitioned by module
//! namespace so each module compiles as its own C translation unit and
//! links against the precompiled runtime archive. Definitions and
//! prototypes use external linkage ([`Lowerer::link_kw`]); the single-TU
//! path is untouched.
//!
//! Partition key: first segment of the dotted function name
//! (`walker.walk_files` → `walker`); bare names land in `_prelude`.
//! Top-level init statements keep merged program order via per-module
//! `zz_init_<ns>()` functions called from `zz_main` in encounter order.

use std::collections::{HashMap, HashSet};

use zz_codegen_runtime::{RUNTIME_C, RUNTIME_H};
use zz_frontend::ast::{Expr, Stmt};

use super::context::NameCtx;
use super::{mangle, Lowerer};

/// Prelude bucket for bare (dotless) names.
pub const PRELUDE_NS: &str = "_prelude";

/// One module's translation unit.
///
/// `header` + `code` is the exact source compiled for this unit and the
/// exact input hashed into its object-cache key, so a unit only misses the
/// cache when something it can actually reach changed. Stdlib units omit
/// program-only declarations (user prototypes, globals, the init driver),
/// which is what keeps their objects reusable across unrelated edits and
/// across projects.
#[derive(Debug)]
pub struct CodeUnit {
    /// Module namespace (or `_prelude`).
    pub ns: String,
    /// Declarations this unit compiles against (runtime + the reachable
    /// subset of program/stdlib declarations).
    pub header: String,
    /// C code: owned global defs + function bodies + closure defs +
    /// `zz_init_<ns>()` (+ `zz_main` glue when `ns` is the entry).
    pub code: String,
}

/// Partitioned lowering output.
#[derive(Debug)]
pub struct LoweredUnits {
    /// Header of the entry unit: the superset (runtime, every struct,
    /// reachable prototypes, globals, init driver and closure
    /// declarations). Kept for dumps and structural tests; the compiler
    /// uses each [`CodeUnit::header`] so units stay narrowly keyed.
    pub header: String,
    /// One unit per namespace with reachable code, encounter order.
    pub units: Vec<CodeUnit>,
    /// Entry namespace (owns `zz_main` / `zz_call_main`).
    pub entry_ns: String,
    /// Link gates, mirroring [`super::LoweredC`].
    pub needs_native_rt: bool,
    pub needs_pg_link: bool,
    pub needs_float_fmt: bool,
    pub needs_curl: bool,
    pub needs_sqlite: bool,
    /// Reachable stdlib natives with no C runtime implementation across
    /// all units (#715).
    pub unimplemented_natives: Vec<String>,
}

impl LoweredUnits {
    /// Fold the link gates discovered while lowering into `opts`.
    ///
    /// Same contract as [`super::LoweredC::apply_link_needs`]; both share one
    /// implementation so the per-unit build cannot drift from the single-TU
    /// build.
    pub fn apply_link_needs(&self, opts: &mut zz_codegen_types::BuildOptions) {
        eprintln!(
            "ZZ_DEBUG_LINK_NEEDS: native_rt={} float_fmt={}",
            self.needs_native_rt, self.needs_float_fmt
        );
        super::merge_link_needs(
            opts,
            self.needs_native_rt,
            self.needs_pg_link,
            self.needs_float_fmt,
            self.needs_curl,
            self.needs_sqlite,
        );
    }
}

/// Namespace of a dotted function name (first segment, `_prelude` if bare).
pub fn ns_of(dotted: &str) -> &str {
    match dotted.split('.').next() {
        Some(seg) if !seg.is_empty() && dotted.contains('.') => seg,
        _ => PRELUDE_NS,
    }
}

impl Lowerer {
    /// Lower to per-module units. See module docs for the contract.
    pub fn lower_units(&self, entry_ns: &str) -> LoweredUnits {
        self.unit_mode.set(true);
        let out = self.lower_units_inner(entry_ns);
        self.unit_mode.set(false);
        out
    }

    fn lower_units_inner(&self, entry_ns: &str) -> LoweredUnits {
        use zz_frontend::ast::Stmt::*;
        // Per-namespace buffers, encounter order.
        let mut order: Vec<String> = Vec::new();
        let mut fns: HashMap<String, String> = HashMap::new();
        let mut inits: HashMap<String, String> = HashMap::new();
        let mut closures: HashMap<String, Vec<String>> = HashMap::new();
        // Closure forward declarations, attributed to the unit that emitted
        // the literal (a closure is never referenced across units).
        let mut closures_fwd: HashMap<String, String> = HashMap::new();
        // Global name -> owning namespace (first declarer wins).
        let mut owners: HashMap<String, String> = HashMap::new();
        // Init-driver call order (namespace first appearance).
        let mut init_order: Vec<String> = Vec::new();
        // Namespaces that contain at least one program-defined symbol.
        // Everything else is stdlib-owned and gets the narrow header, so
        // user edits cannot invalidate its cached object.
        let mut program_ns: HashSet<String> = HashSet::new();
        // `None` outside the precompiled (AOT) path: header partitioning
        // only pays off there, and skipping the stdlib scan keeps unit
        // lowering in unit tests free of stdlib compilation.
        let stdlib_funcs: Option<&'static HashSet<String>> =
            self.precompiled.then(stdlib_func_names);
        let is_stdlib_fn = |fname: &str| stdlib_funcs.is_some_and(|s| s.contains(fname));
        // A namespace is package-owned when the loader tagged it `"pkg"`
        // (external ZZ dependency). Package symbols live in the dependency
        // header section so their units stay stable across unrelated
        // project edits and across projects sharing the same package set.
        let is_pkg_ns =
            |ns: &str| self.tp.module_origins.get(ns).map(|s| s.as_str()) == Some("pkg");
        let is_pkg_fn = |fname: &str| is_pkg_ns(ns_of(fname));

        // Hoisted cache (same data collect_globals computes, once per Lowerer).
        let mut names = NameCtx::new();
        self.seed_globals(&mut names);
        self.seed_scalar_fns(&mut names);
        let mut global_init_done: HashSet<String> = HashSet::new();
        let mut current_ns = PRELUDE_NS.to_string();

        for stmt in self.tp.stmts() {
            match stmt {
                Func {
                    name,
                    params,
                    body: b,
                    ..
                } => {
                    let fname = name.join(".");
                    if !self.reachable_funcs.contains(&fname) {
                        continue;
                    }
                    let ns = ns_of(&fname).to_string();
                    current_ns = ns.clone();
                    if !is_stdlib_fn(&fname) && !is_pkg_fn(&fname) {
                        program_ns.insert(ns.clone());
                    }
                    let before = self.closure_defs.borrow().len();
                    let before_fwd = self.closure_forward_decls.borrow().len();
                    let code = self.emit_function(&fname, params, b);
                    buf_for(&ns, &mut order, &mut fns).push_str(&code);
                    drain_closures(
                        closures.entry(ns.clone()).or_default(),
                        closures_fwd.entry(ns).or_default(),
                        self,
                        before,
                        before_fwd,
                    );
                }
                Impl { name, methods, .. } => {
                    let tname = name.join(".");
                    let ns = ns_of(&tname).to_string();
                    current_ns = ns.clone();
                    for m in methods {
                        if let Stmt::Func {
                            name: mname,
                            params,
                            body: b,
                            ..
                        } = m
                        {
                            let fname = format!("{tname}.{}", mname.join("."));
                            if !self.reachable_funcs.contains(&fname) {
                                continue;
                            }
                            if !is_stdlib_fn(&fname) && !is_pkg_fn(&fname) {
                                program_ns.insert(ns.clone());
                            }
                            let before = self.closure_defs.borrow().len();
                            let before_fwd = self.closure_forward_decls.borrow().len();
                            let code = self.emit_function(&fname, params, b);
                            buf_for(&ns, &mut order, &mut fns).push_str(&code);
                            drain_closures(
                                closures.entry(ns.clone()).or_default(),
                                closures_fwd.entry(ns.clone()).or_default(),
                                self,
                                before,
                                before_fwd,
                            );
                        }
                    }
                }
                Struct { .. } | TypeAlias { .. } | Enum { .. } | Import { .. } => {}
                Decl { name, value, .. } => {
                    let ns = current_ns.clone();
                    if !is_pkg_ns(&ns) {
                        program_ns.insert(ns.clone());
                    }
                    claim_owner(&mut owners, &name.name, &ns);
                    if !init_order.contains(&ns) {
                        init_order.push(ns.clone());
                    }
                    let before = self.closure_defs.borrow().len();
                    let before_fwd = self.closure_forward_decls.borrow().len();
                    let mut out = String::new();
                    emit_top_decl(
                        self,
                        &name.name,
                        value,
                        &mut names,
                        &mut out,
                        &mut global_init_done,
                    );
                    buf_for(&ns, &mut order, &mut inits).push_str(&out);
                    drain_closures(
                        closures.entry(ns.clone()).or_default(),
                        closures_fwd.entry(ns.clone()).or_default(),
                        self,
                        before,
                        before_fwd,
                    );
                }
                Destructure { pat, value, .. } => {
                    let ns = current_ns.clone();
                    if !is_pkg_ns(&ns) {
                        program_ns.insert(ns.clone());
                    }
                    for n in destructure_top_names(pat) {
                        claim_owner(&mut owners, &n, &ns);
                    }
                    if !init_order.contains(&ns) {
                        init_order.push(ns.clone());
                    }
                    let before = self.closure_defs.borrow().len();
                    let before_fwd = self.closure_forward_decls.borrow().len();
                    let mut out = String::new();
                    self.emit_destructure_global(
                        pat,
                        value,
                        &mut names,
                        &mut out,
                        &mut global_init_done,
                    );
                    buf_for(&ns, &mut order, &mut inits).push_str(&out);
                    drain_closures(
                        closures.entry(ns.clone()).or_default(),
                        closures_fwd.entry(ns.clone()).or_default(),
                        self,
                        before,
                        before_fwd,
                    );
                }
                other => {
                    let ns = current_ns.clone();
                    if !is_pkg_ns(&ns) {
                        program_ns.insert(ns.clone());
                    }
                    if !init_order.contains(&ns) {
                        init_order.push(ns.clone());
                    }
                    let before = self.closure_defs.borrow().len();
                    let before_fwd = self.closure_forward_decls.borrow().len();
                    let mut out = String::new();
                    self.emit_stmt(other, &mut names, &mut out, false);
                    buf_for(&ns, &mut order, &mut inits).push_str(&out);
                    drain_closures(
                        closures.entry(ns.clone()).or_default(),
                        closures_fwd.entry(ns.clone()).or_default(),
                        self,
                        before,
                        before_fwd,
                    );
                }
            }
        }

        // Synthetic `_len`/`_cap` globals belong to the same namespace as
        // their parent flat array, so their C definitions land in the same
        // translation unit.
        for (zz_name, _, _, _) in &self.global_list {
            if let Some(base) = zz_name
                .strip_suffix("_len")
                .or_else(|| zz_name.strip_suffix("_cap"))
            {
                if owners.contains_key(base) && !owners.contains_key(zz_name) {
                    owners.insert(zz_name.clone(), owners[base].clone());
                }
            }
        }

        // Selective-import bare aliases: forward to canonical (extern here).
        let mut alias_pairs: Vec<(&String, &String)> = self.import_fn_aliases.iter().collect();
        alias_pairs.sort();
        for (alias, canonical) in alias_pairs {
            if !self.reachable_funcs.contains(alias) {
                continue;
            }
            if self.find_func_def(alias).is_some() {
                continue;
            }
            if self.find_func_def(canonical).is_none() {
                continue;
            }
            if self.is_impl_method(canonical) {
                continue;
            }
            let ans = ns_of(alias).to_string();
            // The forwarder is program code even when it targets a stdlib
            // function: it exists only because this program imported it.
            program_ns.insert(ans.clone());
            buf_for(&ans, &mut order, &mut fns).push_str(&format!(
                "zz_value zz_fn_{}(zz_value *args, size_t argc) {{ return zz_fn_{}(args, argc); }}\n",
                mangle(alias),
                mangle(canonical)
            ));
        }

        // Declarations every unit needs. Struct layouts and the generated
        // debug helpers are program-wide and layout-sensitive: keep them in
        // the stable base so a body edit never invalidates a unit.
        let struct_preamble = self.lower_structs_preamble();
        let struct_debug_fns = self.lower_struct_debug_fns();
        let mut glob_ext = String::new();
        let mut glob_ext_dep = String::new();
        for (zz_name, cid, ctype, _) in &self.global_list {
            let _ = zz_name;
            let line = format!("extern {ctype} {cid};\n");
            let owner = owners
                .get(zz_name)
                .map(|o| o.as_str())
                .unwrap_or(PRELUDE_NS);
            if self.tp.module_origins.get(owner).map(|s| s.as_str()) == Some("pkg") {
                glob_ext_dep.push_str(&line);
            } else {
                glob_ext.push_str(&line);
            }
        }
        // Prototypes split by ownership. Stdlib prototypes are stable for
        // a given import set, so they join the base header; package
        // prototypes join the dependency section (shared by every unit);
        // program prototypes change with user code and stay out of stdlib
        // and package units.
        let mut stdlib_decls = String::new();
        let mut dep_decls = String::new();
        let mut program_decls = String::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Deterministic proto order: sorted (single-TU path keeps its
        // HashSet order for byte-identity; units are order-free).
        let mut rfuncs: Vec<&String> = self.reachable_funcs.iter().collect();
        rfuncs.sort();
        for fname in rfuncs {
            if !seen.insert(fname.clone()) {
                continue;
            }
            if self
                .tp
                .funcs
                .get(fname)
                .map(|sig| sig.is_extern)
                .unwrap_or(false)
            {
                continue;
            }
            let decls = if is_stdlib_fn(fname) {
                &mut stdlib_decls
            } else if is_pkg_fn(fname) {
                &mut dep_decls
            } else {
                &mut program_decls
            };
            let first_struct_c = self
                .tp
                .funcs
                .get(fname)
                .and_then(|sig| sig.params.first().map(|(_, t)| t.clone()))
                .filter(|t| matches!(t, zz_checker::Type::Struct(_, _)))
                .filter(|_| self.is_impl_method(fname))
                .map(|t| self.type_to_c(&t));
            match first_struct_c {
                Some(sct) => decls.push_str(&format!(
                    "zz_value zz_fn_{}({sct} *self, zz_value *args, size_t argc);\n",
                    mangle(fname)
                )),
                None => decls.push_str(&format!(
                    "zz_value zz_fn_{}(zz_value *args, size_t argc);\n",
                    mangle(fname)
                )),
            }
            if self.specialized.contains(fname) {
                if let Some(sig) = self.tp.funcs.get(fname) {
                    let ret = Self::scalar_ctype(&sig.ret).unwrap_or("zz_value");
                    let params: Vec<String> = sig
                        .params
                        .iter()
                        .map(|(_, t)| Self::scalar_ctype(t).unwrap_or("zz_value").to_string())
                        .collect();
                    let psig = if params.is_empty() {
                        "void".to_string()
                    } else {
                        params.join(", ")
                    };
                    decls.push_str(&format!("{ret} zz_fn_{}_u({psig});\n", mangle(fname)));
                }
            }
        }
        let extern_pre = self.extern_prelude();
        let extern_section = if extern_pre.is_empty() {
            String::new()
        } else {
            format!("\n// ---- plugin externs ----\n{extern_pre}\n")
        };
        let mut expanded_natives: HashSet<String> = self.reachable_natives.clone();
        for (alias, canonical) in &self.import_fn_aliases {
            if expanded_natives.contains(alias) {
                expanded_natives.insert(canonical.clone());
            }
        }
        let ffi_pre = zz_codegen_ffi::ffi_prelude(&expanded_natives);
        let ffi_section = if ffi_pre.is_empty() {
            String::new()
        } else {
            format!("\n// ---- native-runtime FFI ----\n{ffi_pre}\n")
        };
        let needs_native_rt = zz_codegen_ffi::needs_native_rt(&expanded_natives);
        let runtime_c = if self.precompiled {
            String::new()
        } else {
            RUNTIME_C.to_string()
        };
        // Every namespace with code gets a unit even without inits, so no
        // reachable body is dropped (init fns only for namespaces in
        // init_order — empty init is still emitted for link uniformity).
        let mut all_ns: Vec<String> = order.clone();
        for ns in &init_order {
            if !all_ns.contains(ns) {
                all_ns.push(ns.clone());
            }
        }
        // The entry unit always owns the `zz_main` driver (program glue),
        // even in the degenerate case where its namespace is stdlib-named.
        program_ns.insert(entry_ns.to_string());

        // Init forward declarations: only the entry unit's `zz_main` calls
        // them, so only the entry header carries them. Non-entry units keep
        // a smaller, more stable key.
        let init_fwd: String = all_ns
            .iter()
            .map(|ns| format!("void zz_init_{}(void);\n", mangle(ns)))
            .collect();

        // Process-global arenas used by unit code (e.g. collection array
        // initializers, per-function scratch). In single-TU mode they're
        // declared `static` in `globals_decl`/`emit_function`; in unit mode
        // they are `extern` here and defined once in the entry unit.
        //
        // Both arenas are initialized via a constructor: it runs before any
        // file-scope initializer that may bump-allocate (e.g. collection
        // modules calling `zz_array_new_arena_sized(&_func_arena, ...)`) and
        // before `zz_main` drives the per-module init bodies. Single-TU mode
        // initializes them at the top of `zz_main`; unit mode cannot, because
        // the arenas are process-global here and init bodies in every unit
        // reference them. A zeroed arena makes the first allocation take the
        // overflow path with `cap == 0`, overrunning the heap.
        let base_header = format!(
            "{runtime_h}\n{runtime_c}\n{ffi_section}\n{extern_section}// ---- struct definitions ----\n{struct_preamble}\n{struct_debug_fns}\n// ---- stdlib forward declarations ----\n{stdlib_decls}// ---- dependency forward declarations ----\n{dep_decls}{glob_ext_dep}extern zz_arena _static_arena;\nextern zz_arena _func_arena;\n",
            runtime_h = RUNTIME_H,
            runtime_c = runtime_c,
            ffi_section = ffi_section,
            extern_section = extern_section,
            struct_preamble = struct_preamble,
            struct_debug_fns = struct_debug_fns,
            stdlib_decls = stdlib_decls,
            dep_decls = dep_decls,
            glob_ext_dep = glob_ext_dep,
        );
        let program_header = format!(
            "// ---- module globals (extern) ----\n{glob_ext}// ---- program forward declarations ----\n{program_decls}"
        );
        // Per-unit header: stable base + (program-only declarations for
        // program units) + this unit's own closure declarations + the init
        // driver for the entry. `header` + `code` is the exact TU, so a
        // cache slug over it invalidates only what actually changed.
        let make_header = |ns: &str| -> String {
            let mut header = String::with_capacity(
                base_header.len() + program_header.len() + init_fwd.len() + 256,
            );
            header.push_str(&base_header);
            if program_ns.contains(ns) {
                header.push_str(&program_header);
            }
            if let Some(fwd) = closures_fwd.get(ns) {
                header.push_str(fwd);
            }
            if ns == entry_ns {
                header.push_str(&init_fwd);
            }
            header
        };

        // Assemble units: owned global defs + bodies + closures + init fn.
        let mut units: Vec<CodeUnit> = Vec::new();
        for ns in &all_ns {
            let mut code = String::new();
            for (zz_name, cid, ctype, _) in &self.global_list {
                if owners.get(zz_name).map(|o| o == ns).unwrap_or(false) {
                    code.push_str(&format!("{ctype} {cid};\n"));
                }
            }
            if let Some(b) = fns.get(ns) {
                code.push_str(b);
            }
            if let Some(defs) = closures.get(ns) {
                for d in defs {
                    code.push_str(d);
                    code.push('\n');
                }
            }
            let init_body = inits.get(ns).cloned().unwrap_or_default();
            code.push_str(&format!(
                "void zz_init_{mns}(void) {{\n{init_body}}}\n",
                mns = mangle(ns)
            ));
            units.push(CodeUnit {
                ns: ns.clone(),
                header: make_header(ns),
                code,
            });
        }

        // Entry glue: zz_main drives inits in encounter order.
        let mut driver = String::new();
        for ns in &init_order {
            driver.push_str(&format!("    zz_init_{}();\n", mangle(ns)));
        }
        let main_decl = if self.reachable_funcs.contains(&self.entry_main) {
            "return zz_call_into_main();".to_string()
        } else {
            String::new()
        };
        let main_tail = if self.reachable_funcs.contains(&self.entry_main) {
            String::new()
        } else {
            "    return 0;".to_string()
        };
        let entry_glue = format!(
            "void zz_main(void) {{\n{driver}}}\n\nint zz_call_main(void) {{\n    {main_decl}\n{main_tail}\n}}\n"
        );
        let with_main = if self.reachable_funcs.contains(&self.entry_main) {
            let m = format!("zz_fn_{}", mangle(&self.entry_main));
            let takes_argv = self
                .tp
                .funcs
                .get(&self.entry_main)
                .map(|sig| sig.params.len() == 1)
                .unwrap_or(false);
            if takes_argv {
                format!(
                    "\nstatic int zz_call_into_main(void);\nstatic int zz_call_into_main(void) {{ int _e = 0; zz_value _cli = zz_env_args(zz_unit(), &_e); zz_value _r = {m}(&_cli, 1); return zz_main_result_code(_r); }}\n"
                )
            } else {
                format!(
                    "\nstatic int zz_call_into_main(void);\nstatic int zz_call_into_main(void) {{ zz_value _r = {m}(NULL, 0); return zz_main_result_code(_r); }}\n"
                )
            }
        } else {
            String::new()
        };
        // with_main first: zz_call_main calls it (single-TU path splices
        // it before zz_call_main the same way).
        let entry_code = format!(
            "zz_arena _static_arena;\nzz_arena _func_arena;\n__attribute__((constructor)) static void zz_init_process_arenas(void) {{\n    zz_arena_init(&_static_arena, 65536);\n    zz_arena_init(&_func_arena, 65536);\n}}\n{}{}",
            with_main, entry_glue
        );
        match units.iter_mut().find(|u| u.ns == entry_ns) {
            Some(u) => {
                u.code.push_str(&entry_code);
            }
            None => units.push(CodeUnit {
                ns: entry_ns.to_string(),
                header: make_header(entry_ns),
                code: entry_code,
            }),
        }

        // Public header = the entry unit's (superset) header, preserving the
        // old "everything a unit could need" contract for dumps/tests.
        let header = units
            .iter()
            .find(|u| u.ns == entry_ns)
            .map(|u| u.header.clone())
            .unwrap_or_else(|| make_header(entry_ns));

        LoweredUnits {
            header,
            units,
            entry_ns: entry_ns.to_string(),
            needs_native_rt,
            needs_pg_link: zz_codegen_ffi::needs_pg_link(&expanded_natives),
            needs_float_fmt: super::needs_float_fmt(&self.tp, &self.reachable_funcs),
            needs_curl: zz_codegen_ffi::needs_curl_link(&expanded_natives),
            needs_sqlite: zz_codegen_ffi::needs_sqlite_link(&expanded_natives),
            unimplemented_natives: self.unimplemented_natives.borrow().clone(),
        }
    }
}

/// Per-namespace string buffer (registers namespace in encounter order).
/// Dedup is on the shared `order` (not the map): a namespace first seen
/// via inits and later via bodies must still produce exactly one unit —
/// per-map dedup emitted the module twice (duplicate definitions at link).
fn buf_for<'a>(
    ns: &str,
    order: &mut Vec<String>,
    map: &'a mut HashMap<String, String>,
) -> &'a mut String {
    if !order.contains(&ns.to_string()) {
        order.push(ns.to_string());
    }
    map.entry(ns.to_string()).or_default()
}

/// First declarer wins (mirrors merged-program semantics: one global).
fn claim_owner(owners: &mut HashMap<String, String>, name: &str, ns: &str) {
    owners
        .entry(name.to_string())
        .or_insert_with(|| ns.to_string());
}

/// Move closure bodies and closure forward declarations emitted since the
/// last drain into the unit that emitted the literal.
///
/// A closure is a value local to one function body, so it is never
/// referenced from another unit; keeping its declarations in that unit's
/// header lets unrelated units keep their object-cache keys.
fn drain_closures(
    defs: &mut Vec<String>,
    fwd: &mut String,
    lowerer: &Lowerer,
    before_defs: usize,
    before_fwd: usize,
) {
    defs.extend(lowerer.closure_defs.borrow_mut().split_off(before_defs));
    let fresh = lowerer
        .closure_forward_decls
        .borrow_mut()
        .split_off(before_fwd);
    fwd.push_str(&fresh.concat());
}

/// Function names defined by the embedded pure-ZZ stdlib.
///
/// Header partitioning must never classify a user module as stdlib (that
/// would omit a prototype its own unit needs), so ownership is derived from
/// the compiled stdlib ASTs — including `impl` methods like
/// `ArgsParser.parse` — instead of a name-prefix heuristic.
fn stdlib_func_names() -> &'static HashSet<String> {
    static NAMES: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = HashSet::new();
        for program in zz_stdlib::zz_stdlib_programs().iter().flatten() {
            for stmt in &program.program.stmts {
                match stmt {
                    Stmt::Func { name, .. } => {
                        names.insert(name.join("."));
                    }
                    Stmt::Impl { name, methods, .. } => {
                        let owner = name.join(".");
                        for method in methods {
                            if let Stmt::Func { name: mname, .. } = method {
                                names.insert(format!("{owner}.{}", mname.join(".")));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        names
    })
}

/// Top-level binding names in a destructure pattern.
fn destructure_top_names(pat: &zz_frontend::ast::Pattern) -> Vec<String> {
    use zz_frontend::ast::Pattern;
    fn walk(pat: &Pattern, into: &mut Vec<String>) {
        match pat {
            Pattern::Binding { name } => {
                if !into.contains(&name.name) {
                    into.push(name.name.clone());
                }
            }
            Pattern::Tuple { pats, .. } => {
                for p in pats {
                    walk(p, into);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(pat, &mut out);
    out
}

/// Top-level `x := rhs` emission (mirrors `lower()`'s Decl arm).
#[allow(clippy::too_many_arguments)]
fn emit_top_decl(
    lowerer: &Lowerer,
    zz_name: &str,
    value: &Expr,
    names: &mut NameCtx,
    out: &mut String,
    global_init_done: &mut HashSet<String>,
) {
    use zz_frontend::ast::Expr;
    let gid = Lowerer::global_cid(zz_name);
    let gtype = names
        .globals
        .get(zz_name)
        .map(|(_, t)| t.clone())
        .unwrap_or_else(|| "zz_value".to_string());
    // Flat array global: `arr: [int] = []` →
    //   zz_global_arr = NULL;
    //   zz_global_arr_len = 0;
    //   zz_global_arr_cap = 0;
    // Growth happens via inline flat-array push in Expr::Call
    // (malloc-based for globals; the buffer outlives frames).
    if Lowerer::is_flat_array_ctype(&gtype)
        && matches!(value, Expr::Array { elems, .. } if elems.is_empty())
    {
        out.push_str(&format!("    {gid} = NULL;\n"));
        out.push_str(&format!("    {gid}_len = 0;\n"));
        out.push_str(&format!("    {gid}_cap = 0;\n"));
        global_init_done.insert(zz_name.to_string());
        return;
    }
    if let Expr::StructInit {
        name: struct_name, ..
    } = value
    {
        if lowerer.is_unboxed_struct(struct_name) {
            let boxed = lowerer.emit_boxed_value(struct_name, value, names, out);
            out.push_str(&format!("    {gid} = {boxed};\n"));
            global_init_done.insert(zz_name.to_string());
            return;
        }
    }
    let val = lowerer.emit_expr(value, names, out);
    let val_is_unboxed = val.starts_with("(int64_t)(")
        || val.starts_with("(double)(")
        || val.starts_with("(bool)(")
        || names
            .globals
            .values()
            .any(|(id, t)| val == *id && matches!(t.as_str(), "int64_t" | "double" | "bool"));
    let final_val = match gtype.as_str() {
        "int64_t" if !val_is_unboxed => format!("({val}).i"),
        "double" if !val_is_unboxed => format!("({val}).f"),
        "bool" if !val_is_unboxed => format!("({val}).b"),
        _ => val,
    };
    let first_init = !global_init_done.contains(zz_name);
    if first_init {
        out.push_str(&format!("    {gid} = {final_val};\n"));
        global_init_done.insert(zz_name.to_string());
    } else if matches!(gtype.as_str(), "int64_t" | "double" | "bool") {
        out.push_str(&format!("    {gid} = {final_val};\n"));
    } else {
        out.push_str(&format!("    zz_assign(&{gid}, {final_val});\n"));
    }
    if let Expr::Array { elems, .. } = value {
        names.set_array_len(zz_name, elems.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_hir::TypedProgram;

    /// Merge the embedded pure-ZZ stdlib with a user program the way the
    /// AOT pipeline does (loader seeds stdlib signatures, HIR merge adds
    /// stdlib bodies, DCE prunes from `main`).
    fn lower_merged(src: &str) -> LoweredUnits {
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );

        let mut funcs = zz_stdlib::stdlib_funcs();
        let mut structs: HashMap<String, zz_checker::StructSig> = HashMap::new();
        let mut enums: HashMap<String, zz_checker::EnumSig> = HashMap::new();
        let stdlib: Vec<TypedProgram> = zz_stdlib::zz_stdlib_programs()
            .iter()
            .filter_map(|p| p.as_ref().ok().cloned())
            .collect();
        for program in &stdlib {
            for (name, sig) in &program.funcs {
                funcs.entry(name.clone()).or_insert_with(|| sig.clone());
            }
            for (name, sig) in &program.structs {
                structs.entry(name.clone()).or_insert_with(|| sig.clone());
            }
            for (name, sig) in &program.enums {
                enums.entry(name.clone()).or_insert_with(|| sig.clone());
            }
        }
        let user = zz_hir::build_program(
            &parsed.program,
            HashMap::new(),
            funcs,
            structs,
            HashMap::new(),
            enums,
        );
        assert!(
            !user
                .diagnostics
                .iter()
                .any(|d| d.severity == zz_frontend::diag::Severity::Error),
            "type errors: {:?}",
            user.diagnostics
        );
        let mut programs = stdlib;
        programs.push(user.program);
        let merged = zz_hir::merge_typed_programs(programs);
        let (pruned, reach) = zz_hir::dce(&merged, "main");
        let mut lowerer = Lowerer::new(
            reach.funcs.clone(),
            reach.natives.clone(),
            "main".to_string(),
            pruned,
        );
        lowerer.set_precompiled(true);
        lowerer.lower_units("main")
    }

    /// Like [`lower_merged`] but tags each namespace with an origin so the
    /// dependency/project header split can be exercised. `origins` maps
    /// namespace → `"pkg"` or `"project"`; absent namespaces count as
    /// project.
    fn lower_merged_origins(src: &str, origins: HashMap<String, String>) -> LoweredUnits {
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );

        let mut funcs = zz_stdlib::stdlib_funcs();
        let mut structs: HashMap<String, zz_checker::StructSig> = HashMap::new();
        let mut enums: HashMap<String, zz_checker::EnumSig> = HashMap::new();
        let stdlib: Vec<TypedProgram> = zz_stdlib::zz_stdlib_programs()
            .iter()
            .filter_map(|p| p.as_ref().ok().cloned())
            .collect();
        for program in &stdlib {
            for (name, sig) in &program.funcs {
                funcs.entry(name.clone()).or_insert_with(|| sig.clone());
            }
            for (name, sig) in &program.structs {
                structs.entry(name.clone()).or_insert_with(|| sig.clone());
            }
            for (name, sig) in &program.enums {
                enums.entry(name.clone()).or_insert_with(|| sig.clone());
            }
        }
        let user = zz_hir::build_program(
            &parsed.program,
            HashMap::new(),
            funcs,
            structs,
            HashMap::new(),
            enums,
        );
        assert!(
            !user
                .diagnostics
                .iter()
                .any(|d| d.severity == zz_frontend::diag::Severity::Error),
            "type errors: {:?}",
            user.diagnostics
        );
        let mut programs = stdlib;
        programs.push(user.program);
        let mut merged = zz_hir::merge_typed_programs(programs);
        merged.module_origins = origins;
        let (pruned, reach) = zz_hir::dce(&merged, "main");
        let mut lowerer = Lowerer::new(
            reach.funcs.clone(),
            reach.natives.clone(),
            "main".to_string(),
            pruned,
        );
        lowerer.set_precompiled(true);
        lowerer.lower_units("main")
    }

    fn unit<'a>(units: &'a LoweredUnits, ns: &str) -> &'a CodeUnit {
        units.units.iter().find(|u| u.ns == ns).unwrap_or_else(|| {
            panic!(
                "missing unit {ns}; have {:?}",
                units.units.iter().map(|u| &u.ns).collect::<Vec<_>>()
            )
        })
    }

    /// A `name := []` declaration whose pushes reveal a scalar element type
    /// through the self-append assignment (`name = vec.push(name, x)`) gets
    /// the inferred flat layout: contiguous `bool *` buffer, inline push, and
    /// a raw index store — no boxed `zz_vec_push_take` / `zz_index_set`.
    #[test]
    fn untyped_inferred_flat_bool_array_lowers_inline() {
        let units = lower_merged(
            "func main() {
                n := 100
                flags := []
                for i in 0..n {
                    flags = vec.push(flags, true)
                }
                flags[0] = false
                count := 0
                for i in 0..n {
                    if flags[i] { count = count + 1 }
                }
                println(count)
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        // Inferred flat declarations get a fresh C id, so assert on the shape
        // of the emitted code rather than the source-level name.
        assert!(
            code.contains("bool * ") && code.contains(" = NULL;"),
            "expected flat bool buffer:\n{code}"
        );
        assert!(
            code.contains("memset(") && code.contains("sizeof(bool)"),
            "constant fill must lower to a single memset:\n{code}"
        );
        assert!(
            code.matches("for (int64_t ").count() == 1,
            "the fill loop must be replaced by the memset:\n{code}"
        );
        assert!(
            !code.contains("zz_vec_push_take"),
            "push must not go through the boxed move path:\n{code}"
        );
        assert!(
            !code.contains("zz_index_set"),
            "index store must use the flat buffer:\n{code}"
        );
        assert!(
            code.contains("zz_flat_index(") || code.contains("zz_flat_index_nn("),
            "index reads/stores must use the flat buffer:\n{code}"
        );
    }

    /// The same inference for `name = vec.push(name, StructInit{...})` with an
    /// all-scalar struct: flat `zz_struct_Pt *` buffer and direct element
    /// field reads.
    #[test]
    fn untyped_inferred_flat_struct_array_lowers_inline() {
        let units = lower_merged(
            "struct Pt { x: int, y: int }
             func main() {
                 n := 100
                 pts := []
                 for i in 0..n {
                     pts = vec.push(pts, Pt{ x: i, y: i * 2 })
                 }
                 total := 0
                 for i in 0..n {
                     total = total + pts[i].x
                 }
                 println(total)
             }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            code.contains("zz_struct_Pt * ") && code.contains(" = NULL;"),
            "expected flat struct buffer:\n{code}"
        );
        assert!(
            !code.contains("zz_vec_push_take"),
            "push must not go through the boxed move path:\n{code}"
        );
        assert!(
            !code.contains("zz_object_get_field"),
            "field read must not go through the boxed object path:\n{code}"
        );
        assert!(
            code.contains("].x"),
            "field read must index the flat buffer:\n{code}"
        );
    }

    /// Control: a push argument with no flat representation (a string) keeps
    /// the array boxed. Loop variables and arithmetic expressions no longer
    /// disqualify a candidate: the checker's inferred type makes them scalars.
    #[test]
    fn untyped_inferred_array_with_string_element_stays_boxed() {
        let units = lower_merged(
            "func main() {
                n := 100
                arr := []
                for i in 0..n {
                    arr = vec.push(arr, \"x\")
                }
                println(len(arr))
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            code.contains("zz_vec_push_take"),
            "boxed self-push must keep the take path:\n{code}"
        );
        assert!(
            !code.contains("__flat_grow") && !code.contains("zz_flat_index("),
            "expected the boxed layout:\n{code}"
        );
    }

    /// A loop-variable element now resolves through the checker type, so the
    /// inferred flat layout applies to `arr = vec.push(arr, i)`.
    #[test]
    fn untyped_inferred_array_with_loop_element_is_flat() {
        let units = lower_merged(
            "func main() {
                n := 100
                arr := []
                for i in 0..n {
                    arr = vec.push(arr, i)
                }
                total := 0
                for i in 0..n { total = total + arr[i] }
                println(total)
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            code.contains("int64_t * ") && code.contains(" = NULL;"),
            "expected a flat int buffer:\n{code}"
        );
        assert!(
            !code.contains("zz_vec_push_take"),
            "push must stay inline:\n{code}"
        );
    }

    /// A body assignment to the loop variable writes a per-iteration copy in
    /// the VM, so lowering must not let it drive the C induction variable
    /// (`k = -1` would otherwise reset the counter and hang).
    #[test]
    fn for_loop_var_write_uses_hidden_counter() {
        let units = lower_merged(
            "func main() {
                total := 0
                for k in 0..3 {
                    k = 0 - 1
                    total = total + k
                }
                println(total)
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            code.contains("for (int64_t _lf"),
            "the counter must be a hidden induction variable:\n{code}"
        );
        assert!(
            code.lines()
                .any(|l| l.trim_start().starts_with("int64_t v") && l.contains("= _lf")),
            "the loop binding must be re-created inside the body:\n{code}"
        );
    }

    /// The hidden counter is only for bodies that assign the binding: an
    /// ordinary counted loop keeps the direct (unboxed) counter.
    #[test]
    fn for_loop_without_var_write_keeps_direct_counter() {
        let units = lower_merged(
            "func main() {
                total := 0
                for k in 0..3 { total = total + k }
                println(total)
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            !code.contains("_lf"),
            "a read-only loop variable must drive the counter directly:\n{code}"
        );
        assert!(
            code.contains("for (int64_t v"),
            "expected the direct counter header:\n{code}"
        );
    }

    /// Nested inferred arrays: `a := []` whose elements are flat scalar rows
    /// lowers to `zz_flat_row *`, and `a[i][j]` reads/stores index the row
    /// buffer directly instead of the boxed read-modify-write chain.
    #[test]
    fn untyped_inferred_flat_row_matrix_lowers_inline() {
        let units = lower_merged(
            "func main() {
                n := 8
                a := []
                for i in 0..n {
                    row := []
                    for j in 0..n {
                        row = vec.push(row, (i * n + j) % 1000)
                    }
                    a = vec.push(a, row)
                }
                total := 0
                for i in 0..n {
                    for j in 0..n {
                        total = total + a[i][j]
                    }
                }
                a[0][0] = 1
                println(total)
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            code.contains("zz_flat_row * ") && code.contains(" = NULL;"),
            "expected a flat row buffer:\n{code}"
        );
        assert!(
            code.contains("zz_flat_row "),
            "row snapshot temp missing:\n{code}"
        );
        assert!(
            !code.contains("zz_vec_push_take"),
            "pushes must not take the boxed path:\n{code}"
        );
        assert!(
            !code.contains("zz_index_get") && !code.contains("zz_index_set"),
            "nested reads/stores must stay raw:\n{code}"
        );
        assert!(
            code.matches("zz_flat_index").count() >= 4,
            "both bounds checks missing:\n{code}"
        );
    }

    /// The non-negative index fact is bound to the C id of the binding: an
    /// inner shadow must not mark the outer variable, which still needs the
    /// sign-folding check (a negative index counts from the end, it is not an
    /// out-of-bounds trap).
    #[test]
    fn flat_index_fast_path_survives_shadowing() {
        let units = lower_merged(
            "func main() {
                arr := []
                for i in 0..3 { arr = vec.push(arr, i) }
                j := -1
                {
                    j := 5
                    println(j)
                }
                println(arr[j])
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            !code.contains("zz_flat_index_nn("),
            "a shadowed binding must not inherit the non-negative fact:\n{code}"
        );
        assert!(
            code.contains("zz_flat_index("),
            "negative index needs the sign-folding check:\n{code}"
        );
    }

    /// A constant fill lowers to one `memset`; a constant without a
    /// byte-uniform representation (`5` in an int array) keeps the loop.
    #[test]
    fn flat_constant_fill_uses_memset_only_for_byte_values() {
        let units = lower_merged(
            "func main() {
                zeros := []
                for i in 0..100 { zeros = vec.push(zeros, 0) }
                fives := []
                for i in 0..100 { fives = vec.push(fives, 5) }
                println(zeros[0])
                println(fives[0])
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            code.contains("memset("),
            "zero fill must lower to a memset:\n{code}"
        );
        assert!(
            code.matches("for (int64_t ").count() == 1,
            "only the non-byte fill may keep its loop:\n{code}"
        );
    }

    /// The non-negative index fast path is a flow fact, not a property of the
    /// name: a negative local keeps the general check (which folds from the
    /// end), while a literal uses the single-compare variant.
    #[test]
    fn flat_index_fast_path_tracks_sign() {
        let units = lower_merged(
            "func main() {
                arr := []
                for i in 0..4 { arr = vec.push(arr, i) }
                j := -1
                println(arr[j])
                println(arr[1])
            }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let code = &prelude.code;
        assert!(
            code.contains("zz_flat_index("),
            "negative local needs the general check:\n{code}"
        );
        assert!(
            code.contains("zz_flat_index_nn(1"),
            "literal index should use the unsigned fast path:\n{code}"
        );
    }

    /// A module-level `arr: [int] = []` used only through the flat-array
    /// operations lowers to a contiguous pointer plus length/capacity
    /// counters (not a boxed `zz_value`).
    #[test]
    fn flat_global_array_gets_pointer_and_counters() {
        let units = lower_merged(
            "arr: [int] = []
             func main() {
                 for i in 0..3 { arr.push(i) }
                 total := 0
                 for x in arr { total = total + x }
                 println(total)
                 println(len(arr))
             }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let all_code: String = units.units.iter().map(|u| u.code.as_str()).collect();
        assert!(
            all_code.contains("int64_t * zz_global_arr;"),
            "flat buffer def missing:\n{all_code}"
        );
        assert!(
            all_code.contains("int64_t zz_global_arr_len;"),
            "length counter def missing:\n{all_code}"
        );
        assert!(
            all_code.contains("int64_t zz_global_arr_cap;"),
            "capacity counter def missing:\n{all_code}"
        );
        assert!(
            prelude.header.contains("extern int64_t * zz_global_arr;"),
            "flat buffer extern missing:\n{}",
            prelude.header
        );
    }

    /// An unsupported mention (printing the whole array) keeps the global
    /// boxed: there is no flat → zz_value materialization at that boundary.
    #[test]
    fn escaping_global_array_stays_boxed() {
        let units = lower_merged(
            "arr: [int] = []
             func main() { arr.push(1)\nprintln(arr) }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        assert!(
            prelude.code.contains("zz_value zz_global_arr;"),
            "expected boxed global:\n{}",
            prelude.code
        );
        assert!(
            !prelude.code.contains("zz_global_arr_len"),
            "boxed global must not get counters:\n{}",
            prelude.code
        );
    }

    /// Regression for the double-mangled flat-array counter: `arr_len`/`arr_cap`
    /// must lower to `zz_global_arr_len` / `zz_global_arr_cap` (single
    /// underscore, appended to the already-mangled base `zz_global_arr`), NOT
    /// `zz_global_arr__len` (double underscore from re-mangling `arr_len`).
    /// Both `arr.push(...)` growth and `len(arr)` must reference the same
    /// single-underscore counter symbol.
    #[test]
    fn flat_global_array_counter_uses_single_underscore() {
        let units = lower_merged(
            "arr: [int] = []
             func main() {
                 arr.push(1)
                 println(len(arr))
             }",
        );
        let prelude = unit(&units, PRELUDE_NS);
        let all_code: String = units.units.iter().map(|u| u.code.as_str()).collect();
        // Definitions live across units' code (see flat_global_array_gets_pointer_and_counters).
        assert!(
            all_code.contains("int64_t zz_global_arr_len;"),
            "flat counter def missing (single underscore):\n{all_code}"
        );
        assert!(
            all_code.contains("int64_t zz_global_arr_cap;"),
            "flat cap def missing (single underscore):\n{all_code}"
        );
        assert!(
            !all_code.contains("zz_global_arr__len") && !all_code.contains("zz_global_arr__cap"),
            "double-mangled counter must not appear:\n{all_code}"
        );
        // Usage lives in main's body (prelude.code).
        assert!(
            prelude.code.contains("zz_global_arr[zz_global_arr_len]")
                && prelude.code.contains("zz_global_arr_len++"),
            "push must use the single-underscore counter:\n{}",
            prelude.code
        );
        assert!(
            prelude.code.contains("zz_int(zz_global_arr_len)"),
            "len(arr) must read the single-underscore counter:\n{}",
            prelude.code
        );
    }

    /// The whole point of per-unit headers: editing user code must leave a
    /// stdlib unit byte-identical, so its cached object is reusable across
    /// builds and across projects.
    #[test]
    fn stdlib_unit_is_stable_across_program_edits() {
        let a = lower_merged(r#"func main() { println(str.is_empty("")) }"#);
        let b = lower_merged(r#"func main() { println(str.is_empty("x")); println(40 + 2) }"#);

        let str_a = unit(&a, "str");
        let str_b = unit(&b, "str");
        assert_eq!(
            str_a.header, str_b.header,
            "stdlib header must not depend on user code"
        );
        assert_eq!(
            str_a.code, str_b.code,
            "stdlib body must not depend on user code"
        );

        // Program-only sections never reach the stdlib unit ...
        assert!(!str_a.header.contains("zz_fn_main("), "{}", str_a.header);
        assert!(!str_a
            .header
            .contains("// ---- module globals (extern) ----"));
        assert!(!str_a.header.contains("zz_init__prelude"));
        // ... while program units see every prototype they can call.
        let prelude = unit(&a, PRELUDE_NS);
        assert!(
            prelude.header.contains("zz_fn_main_("),
            "{}",
            prelude.header
        );
        assert!(prelude
            .header
            .contains("// ---- module globals (extern) ----"));
        assert!(
            prelude.header.contains("zz_fn_str_is__empty("),
            "{}",
            prelude.header
        );
        // The init driver is entry-only: `zz_main` lives there and is the
        // only caller of `zz_init_*`.
        let entry = unit(&a, "main");
        assert!(entry.header.contains("zz_init_str"), "{}", entry.header);
        assert!(
            entry.header.contains("zz_init_z__prelude"),
            "{}",
            entry.header
        );
        assert!(!str_a.header.contains("zz_init_str"), "{}", str_a.header);
        // `LoweredUnits::header` stays the entry-unit superset.
        assert_eq!(a.header, entry.header);
    }

    /// Direct callers that never set `precompiled` (unit tests, embed-only
    /// paths) keep the old whole-program header contract: no stdlib scan,
    /// no partitioning, same generated C.
    #[test]
    fn non_precompiled_lowering_keeps_the_shared_header() {
        let parsed = zz_frontend::parse("func main() { println(1) }");
        assert!(parsed.errors.is_empty());
        let user = zz_hir::build_program(
            &parsed.program,
            HashMap::new(),
            zz_stdlib::stdlib_funcs(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let (pruned, reach) = zz_hir::dce(&user.program, "main");
        let lowerer = Lowerer::new(
            reach.funcs.clone(),
            reach.natives.clone(),
            "main".to_string(),
            pruned,
        );
        let units = lowerer.lower_units("main");
        let prelude = unit(&units, PRELUDE_NS);
        assert!(prelude.header.contains("zz_fn_main_("));
        assert_eq!(units.header, unit(&units, "main").header);
    }

    /// A package unit carries the dependency section (its own prototypes)
    /// but no project-only sections, so it can be cached independently of
    /// the program that imports it.
    #[test]
    fn pkg_unit_carries_only_dependency_header() {
        let origins: HashMap<String, String> = [("pkg".to_string(), "pkg".to_string())].into();
        let units = lower_merged_origins(
            "func pkg.add(a: int, b: int) -> int { return a + b }\nfunc main() { pkg.add(1, 2) }",
            origins,
        );
        let pkg = unit(&units, "pkg");
        assert!(
            pkg.header.contains("zz_fn_pkg_add("),
            "pkg header must expose its own prototype\n{}",
            pkg.header
        );
        // No project-only sections reach the package unit.
        assert!(!pkg.header.contains("zz_fn_main("), "{}", pkg.header);
        assert!(
            !pkg.header.contains("// ---- module globals (extern) ----"),
            "{}",
            pkg.header
        );
        assert!(!pkg.header.contains("zz_init__prelude"), "{}", pkg.header);
        // The package unit still compiles standalone-ish: its code defines
        // the function it declares.
        assert!(pkg.code.contains("zz_fn_pkg_add("), "{}", pkg.code);
    }

    /// The same external package imported by two different projects yields
    /// a byte-identical package unit (header + code), so its object is
    /// reusable across projects. Editing project code must not touch it.
    #[test]
    fn pkg_unit_is_reusable_across_projects() {
        let origins: HashMap<String, String> = [("pkg".to_string(), "pkg".to_string())].into();
        let pkg_src = "func pkg.add(a: int, b: int) -> int { return a + b }";
        let proj_a = format!("{pkg_src}\nfunc main() {{ pkg.add(1, 2) }}");
        let proj_b = format!("{pkg_src}\nfunc main() {{ pkg.add(3, 4) }}");
        let a = lower_merged_origins(&proj_a, origins.clone());
        let b = lower_merged_origins(&proj_b, origins.clone());

        let pkg_a = unit(&a, "pkg");
        let pkg_b = unit(&b, "pkg");
        assert_eq!(
            pkg_a.header, pkg_b.header,
            "pkg header must be project-independent"
        );
        assert_eq!(
            pkg_a.code, pkg_b.code,
            "pkg body must be project-independent"
        );

        // Project units reflect the project edit (code differs).
        let pre_a = unit(&a, PRELUDE_NS);
        let pre_b = unit(&b, PRELUDE_NS);
        assert_ne!(pre_a.code, pre_b.code, "project body must reflect the edit");
    }

    /// When no origin tag is present, every namespace falls back to
    /// project (the pre-step-5 behavior), so no unit is wrongly treated as
    /// a dependency.
    #[test]
    fn untagged_namespaces_stay_project_owned() {
        let units = lower_merged_origins(
            "func main() { println(str.is_empty(\"\")) }",
            HashMap::new(),
        );
        let str_unit = unit(&units, "str");
        // `str` is stdlib (via stdlib_func_names), not project: narrow header.
        assert!(
            !str_unit.header.contains("zz_fn_main("),
            "{}",
            str_unit.header
        );
        let prelude = unit(&units, PRELUDE_NS);
        assert!(
            prelude.header.contains("zz_fn_main_("),
            "{}",
            prelude.header
        );
    }
}
