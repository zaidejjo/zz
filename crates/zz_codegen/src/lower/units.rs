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

use zz_frontend::ast::{Expr, Stmt};

use super::context::NameCtx;
use super::{mangle, Lowerer};

/// Prelude bucket for bare (dotless) names.
pub const PRELUDE_NS: &str = "_prelude";

/// One module's translation-unit body (header shared, see [`LoweredUnits`]).
#[derive(Debug)]
pub struct CodeUnit {
    /// Module namespace (or `_prelude`).
    pub ns: String,
    /// C code: owned global defs + function bodies + closure defs +
    /// `zz_init_<ns>()` (+ `zz_main` glue when `ns` is the entry).
    pub code: String,
}

/// Partitioned lowering output.
#[derive(Debug)]
pub struct LoweredUnits {
    /// Shared C header: runtime decls, struct preamble, extern globals,
    /// extern prototypes, closure forward decls. Prepended to every unit.
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
        // Global name -> owning namespace (first declarer wins).
        let mut owners: HashMap<String, String> = HashMap::new();
        // Init-driver call order (namespace first appearance).
        let mut init_order: Vec<String> = Vec::new();

        // Hoisted cache (same data collect_globals computes, once per Lowerer).
        let mut names = NameCtx::new();
        self.seed_globals(&mut names);
        self.seed_scalar_fns(&mut names);
        let mut global_init_done: HashSet<String> = HashSet::new();
        let mut current_ns = PRELUDE_NS.to_string();

        // Drain newly pushed closure defs into `dst` (attribution by emitter).
        let drain_closures = |dst: &mut Vec<String>, lowerer: &Lowerer, before: usize| {
            let fresh: Vec<String> = lowerer.closure_defs.borrow_mut().split_off(before);
            dst.extend(fresh);
        };

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
                    let before = self.closure_defs.borrow().len();
                    let code = self.emit_function(&fname, params, b);
                    buf_for(&ns, &mut order, &mut fns).push_str(&code);
                    let slot = closures.entry(ns).or_default();
                    drain_closures(slot, self, before);
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
                            let before = self.closure_defs.borrow().len();
                            let code = self.emit_function(&fname, params, b);
                            buf_for(&ns, &mut order, &mut fns).push_str(&code);
                            let slot = closures.entry(ns.clone()).or_default();
                            drain_closures(slot, self, before);
                        }
                    }
                }
                Struct { .. } | TypeAlias { .. } | Enum { .. } | Import { .. } => {}
                Decl { name, value, .. } => {
                    let ns = current_ns.clone();
                    claim_owner(&mut owners, &name.name, &ns);
                    if !init_order.contains(&ns) {
                        init_order.push(ns.clone());
                    }
                    let before = self.closure_defs.borrow().len();
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
                    let slot = closures.entry(ns).or_default();
                    drain_closures(slot, self, before);
                }
                Destructure { pat, value, .. } => {
                    let ns = current_ns.clone();
                    for n in destructure_top_names(pat) {
                        claim_owner(&mut owners, &n, &ns);
                    }
                    if !init_order.contains(&ns) {
                        init_order.push(ns.clone());
                    }
                    let before = self.closure_defs.borrow().len();
                    let mut out = String::new();
                    self.emit_destructure_global(
                        pat,
                        value,
                        &mut names,
                        &mut out,
                        &mut global_init_done,
                    );
                    buf_for(&ns, &mut order, &mut inits).push_str(&out);
                    let slot = closures.entry(ns).or_default();
                    drain_closures(slot, self, before);
                }
                other => {
                    let ns = current_ns.clone();
                    if !init_order.contains(&ns) {
                        init_order.push(ns.clone());
                    }
                    let before = self.closure_defs.borrow().len();
                    let mut out = String::new();
                    self.emit_stmt(other, &mut names, &mut out, false);
                    buf_for(&ns, &mut order, &mut inits).push_str(&out);
                    let slot = closures.entry(ns).or_default();
                    drain_closures(slot, self, before);
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
            buf_for(&ans, &mut order, &mut fns).push_str(&format!(
                "zz_value zz_fn_{}(zz_value *args, size_t argc) {{ return zz_fn_{}(args, argc); }}\n",
                mangle(alias),
                mangle(canonical)
            ));
        }

        // Shared header: runtime decls + preamble + extern globals/protos.
        let struct_preamble = self.lower_structs_preamble();
        let struct_debug_fns = self.lower_struct_debug_fns();
        let mut glob_ext = String::new();
        for (zz_name, cid, ctype, _) in &self.global_list {
            let _ = zz_name;
            glob_ext.push_str(&format!("extern {ctype} {cid};\n"));
        }
        let mut forward_decls = String::new();
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
            let first_struct_c = self
                .tp
                .funcs
                .get(fname)
                .and_then(|sig| sig.params.first().map(|(_, t)| t.clone()))
                .filter(|t| matches!(t, zz_checker::Type::Struct(_, _)))
                .filter(|_| self.is_impl_method(fname))
                .map(|t| self.type_to_c(&t));
            match first_struct_c {
                Some(sct) => forward_decls.push_str(&format!(
                    "zz_value zz_fn_{}({sct} *self, zz_value *args, size_t argc);\n",
                    mangle(fname)
                )),
                None => forward_decls.push_str(&format!(
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
                    forward_decls.push_str(&format!("{ret} zz_fn_{}_u({psig});\n", mangle(fname)));
                }
            }
        }
        let closure_fwd = self.closure_forward_decls.borrow().join("");
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
        let ffi_pre = crate::ffi::ffi_prelude(&expanded_natives);
        let ffi_section = if ffi_pre.is_empty() {
            String::new()
        } else {
            format!("\n// ---- native-runtime FFI ----\n{ffi_pre}\n")
        };
        let needs_native_rt = crate::ffi::needs_native_rt(&expanded_natives);
        let runtime_c = if self.precompiled {
            String::new()
        } else {
            crate::RUNTIME_C.to_string()
        };
        let header = format!(
            "{runtime_h}\n{runtime_c}\n{ffi_section}\n{extern_section}// ---- struct definitions ----\n{struct_preamble}\n{struct_debug_fns}\n// ---- module globals (extern) ----\n{glob_ext}\n// ---- forward declarations ----\n{forward_decls}{closure_fwd}",
            runtime_h = crate::RUNTIME_H,
        );

        // Assemble units: owned global defs + bodies + closures + init fn.
        let mut units: Vec<CodeUnit> = Vec::new();
        // Every namespace with code gets a unit even without inits, so no
        // reachable body is dropped (init fns only for namespaces in
        // init_order — empty init is still emitted for link uniformity).
        let mut all_ns: Vec<String> = order.clone();
        for ns in &init_order {
            if !all_ns.contains(ns) {
                all_ns.push(ns.clone());
            }
        }
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
        let entry_code = with_main + &entry_glue;
        match units.iter_mut().find(|u| u.ns == entry_ns) {
            Some(u) => {
                u.code.push_str(&entry_code);
            }
            None => units.push(CodeUnit {
                ns: entry_ns.to_string(),
                code: entry_code,
            }),
        }

        LoweredUnits {
            header,
            units,
            entry_ns: entry_ns.to_string(),
            needs_native_rt,
            needs_pg_link: crate::ffi::needs_pg_link(&expanded_natives),
            needs_float_fmt: self.tp.types.values().any(super::type_has_float)
                || self.tp.bindings.values().any(super::type_has_float)
                || self.tp.funcs.values().any(|s| {
                    s.params.iter().any(|(_, ty)| super::type_has_float(ty))
                        || super::type_has_float(&s.ret)
                }),
            needs_curl: crate::ffi::needs_curl_link(&expanded_natives),
            needs_sqlite: crate::ffi::needs_sqlite_link(&expanded_natives),
        }
    }
}

/// Per-namespace string buffer (registers namespace in encounter order).
fn buf_for<'a>(
    ns: &str,
    order: &mut Vec<String>,
    map: &'a mut HashMap<String, String>,
) -> &'a mut String {
    if !map.contains_key(ns) {
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
