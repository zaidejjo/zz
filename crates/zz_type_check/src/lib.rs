//! Core type-checking logic: statements, expressions, patterns.

use zz_checker_core::{Checker, FuncSig};
use zz_diag_core::{error_at, warning_at, ErrorCode, FixIt};
use zz_infer::InferOps;
use zz_lints::http_lint::HttpLintOps;
use zz_scope::ScopeOps;
use zz_sigs::enums::EnumOps;
use zz_span_ast::ast::{BinOp, Block, Expr, FmtPart, Ident, Lit, Param, Pattern, Stmt, Ty, UnOp};
use zz_span_ast::span::Span;
use zz_structs::StructOps;
use zz_suggest::suggest_all;
use zz_types::Type;
use zz_unify::Unifier;

mod check_call_args;
mod check_stmt;
mod definite_assignment;

/// Cheap static sanity check over the literal SQL text: balanced
/// single/double quotes and parens. Returns an error message when the
/// text is clearly malformed; `None` means "looks plausible".
fn check_sql_static(text: &str) -> Option<String> {
    let mut single = false;
    let mut double = false;
    let mut depth: i32 = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '(' if !single && !double => depth += 1,
            ')' if !single && !double => {
                depth -= 1;
                if depth < 0 {
                    return Some("unbalanced `)` in SQL string".to_string());
                }
            }
            '-' if !single && !double && chars.peek() == Some(&'-') => {
                // `--` line comment: skip to end of line.
                for c2 in chars.by_ref() {
                    if c2 == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if single {
        return Some("unbalanced `'` in SQL string".to_string());
    }
    if double {
        return Some("unbalanced `\"` in SQL string".to_string());
    }
    if depth != 0 {
        return Some("unbalanced `(` in SQL string".to_string());
    }
    None
}

/// Human wording for the valid index range of a value of length `len`,
/// shared by the checker's static out-of-bounds diagnostics.
fn index_range_hint(len: usize) -> String {
    if len == 0 {
        "the value is empty, so no index is valid (check `len(...)` first)".to_string()
    } else {
        format!(
            "valid indices are 0..={} (or -1..=-{len}, counting from the end)",
            len - 1
        )
    }
}

/// Fold an integer literal expression to a constant i64. Handles int
/// literals, negated literals, and binary add/sub/mul/div/rem of int
/// literals. Returns `None` for anything that can't be folded at check
/// time (leaving the runtime bounds-check to handle it — zero false
/// positives).
fn const_eval_int(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Int { value, .. } => Some(*value),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => const_eval_int(expr).and_then(|v| v.checked_neg()),
        Expr::Binary {
            op, left, right, ..
        } => {
            let a = const_eval_int(left)?;
            let b = const_eval_int(right)?;
            match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                BinOp::Div => {
                    if b == 0 {
                        None // div-by-zero handled by check_arith
                    } else {
                        a.checked_div(b)
                    }
                }
                BinOp::Rem => {
                    if b == 0 {
                        None
                    } else {
                        a.checked_rem(b)
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// A method invocation: resolved signature plus receiver, args, and span.
pub struct MethodCall<'a> {
    pub sig: &'a FuncSig,
    pub recv_t: &'a Type,
    /// Embedded-promoted receiver (struct embedding only), if any.
    pub promoted_recv: Option<Type>,
    pub method: &'a str,
    pub args: &'a [Expr],
    pub named: &'a [(Ident, Expr)],
    pub span: Span,
}

/// Every name a pattern binds, in source order (#518).
pub(crate) fn pattern_bindings(pat: &Pattern) -> Vec<(String, Span)> {
    fn walk(pat: &Pattern, out: &mut Vec<(String, Span)>) {
        match pat {
            Pattern::Binding { name } => out.push((name.name.clone(), name.span)),
            Pattern::Tuple { pats, .. } => pats.iter().for_each(|p| walk(p, out)),
            Pattern::Variant { arg, .. } => {
                if let Some(a) = arg {
                    walk(a, out);
                }
            }
            Pattern::Or { pats, .. } => pats.iter().for_each(|p| walk(p, out)),
            Pattern::Wildcard { .. } | Pattern::Literal { .. } => {}
        }
    }
    let mut out = Vec::new();
    walk(pat, &mut out);
    out
}

pub trait TypeCheckOps {
    /// Warn when a `:=` initializer read the binding the declaration shadows
    /// (#518): `x := x + 1` in a loop body re-declares a body-scoped `x` from
    /// the untouched outer value every pass, so the outer binding never
    /// advances while `x = x + 1` does. Only the self-referential shape warns:
    /// shadowing with a fresh value is deliberate and common.
    fn warn_if_shadowed_decl(&mut self, mark: usize, value: &Expr, annotated: bool, is_const: bool);

    /// Warn for destructuring declarations whose initializer read a binding
    /// they shadow (#518). There is no one-token fix — a pattern cannot become
    /// an assignment — so the note points at the two-step split instead.
    fn warn_if_shadowed_destructure(&mut self, mark: usize);

    /// Type-check one statement, dispatching to per-statement helpers in
    /// `check_stmt` module. Returns the statement's value type (`Unit` for
    /// most statements, `Never` for `return`).
    fn check_stmt(&mut self, stmt: &Stmt) -> Type;

    /// Type of an assignment target: a variable, a qualified name, or a
    /// struct field path.
    /// Reject assignment to immutable (`const`) variables. The target
    /// is an `Ident` in plain programs and a `Path` (e.g. `ns.x`) after
    /// the loader namespaces top-level bindings. Shared by `=` and
    /// compound assignment so both reject `const` identically.
    fn reject_const_target(&mut self, target: &Expr);

    /// Check an enum construction `Enum.Variant(args)` where `enum_name`
    /// is the already-canonicalized enum and `variant` the trailing call
    /// path component. Returns the enum type on success.
    fn check_enum_construction(
        &mut self,
        enum_name: &str,
        variant: &str,
        args: &[zz_span_ast::ast::Expr],
        named: &[(zz_span_ast::ast::Ident, zz_span_ast::ast::Expr)],
        span: Span,
    ) -> Option<Type>;

    fn check_func_body(&mut self, stmt: &Stmt, sig: &FuncSig);
    /// Report a missing-return diagnostic with improved diagnostics:
    /// - unit-tail functions (println, match, etc.) get the message only, no fixit.
    /// - annotated non-unit functions that can fall through get error + fixit.
    /// - fixit is withheld for `if` without `else`, match arms, and `let`
    ///   bindings whose name is used later (intentional shadowings).
    fn report_missing_return(&mut self, body: &Block, ret_ty: &Type);
    /// Extract an identifier name from a statement or expression for shadow-checking.
    fn extract_name(stmt: &Stmt) -> Option<String>;
    fn extract_name_from_block(block: &Block) -> Option<String>;
    /// Extract all identifier names from a statement (for shadow checking).
    fn extract_names(stmt: &Stmt) -> Vec<String>;
    fn extract_names_from_block(block: &Block) -> Vec<String>;
    fn block_has_return(block: &Block) -> bool;
    fn stmt_has_return(stmt: &Stmt) -> bool;
    fn expr_has_return(expr: &Expr) -> bool;

    fn check_assign_target(&mut self, target: &Expr) -> Type;

    /// Element type of a tuple index, shared by reads and writes.
    /// Tuples index like arrays at runtime (AOT lowers them to arrays;
    /// the VM stores them as `Tuple`), but the element type depends on
    /// the position, so only integer literals type-check — negative
    /// literals count from the end like arrays. Anything else should
    /// destructure: `(a, b) := t`.
    fn check_tuple_index(&mut self, elems: &[Type], index: &Expr) -> Type;

    /// A constant index into an array *literal* is decidable at check time:
    /// report the out-of-bounds before the program runs (the runtime would
    /// trap on it anyway), with the valid range as the hint.
    fn check_literal_array_index(&mut self, len: usize, index: &Expr);

    /// Whether a loop body can leave through a `break` that targets this
    /// loop. Nested loops capture their own breaks, so they are not
    /// descended into.
    fn block_has_break(block: &Block) -> bool;

    fn expr_has_break(expr: &Expr) -> bool;

    fn check_block(&mut self, block: &Block) -> Type;

    /// Check a block whose value is discarded (loop bodies): tail
    /// expressions are discards, so must-use warnings apply to them.
    fn check_block_discarded(&mut self, block: &Block) -> Type;

    /// Check a named function body: its tail expression is **discarded**.
    /// A named function yields a value only through `return`; block
    /// expressions and closures keep their tail-value semantics
    /// (`check_fn_body_block`'s old behaviour). Discarding the tail also
    /// gives `Result`/`Option` tails the must-use warning they deserve.
    fn check_named_fn_body(&mut self, block: &Block) -> Type;

    /// Sherlock: warn when a value-position expression discards a
    /// `Result`/`Option`. Structural expressions (if/match/blocks) flow
    /// from their inner statements, which are checked on their own.
    /// Tail exemption lives with the caller: only `Stmt::Expr` tails
    /// are block values (`defer` tails are not — its value is `unit`).
    fn warn_if_discarded(&mut self, e: &Expr, t: &Type);

    /// Check an expression, recording its resolved type keyed by scoped key
    /// so the HIR could bindings can be built deterministically afterward.
    /// The key pairs the span with the enclosing top-level item: bare spans
    /// repeat across modules (offsets restart at 0 per file).
    /// Resolve a named-function value type (`p1.greet` from using a bare
    /// function name as a value) to its `Func` type so it unifies with
    /// func-typed positions like struct fields (#385). Generic functions
    /// stay `Named` (using one as a value errors at lookup).
    fn resolve_fn_value(&mut self, t: Type) -> Type;

    fn check_expr(&mut self, e: &Expr) -> Type;

    fn check_expr_impl(&mut self, e: &Expr) -> Type;

    fn check_unary(&mut self, op: UnOp, expr: &Expr, span: Span) -> Type;

    fn check_binary(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type;

    /// Bitwise operators are strictly `(int, int) -> int`. Unlike
    /// [`Self::check_arith`] there is no float promotion, no string
    /// overload, and no generic `Num` path — a float bit-pattern op is
    /// almost always a bug, so it is a hard error with a cast hint.
    fn check_bitwise(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type;

    fn check_arith(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type;

    /// Type-check array concatenation `[T] + [U]`: element types must match
    /// exactly, or one side can be unified with the other.
    fn check_array_concat(&mut self, a: &Box<Type>, b: &Box<Type>, span: Span) -> Type;

    /// True when a `head.method(args)` call must be read as a method call
    /// even though `head.method` also names a seeded free function: the
    /// head is a genuine local value, the direct reading is already
    /// impossible (fewer args than its minimum), and the method reading
    /// fits with a receiver type matching the first parameter. Because
    /// diversion requires the direct reading to fail, previously-passing
    /// programs are untouched by construction.
    ///
    /// Motivating case: `db.exec(sql)` with a *local* `db` must not match
    /// the seeded `db.exec` free function at full arity (which rejects
    /// the receiver-implicit form); a top-level `db` instead resolves
    /// through the module-namespace path and is unaffected, as are all
    /// qualified module calls (their heads never live in value scope).
    fn method_shadow_call(&mut self, callee: &Expr, args: &[Expr], named: &[(Ident, Expr)])
        -> bool;

    /// True when a named receiver type carries its own `impl` method
    /// called `method`. `impl W { func get(self) }` registers `W.get`, so
    /// this is the dot-call form's own definition: it outranks a
    /// same-named receiver-implicit free function (`get(w)`), which keeps
    /// dot syntax from silently binding the free function and leaving the
    /// method unreachable (#500). The free function stays callable by
    /// name, so neither form is lost.
    fn receiver_impl_method(&self, recv: &Type, method: &str) -> bool;

    /// Table-driven removed/renamed stdlib API check (see [`REMOVED_APIS`]).
    fn check_removed_api(
        &mut self,
        name: &str,
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) -> Option<Type>;

    fn check_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) -> Type;

    /// Handle a `Field` callee (`obj.method(...)`): resolve the method
    /// signature against the receiver's type and delegate to
    /// `check_method_call`. Returns `Some(type)` if dispatched, `None`
    /// to fall through to direct-name handling.
    fn check_call_field_callee(
        &mut self,
        obj: &Expr,
        method: String,
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) -> Option<Type>;

    /// Handle `print`/`println` builtin calls. Returns `Some(Unit)` if
    /// `name` is a print builtin and no local shadows it, `None` otherwise.
    fn check_call_print_builtin(
        &mut self,
        name: &str,
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) -> Option<Type>;

    /// Resolve a method signature for a typed receiver by looking up
    /// `TypeName.method` in the stdlib table.
    fn resolve_method_sig(&self, recv_t: &Type, method: &str) -> Option<FuncSig>;

    /// Check that the given positional and named arguments match the parameter
    /// types.  `has_default` indicates which trailing parameters have defaults;
    /// callers may omit those.
    fn check_method_call(&mut self, call: &MethodCall<'_>) -> Type;

    /// Short display name for call diagnostics: `math.sin` → `sin`.
    fn short_name(name: &str) -> &str;

    /// Sherlock notes for an argument/parameter mismatch: name the
    /// parameter and its expected type so the error points at the fix,
    /// not just the conflict. Placeholder names (`_0`, from `func`
    /// values) still show the type.
    fn param_mismatch_notes(param_names: &[String], ps: &[Type], i: usize) -> Vec<String>;

    #[allow(clippy::too_many_arguments)]
    fn check_args_against(
        &mut self,
        callee: Option<&str>,
        param_names: &[String],
        ps: &[Type],
        has_default: &[bool],
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    );

    /// Compile-time SQL verification for `db.query(sql)` / `db.exec(sql)`.
    ///
    /// The SQL arg is normally an interpolated string (`Fmt`): static text
    /// segments stay in the prepared statement, `{expr}` segments become
    /// `?N` bound parameters. This check ensures every bound expr is a
    /// bindable scalar (int/float/str/bool) and rejects format specs
    /// (`{x:.2f}` would render client-side, breaking parameterization).
    /// Static text is scanned for balanced quotes/parens as a cheap
    /// syntax sanity check; full schema verification happens at runtime
    /// against the live connection.
    fn verify_sql_params(&mut self, sql_arg: &Expr, span: Span);

    /// `expected_params`: when a closure is passed to a function with a
    /// known signature, the caller can provide the expected parameter types
    /// here. For each unannotated param, if an expected type exists, the
    /// fresh var is unified with it *before* checking the body — so the
    /// body sees known param types and can resolve method calls (e.g.
    /// `tx.exec(...)` when `tx: Db`).
    fn check_closure(
        &mut self,
        params: &[Param],
        ret_ty: Option<&Ty>,
        body: &Expr,
        _span: Span,
        expected_params: Option<&[Type]>,
    ) -> Type;

    /// Innermost span of a match-guard subexpression the native backend
    /// cannot lower (`None` when the whole guard is in the scalar subset:
    /// idents, paths, int/float/bool literals, and binary/unary/paren
    /// combinations). Native guard lowering folds anything else to a
    /// constant while the VM evaluates it fully — a silent engine
    /// divergence the caller turns into a loud check-time error.
    fn native_unsupported_guard(e: &Expr) -> Option<Span>;

    fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[zz_span_ast::ast::MatchArm],
        span: Span,
    ) -> Type;

    fn check_try(&mut self, expr: &Expr, span: Span) -> Type;

    /// True when an expression tree contains a `Try` (`?` / `try`) node.
    fn expr_contains_try(e: &Expr) -> bool;

    fn stmt_contains_try(s: &Stmt) -> bool;

    /// Find conversion impl spans from `from` to `to`.
    /// Checks the local `convert_impls` registry plus seeded `ext_methods` /
    /// `funcs` (cross-module `convert_to_` methods) so multi-file programs work.
    fn find_converts(&self, from: &Type, to: &Type) -> Vec<Option<Span>>;

    fn bind_pattern(&mut self, pat: &Pattern, ty: &Type);
    /// Bind a `.Variant` pattern: constrains the scrutinee type, then binds payload names.
    fn bind_pattern_variant(
        &mut self,
        name: &str,
        arg: &Option<Box<Pattern>>,
        span: Span,
        ty: &Type,
    );

    /// Handle `.Variant` pattern on an enum scrutinee.
    fn bind_pattern_variant_enum(
        &mut self,
        ename: &str,
        vname: &str,
        eargs: &[Type],
        arg: &Option<Box<Pattern>>,
        span: Span,
    ) -> Option<(Pattern, Type)>;

    /// Handle `.some(x)` pattern: binds payload name to Option's inner type.
    fn bind_pattern_variant_option(
        &mut self,
        inner: &Type,
        arg: &Option<Box<Pattern>>,
        span: Span,
    ) -> Option<(Pattern, Type)>;

    /// Handle `.none` pattern: rejects any argument.
    fn bind_pattern_variant_none(
        &mut self,
        arg: &Option<Box<Pattern>>,
        span: Span,
    ) -> Option<(Pattern, Type)>;

    fn check_exhaustive(&mut self, st: &Type, arms: &[zz_span_ast::ast::MatchArm], span: Span);

    /// True if `pat` matches every value of its type (wildcard / binding /
    /// catch-all or-pattern / all-catch-all tuple).
    fn pat_covers_all(&self, pat: &Pattern) -> bool;

    /// Constructors of `ty` not covered by `pats` (Maranget specialization).
    fn missing_patterns(&self, pats: &[&Pattern], ty: &Type) -> Option<Vec<String>>;

    /// Check missing patterns for an enum type.
    fn missing_patterns_enum(&self, ename: &str, flat: &[&Pattern]) -> Option<Vec<String>>;

    /// Flatten an `or`-pattern into its alternatives.
    fn or_alt<'a>(&self, pat: &'a Pattern) -> Vec<&'a Pattern>;

    /// Argument pattern of a variant pattern `.V(arg)`, or wildcard if none.
    fn variant_arg(&self, pat: &Pattern) -> Pattern;
}

/// `print`/`println` accept any number of values (#713): like other
/// languages, positional arguments render space-separated.
fn is_print_builtin(name: &str) -> bool {
    matches!(name, "print" | "println")
}

/// A removed/renamed stdlib API: old spellings → replacement hint + the type
/// the (now-removed) call used to produce, so downstream checking stays quiet.
struct RemovedApi {
    names: &'static [&'static str],
    replacement: &'static str,
    ret: RemovedRet,
}

#[derive(Clone, Copy)]
enum RemovedRet {
    Unit,
    Int,
    Str,
    Array,
    /// `Result<Str, Str>`
    ResultStrStr,
    /// `Result<Unit, Str>`
    ResultUnitStr,
    /// `Result<Array<Str>, Str>`
    ResultArrayStr,
}

impl RemovedRet {
    fn to_type(self, unifier: &mut Unifier) -> Type {
        match self {
            RemovedRet::Unit => Type::Unit,
            RemovedRet::Int => Type::Int,
            RemovedRet::Str => Type::Str,
            RemovedRet::Array => Type::Array(Box::new(unifier.fresh_var())),
            RemovedRet::ResultStrStr => Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
            RemovedRet::ResultUnitStr => Type::Result(Box::new(Type::Unit), Box::new(Type::Str)),
            RemovedRet::ResultArrayStr => Type::Result(
                Box::new(Type::Array(Box::new(Type::Str))),
                Box::new(Type::Str),
            ),
        }
    }
}

/// Removed/renamed stdlib APIs, checked table-driven so each arm doesn't
/// repeat the "check args, report error, return type" boilerplate.
/// Entries are consulted only when `name` does not resolve to a known
/// function (so real `pg.query` after `import std.sqlz.postgres as pg`
/// still resolves through the signature table).
static REMOVED_APIS: &[RemovedApi] = &[
    RemovedApi {
        names: &["vec.append", "std.vec.append", "append"],
        replacement: "vec.push(v, x) or the method form xs.push(x)",
        ret: RemovedRet::Array,
    },
    RemovedApi {
        names: &["pg.query", "postgres.query", "my.query", "mysql.query"],
        replacement: "pg.query(..) / my.query(..)",
        ret: RemovedRet::Array,
    },
    RemovedApi {
        names: &["pg.exec", "postgres.exec", "my.exec", "mysql.exec"],
        replacement: "pg.query(..) / my.query(..)",
        ret: RemovedRet::Int,
    },
    RemovedApi {
        names: &["std.env.unset", "env.unset"],
        replacement: "env.remove(key)",
        ret: RemovedRet::Unit,
    },
    RemovedApi {
        names: &[
            "std.fs.read_file",
            "fs.read_file",
            "std.fs.read_to_string",
            "fs.read_to_string",
        ],
        replacement: "std.fs.read",
        ret: RemovedRet::ResultStrStr,
    },
    RemovedApi {
        names: &["std.fs.write_file", "fs.write_file"],
        replacement: "std.fs.write",
        ret: RemovedRet::ResultUnitStr,
    },
    RemovedApi {
        names: &["std.fs.remove_file", "fs.remove_file"],
        replacement: "std.fs.remove",
        ret: RemovedRet::ResultUnitStr,
    },
    RemovedApi {
        names: &["std.fs.readdir", "fs.readdir"],
        replacement: "std.fs.read_dir",
        ret: RemovedRet::ResultArrayStr,
    },
    RemovedApi {
        names: &["std.fs.rename", "fs.rename"],
        replacement: "fs.move(src, dst)",
        ret: RemovedRet::ResultUnitStr,
    },
    RemovedApi {
        names: &["std.net.tcp_close", "net.tcp_close", "tcp_close"],
        replacement: "stream.close() / stream.shutdown()",
        ret: RemovedRet::ResultUnitStr,
    },
];

impl TypeCheckOps for Checker {
    // --- statements -------------------------------------------------------

    /// Warn when a `:=` initializer read the binding the declaration shadows
    /// (#518): `x := x + 1` in a loop body re-declares a body-scoped `x` from
    /// the untouched outer value every pass, so the outer binding never
    /// advances while `x = x + 1` does. Only the self-referential shape warns:
    /// shadowing with a fresh value is deliberate and common.
    fn warn_if_shadowed_decl(
        &mut self,
        mark: usize,
        value: &Expr,
        annotated: bool,
        is_const: bool,
    ) {
        let Some(probe) = self.finish_shadow_probes(mark).pop() else {
            return;
        };
        let name = probe.name;
        let mut diag = warning_at(
            format!("`{name} := …` shadows the outer `{name}` — the outer binding keeps its value"),
            probe.span,
        )
        .with_note(format!(
            "the declaration is scoped to the surrounding block; each pass re-reads the outer value — `{name} = …` updates it instead"
        ));
        // `x := x + 1` → `x = x + 1` is a one-token fix. A `const` binding and
        // an annotated declaration have no assignment form, so they keep only
        // the note.
        if !is_const && !annotated {
            diag = diag.with_fixit(FixIt::safe(
                Span::new(probe.span.end, value.span().start),
                " = ",
                "assign the outer binding",
            ));
        }
        self.errors.push(diag);
    }

    /// Warn for destructuring declarations whose initializer read a binding
    /// they shadow (#518). There is no one-token fix — a pattern cannot become
    /// an assignment — so the note points at the two-step split instead.
    fn warn_if_shadowed_destructure(&mut self, mark: usize) {
        for probe in self.finish_shadow_probes(mark) {
            let name = probe.name;
            self.errors.push(
                warning_at(
                    format!(
                        "`{name} := …` shadows the outer `{name}` — the outer binding keeps its value"
                    ),
                    probe.span,
                )
                .with_note(format!(
                    "destructuring binds fresh names in this block: keep the result (`p := …`) and assign the outer binding separately (`{name} = p[1]`)"
                )),
            );
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Type {
        check_stmt::check_stmt(self, stmt)
    }

    /// Type of an assignment target: a variable, a qualified name, or a
    /// struct field path.
    /// Reject assignment to immutable (`const`) variables. The target
    /// is an `Ident` in plain programs and a `Path` (e.g. `ns.x`) after
    /// the loader namespaces top-level bindings. Shared by `=` and
    /// compound assignment so both reject `const` identically.
    fn reject_const_target(&mut self, target: &Expr) {
        let tname: Option<String> = match target {
            Expr::Ident { name, .. } => Some(name.clone()),
            Expr::Path { parts, .. } => Some(parts.join(".")),
            _ => None,
        };
        if let Some(tname) = tname {
            if let Some(def_span) = self.lookup_const_span(&tname) {
                let display = Self::display_name(&tname);
                self.errors.push(
                    error_at(
                        format!("cannot assign to immutable variable `{}`", display),
                        target.span(),
                    )
                    .with_secondary(zz_diag_core::SecondaryLabel {
                        span: def_span,
                        message: "variable defined as immutable here".to_string(),
                    })
                    .with_note(format!(
                        "hint: remove `const` to make `{}` mutable",
                        display
                    )),
                );
            }
        }
    }

    fn check_assign_target(&mut self, target: &Expr) -> Type {
        match target {
            Expr::Ident { name, span } => self.lookup(name, *span),
            Expr::Path { parts, span } => self.lookup_path(parts, *span),
            Expr::Field { obj, name, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                match ot {
                    Type::Struct(sname, args) => {
                        self.struct_field_access(&sname, &args, name, *span)
                    }
                    Type::Dict(k, v) => {
                        // Dict field access: req.body returns the value type
                        if let Err(e) = self.unifier.unify(&Type::Str, &k) {
                            self.report_mismatch(e, *span);
                        }
                        *v
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot assign to field `{name}` of a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Index { obj, index, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                let it = self.check_expr(index);
                match ot {
                    Type::Array(elem) => {
                        self.ensure_int(it, index.span());
                        if let Expr::Array { elems, .. } = obj.as_ref() {
                            self.check_literal_array_index(elems.len(), index);
                        }
                        *elem
                    }
                    Type::Dict(k, v) => {
                        if let Err(e) = self.unifier.unify(&it, &k) {
                            self.report_mismatch(e, index.span());
                        }
                        *v
                    }
                    Type::Str => {
                        self.errors
                            .push(error_at("cannot assign to an index of a string", *span));
                        Type::Unit
                    }
                    Type::Tuple(elems) => self.check_tuple_index(&elems, index),
                    Type::Bytes => {
                        self.errors
                            .push(error_at("cannot assign to an index of bytes", *span));
                        Type::Unit
                    }
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot assign to an index of a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot assign to an index of a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            other => {
                self.errors.push(error_at(
                    "cannot assign to this expression".to_string(),
                    other.span(),
                ));
                Type::Unit
            }
        }
    }

    /// Element type of a tuple index, shared by reads and writes.
    /// Tuples index like arrays at runtime (AOT lowers them to arrays;
    /// the VM stores them as `Tuple`), but the element type depends on
    /// the position, so only integer literals type-check — negative
    /// literals count from the end like arrays. Anything else should
    /// destructure: `(a, b) := t`.
    fn check_tuple_index(&mut self, elems: &[Type], index: &Expr) -> Type {
        let pos: Option<usize> = match index {
            Expr::Int { value, .. } if *value >= 0 => Some(*value as usize),
            // Literal `i64::MIN` (the negation fold) and other negatives
            // fall through to the out-of-bounds error below.
            Expr::Int { .. } => None,
            Expr::Unary { op, expr, .. } if *op == UnOp::Neg => match expr.as_ref() {
                Expr::Int { value: 0, .. } => Some(0),
                Expr::Int { value, .. } => elems.len().checked_sub(*value as usize),
                _ => {
                    self.errors.push(error_at(
                        "tuple index must be an integer literal\n\
                         hint: destructure with `(a, b) := t` for dynamic access",
                        index.span(),
                    ));
                    return Type::Error;
                }
            },
            _ => {
                self.errors.push(error_at(
                    "tuple index must be an integer literal\n\
                     hint: destructure with `(a, b) := t` for dynamic access",
                    index.span(),
                ));
                return Type::Error;
            }
        };
        match pos {
            Some(i) if i < elems.len() => elems[i].clone(),
            _ => {
                self.errors.push(
                    error_at(
                        format!(
                            "tuple index out of bounds for length {}\n\
                         hint: {}",
                            elems.len(),
                            index_range_hint(elems.len())
                        ),
                        index.span(),
                    )
                    .with_code(ErrorCode(8)),
                );
                Type::Error
            }
        }
    }

    /// A constant index into an array *literal* is decidable at check time:
    /// report the out-of-bounds before the program runs (the runtime would
    /// trap on it anyway), with the valid range as the hint.
    fn check_literal_array_index(&mut self, len: usize, index: &Expr) {
        let raw: i64 = match const_eval_int(index) {
            Some(v) => v,
            None => return,
        };
        let in_bounds = if raw < 0 {
            (len as i64).checked_add(raw).is_some_and(|i| i >= 0)
        } else {
            (raw as u64) < len as u64
        };
        if in_bounds {
            return;
        }
        self.errors.push(
            error_at(
                format!(
                    "index {raw} out of bounds for length {len}\n\
                 hint: {}",
                    index_range_hint(len)
                ),
                index.span(),
            )
            .with_code(ErrorCode(8)),
        );
    }

    /// Whether a loop body can leave through a `break` that targets this
    /// loop. Nested loops capture their own breaks, so they are not
    /// descended into.
    fn block_has_break(block: &Block) -> bool {
        block.stmts.iter().any(|stmt| match stmt {
            Stmt::Break { .. } => true,
            Stmt::Expr(expr) => Self::expr_has_break(expr),
            _ => false,
        })
    }

    fn expr_has_break(expr: &Expr) -> bool {
        match expr {
            Expr::Break { .. } => true,
            Expr::If { then, els, .. } | Expr::IfLet { then, els, .. } => {
                Self::block_has_break(then)
                    || els
                        .as_ref()
                        .is_some_and(|e| Self::expr_has_break(e.as_ref()))
            }
            Expr::Match { arms, .. } => arms.iter().any(|arm| Self::expr_has_break(&arm.body)),
            Expr::Block(block) => Self::block_has_break(block),
            // A nested `while` owns its breaks; no other expression can
            // carry a `break` bound to the enclosing loop.
            Expr::While { .. } => false,
            _ => false,
        }
    }

    fn check_block(&mut self, block: &Block) -> Type {
        self.push_scope();
        let mut result = Type::Unit;
        // Sherlock: code after a diverging statement (`return`/`break`/
        // `continue`, or an if/match that diverges on every path) never
        // runs — warn once per statement instead of checking dead code
        // into confusing cascades.
        let mut diverged: Option<&'static str> = None;
        let last = block.stmts.len().saturating_sub(1);
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let Some(how) = diverged {
                self.errors.push(warning_at(
                    format!("unreachable code after diverging {how}"),
                    stmt.span(),
                ));
            }
            // The tail expression is the block's value — unless the
            // block itself is discarded (loop bodies) or top-level code.
            let outer_tail_is_value = self.tail_is_value;
            let prev_tail =
                std::mem::replace(&mut self.tail_is_value, (i == last) && outer_tail_is_value);
            result = self.check_stmt(stmt);
            self.tail_is_value = prev_tail;
            // A bare `return` types as `unit` (not `Never`), so track
            // syntactic divergence too — it still never falls through.
            let syntactic = matches!(
                stmt,
                Stmt::Return { .. }
                    | Stmt::Break { .. }
                    | Stmt::Continue { .. }
                    | Stmt::Expr(Expr::Break { .. })
                    | Stmt::Expr(Expr::Continue { .. })
            );
            if syntactic || matches!(self.unifier.resolve(&result), Type::Never) {
                diverged = Some(match stmt {
                    Stmt::Return { .. } => "`return`",
                    Stmt::Break { .. } => "`break`",
                    Stmt::Continue { .. } => "`continue`",
                    Stmt::Expr(Expr::Break { .. }) => "`break`",
                    Stmt::Expr(Expr::Continue { .. }) => "`continue`",
                    _ => "statement",
                });
            }
        }
        self.last_block_tail = block
            .stmts
            .last()
            .map(|s| (s.span(), self.unifier.resolve(&result)));
        self.pop_scope();
        result
    }

    /// Check a block whose value is discarded (loop bodies): tail
    /// expressions are discards, so must-use warnings apply to them.
    fn check_block_discarded(&mut self, block: &Block) -> Type {
        let prev = std::mem::replace(&mut self.tail_is_value, false);
        let t = self.check_block(block);
        self.tail_is_value = prev;
        t
    }

    /// Check a named function body: its tail expression is **discarded**.
    /// A named function yields a value only through `return`; block
    /// expressions and closures keep their tail-value semantics
    /// (`check_fn_body_block`'s old behaviour). Discarding the tail also
    /// gives `Result`/`Option` tails the must-use warning they deserve.
    fn check_named_fn_body(&mut self, block: &Block) -> Type {
        let prev = std::mem::replace(&mut self.tail_is_value, false);
        let t = self.check_block(block);
        self.tail_is_value = prev;
        t
    }

    /// Sherlock: warn when a value-position expression discards a
    /// `Result`/`Option`. Structural expressions (if/match/blocks) flow
    /// from their inner statements, which are checked on their own.
    /// Tail exemption lives with the caller: only `Stmt::Expr` tails
    /// are block values (`defer` tails are not — its value is `unit`).
    fn warn_if_discarded(&mut self, e: &Expr, t: &Type) {
        if matches!(
            e,
            Expr::If { .. }
                | Expr::While { .. }
                | Expr::Match { .. }
                | Expr::IfLet { .. }
                | Expr::Block(_)
        ) {
            return;
        }
        match self.unifier.resolve(t) {
            Type::Result(_, _) => {
                self.errors.push(
                    warning_at("unused `Result` — errors are silently discarded", e.span())
                        .with_note(
                            "use `?` to propagate, `match` to handle, or `_ :=` to ignore it explicitly",
                        ),
                );
            }
            Type::Option(_) => {
                self.errors.push(
                    warning_at(
                        "unused `Option` — a missing value is silently discarded",
                        e.span(),
                    )
                    .with_note(
                        "use `?` to propagate, `match` to handle, or `_ :=` to ignore it explicitly",
                    ),
                );
            }
            _ => {}
        }
    }

    // --- expressions ------------------------------------------------------

    /// Check an expression, recording its resolved type keyed by scoped key
    /// so the HIR could bindings can be built deterministically afterward.
    /// The key pairs the span with the enclosing top-level item: bare spans
    /// repeat across modules (offsets restart at 0 per file).
    /// Resolve a named-function value type (`p1.greet` from using a bare
    /// function name as a value) to its `Func` type so it unifies with
    /// func-typed positions like struct fields (#385). Generic functions
    /// stay `Named` (using one as a value errors at lookup).
    fn resolve_fn_value(&mut self, t: Type) -> Type {
        match t {
            Type::Named(ref n) => match zz_checker_core::lookup_std(&self.funcs, n).cloned() {
                Some(sig) if sig.generics.is_empty() => {
                    let (ps, ret, _) = self.instantiate(&sig);
                    Type::Func(ps, Box::new(ret))
                }
                _ => t,
            },
            other => other,
        }
    }

    fn check_expr(&mut self, e: &Expr) -> Type {
        let ty = self.check_expr_impl(e);
        // Unresolved types stay as `Var`s during the walk; the typed map is
        // deep-resolved at the end (see `check_program_typed`).
        self.span_types.insert(self.scope_key(e.span()), ty.clone());
        ty
    }

    fn check_expr_impl(&mut self, e: &Expr) -> Type {
        match e {
            Expr::Int { .. } => Type::Int,
            Expr::Float { .. } => Type::Float,
            Expr::Str { .. } => Type::Str,
            Expr::Bool { .. } => Type::Bool,
            Expr::Ident { name, span } => self.lookup(name, *span),
            Expr::Path { parts, span } => {
                // Unit-variant value (`Token.Eof`, no parens): a path, not
                // a call. Resolves against the enum table; payload
                // variants must use call form (`Token.IntLit(1)`).
                if parts.len() >= 2 {
                    let enum_head = parts[..parts.len() - 1].join(".");
                    let canonical_head = self.canonical_enum_name(&enum_head);
                    if self.enums.contains_key(&canonical_head)
                        && !self.funcs.contains_key(&parts.join("."))
                        && self.lookup_opt(&enum_head).is_none()
                    {
                        let variant = parts.last().cloned().unwrap_or_default();
                        if self
                            .enum_variant_payload(&canonical_head, &variant, *span)
                            .is_some_and(|p| p.is_some())
                        {
                            self.errors.push(error_at(
                                format!(
                                    "variant `{canonical_head}.{variant}` holds a value: construct it as `{canonical_head}.{variant}(...)`"
                                ),
                                *span,
                            ));
                        }
                        if canonical_head.contains('.') {
                            self.used_names.insert(canonical_head.clone());
                        }
                        // Generic parameters stay inference variables
                        // (defaulted like `Option` when never constrained).
                        let (gen_vars, _) = self.fresh_enum_vars(&canonical_head);
                        return Type::Enum(canonical_head, gen_vars);
                    }
                }
                self.lookup_path(parts, *span)
            }
            Expr::Field { obj, name, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                match ot {
                    Type::Struct(sname, args) => {
                        self.struct_field_access(&sname, &args, name, *span)
                    }
                    Type::Dict(k, v) => {
                        // Dict field access: req.body returns the value type
                        if let Err(e) = self.unifier.unify(&Type::Str, &k) {
                            self.report_mismatch(e, *span);
                        }
                        *v
                    }
                    Type::HttpRequest => {
                        // Typed request field access: `req.method/path/body`
                        // are `str`; `req.headers/query/params` are `{str: str}`.
                        match name.as_str() {
                            "method" | "path" | "body" => Type::Str,
                            "headers" | "query" | "params" => {
                                Type::Dict(Box::new(Type::Str), Box::new(Type::Str))
                            }
                            _ => {
                                self.errors.push(error_at(
                                    format!("http.request has no field `{name}`"),
                                    *span,
                                ));
                                Type::Unit
                            }
                        }
                    }
                    Type::Var(_id) => {
                        // Inference variable — not yet resolved (e.g. untyped closure param).
                        // Return a fresh var; unification will catch real mismatches later.
                        self.unifier.fresh_var()
                    }
                    Type::Error => Type::Error,
                    other => {
                        let method = name.clone();
                        let ns = match &other {
                            Type::Str => Some("str"),
                            Type::Array(_) => Some("vec"),
                            Type::Option(_) => Some("option"),
                            Type::Result(_, _) => Some("result"),
                            Type::Int => Some("int"),
                            Type::Float => Some("float"),
                            Type::Bool => Some("bool"),
                            _ => None,
                        };
                        if let Some(ns) = ns {
                            if let Some(sig) =
                                zz_checker_core::lookup_std(&self.funcs, &format!("{ns}.{method}"))
                                    .cloned()
                            {
                                let (ps, ret, subs) = self.instantiate(&sig);
                                if !ps.is_empty() {
                                    if let Err(e) = self.unifier.unify(&other, &ps[0]) {
                                        self.report_mismatch(e, *span);
                                    }
                                }
                                self.validate_bounds(&sig, &subs, *span);
                                return ret;
                            }
                        }
                        self.errors.push(error_at(
                            format!("cannot access field `{name}` on a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Range { start, end, .. } => {
                let st = self.check_expr(start);
                let et = self.check_expr(end);
                for (t, s) in [(st, start.span()), (et, end.span())] {
                    match self.unifier.resolve(&t) {
                        Type::Int => {}
                        Type::Var(id) => {
                            self.unifier.bind(id, Type::Int);
                        }
                        Type::Error => {}
                        other => {
                            self.errors.push(error_at(
                                format!("range bounds must be `int`, found `{other}`"),
                                s,
                            ));
                        }
                    }
                }
                Type::Range(Box::new(Type::Int))
            }
            Expr::StructInit { name, fields, span } => {
                self.used_names.insert(name.clone());
                // Canonicalize selective imports (`Product` →
                // `product.Product`) so the constructed type matches the
                // qualified spelling everywhere (methods, identity).
                // Messages keep the as-written name.
                let cname = self.canonical_struct_name(name);
                let Some(sig) = self.structs.get(&cname).cloned() else {
                    self.errors
                        .push(error_at(format!("unknown struct `{name}`"), *span));
                    return Type::Unit;
                };
                // Each given field maps to a concrete path: direct fields to
                // `[name]`, promoted (flattened) fields to their embedded
                // prefix + `[name]` (e.g. `id` in `User{id: 1, ...}` maps to
                // `[Base, id]`).
                //
                // Generic parameters instantiate to fresh variables, filled
                // in by unifying each value with its (substituted) field
                // type — so `Box{ v: 1 }` infers `Box<int>`, exactly like a
                // generic function call infers its type arguments.
                let gen_vars: Vec<Type> = sig
                    .generics
                    .iter()
                    .map(|_| self.unifier.fresh_var())
                    .collect();
                let gen_map: std::collections::HashMap<String, Type> = sig
                    .generics
                    .iter()
                    .cloned()
                    .zip(gen_vars.iter().cloned())
                    .collect();
                let mut given_paths: Vec<Vec<String>> = Vec::new();
                for (fname, fval) in fields {
                    if let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == fname) {
                        let vt = self.check_expr(fval);
                        let vt = self.resolve_fn_value(vt);
                        let exp = zz_infer::subst(ft, &gen_map);
                        if let Err(e) = self.unifier.unify(&vt, &exp) {
                            self.report_mismatch(e, fval.span());
                        }
                        given_paths.push(vec![fname.clone()]);
                    } else if let Some((prefix, pft)) =
                        self.resolve_struct_field_generic(&cname, &gen_vars, fname)
                    {
                        if prefix.is_empty() {
                            // Unreachable: direct fields are handled above.
                            continue;
                        }
                        let vt = self.check_expr(fval);
                        let vt = self.resolve_fn_value(vt);
                        if let Err(e) = self.unifier.unify(&vt, &pft) {
                            self.report_mismatch(e, fval.span());
                        }
                        let mut full = prefix;
                        full.push(fname.clone());
                        given_paths.push(full);
                    } else {
                        self.errors.push(error_at(
                            format!("struct `{name}` has no field `{fname}`"),
                            fval.span(),
                        ));
                    }
                }
                // An explicit embedded value and flattened leaves inside the
                // same subtree are ambiguous: reject rather than guess.
                for (i, a) in given_paths.iter().enumerate() {
                    for b in given_paths.iter().skip(i + 1) {
                        let conflict = (a.len() < b.len() && b.starts_with(a))
                            || (b.len() < a.len() && a.starts_with(b));
                        if conflict {
                            let (outer, inner) = if a.len() < b.len() { (a, b) } else { (b, a) };
                            self.errors.push(error_at(
                                format!(
                                    "field `{}` conflicts with embedded value `{}` in struct literal `{name}` (provide one or the other)",
                                    inner.last().unwrap_or(&String::new()),
                                    outer.last().unwrap_or(&String::new()),
                                ),
                                *span,
                            ));
                        }
                    }
                }
                // Verify all required fields are provided (flattened leaves
                // count toward their embedded subtree).
                if let Some(leaf) = self.first_uncovered_leaf(&cname, &[], &given_paths, 0) {
                    self.errors.push(error_at(
                        format!(
                            "missing field `{}` in struct literal `{name}`",
                            leaf.join("."),
                        ),
                        *span,
                    ));
                }
                Type::Struct(
                    cname,
                    gen_vars
                        .iter()
                        .map(|v| self.unifier.resolve_deep(v))
                        .collect(),
                )
            }
            Expr::Index { obj, index, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                let it = self.check_expr(index);
                match ot {
                    Type::Array(elem) => {
                        self.ensure_int(it, index.span());
                        if let Expr::Array { elems, .. } = obj.as_ref() {
                            self.check_literal_array_index(elems.len(), index);
                        }
                        *elem
                    }
                    Type::Bytes => {
                        self.ensure_int(it, index.span());
                        Type::Int
                    }
                    Type::Dict(k, v) => {
                        if let Err(e) = self.unifier.unify(&it, &k) {
                            self.report_mismatch(e, index.span());
                        }
                        *v
                    }
                    Type::Str => {
                        self.ensure_int(it, index.span());
                        Type::Str
                    }
                    Type::Tuple(elems) => self.check_tuple_index(&elems, index),
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot index a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    Type::Error => Type::Unit,
                    other => {
                        self.errors.push(error_at(
                            format!("cannot index a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Slice {
                obj,
                start,
                end,
                span,
            } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                for bound in [start.as_deref(), end.as_deref()].into_iter().flatten() {
                    let bt = self.check_expr(bound);
                    self.ensure_int(bt, bound.span());
                }
                match ot {
                    Type::Array(elem) => Type::Array(elem),
                    Type::Bytes => Type::Bytes,
                    Type::Str => Type::Str,
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot slice a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    Type::Error => Type::Unit,
                    other => {
                        self.errors.push(error_at(
                            format!("cannot slice a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Fmt { parts, .. } => {
                for part in parts {
                    if let FmtPart::Expr(e, _) = part {
                        let _ = self.check_expr(e);
                    }
                }
                Type::Str
            }
            Expr::Paren { expr, .. } => self.check_expr(expr),
            Expr::Unary { op, expr, span } => self.check_unary(*op, expr, *span),
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => self.check_binary(*op, left, right, *span),
            Expr::Call {
                callee,
                args,
                named,
                span,
            } => self.check_call(callee, args, named, *span),
            Expr::Closure {
                params,
                ret_ty,
                body,
                span,
            } => self.check_closure(params, ret_ty.as_ref(), body, *span, None),
            Expr::If {
                cond,
                then,
                els,
                span,
            } => {
                let ct = self.check_expr(cond);
                let glob_hint = match cond.as_ref() {
                    Expr::Str { value, .. } if Self::looks_like_glob(value) => Some(value.as_str()),
                    _ => None,
                };
                self.ensure_bool(ct, cond.span(), glob_hint);
                let tt = self.check_block(then);
                match els {
                    Some(e) => {
                        let et = self.check_expr(e);
                        // Join: a divergent arm vanishes (join(Never, T)
                        // = T) — only arms that can yield values constrain
                        // each other.
                        let tt_r = self.unifier.resolve(&tt);
                        let et_r = self.unifier.resolve(&et);
                        match (tt_r, et_r) {
                            (Type::Never, Type::Never) => Type::Never,
                            (Type::Never, _) => et,
                            (_, Type::Never) => tt,
                            _ => {
                                if let Err(err) = self.unifier.unify(&et, &tt) {
                                    self.report_mismatch(err, e.span());
                                }
                                tt
                            }
                        }
                    }
                    None => {
                        // If the then-block contains a `return`, its type
                        // may be `Never` (e.g. `if x { return 5 }`), but
                        // that's fine — the return short-circuits. Either
                        // way the expression yields unit when the branch
                        // is not taken, so the if-expression types as
                        // unit (this rejects a trailing no-else
                        // if-with-return in a non-unit function, since
                        // the fall-through path yields unit).
                        if !Self::block_has_return(then) {
                            let tt_r = self.unifier.resolve(&tt);
                            match tt_r {
                                Type::Unit | Type::Never | Type::Var(_) => {
                                    if let Err(err) = self.unifier.unify(&Type::Unit, &tt) {
                                        self.report_mismatch(err, *span);
                                    }
                                }
                                _ => {
                                    // Trailing value discarded (#388): point
                                    // at the tail statement that produced
                                    // it, not the `if` — the old span
                                    // (whole `if`) read as if the
                                    // condition failed with reversed
                                    // types. Vars/aliases keep the unify
                                    // path (it pins them to unit).
                                    let tail = then.stmts.last().map(|s| s.span()).unwrap_or(*span);
                                    self.errors.push(
                                        error_at(
                                            format!(
                                                "value of type `{tt_r}` discarded: `if` without `else` used as a statement must end in `unit`"
                                            ),
                                            tail,
                                        )
                                        .with_note(
                                            "assign the value or end the branch with a statement of type `unit`",
                                        ),
                                    );
                                }
                            }
                        }
                        Type::Unit
                    }
                }
            }
            Expr::While { cond, body, .. } => {
                let ct = self.check_expr(cond);
                let glob_hint = match cond.as_ref() {
                    Expr::Str { value, .. } if Self::looks_like_glob(value) => Some(value.as_str()),
                    _ => None,
                };
                self.ensure_bool(ct, cond.span(), glob_hint);
                self.loop_depth += 1;
                self.check_block_discarded(body);
                self.loop_depth -= 1;
                // `while true` without a `break` never leaves the loop, so
                // a named function whose body ends in one does not fall
                // through (e.g. `func serve() -> int { while true { ... return } }`).
                let diverges = matches!(cond.as_ref(), Expr::Bool { value: true, .. })
                    && !Self::block_has_break(body);
                if diverges {
                    Type::Never
                } else {
                    Type::Unit
                }
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.check_match(scrutinee, arms, *span),
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                span,
            } => {
                let vt = self.check_expr(value);
                self.push_scope();
                self.bind_pattern(pat, &vt);
                let tt = self.check_block(then);
                self.pop_scope();
                match els {
                    Some(e) => {
                        let et = self.check_expr(e);
                        // Join: a divergent arm vanishes (join(Never, T)
                        // = T) — only arms that can yield values constrain
                        // each other.
                        let tt_r = self.unifier.resolve(&tt);
                        let et_r = self.unifier.resolve(&et);
                        match (tt_r, et_r) {
                            (Type::Never, Type::Never) => Type::Never,
                            (Type::Never, _) => et,
                            (_, Type::Never) => tt,
                            _ => {
                                if let Err(err) = self.unifier.unify(&et, &tt) {
                                    self.report_mismatch(err, e.span());
                                }
                                tt
                            }
                        }
                    }
                    None => {
                        let tt_r = self.unifier.resolve(&tt);
                        match tt_r {
                            Type::Unit | Type::Never | Type::Var(_) => {
                                if let Err(err) = self.unifier.unify(&Type::Unit, &tt) {
                                    self.report_mismatch(err, *span);
                                }
                            }
                            _ => {
                                // Same discarded-tail rule as `if` (#388).
                                let tail = then.stmts.last().map(|s| s.span()).unwrap_or(*span);
                                self.errors.push(
                                    error_at(
                                        format!(
                                            "value of type `{tt_r}` discarded: `if let` without `else` used as a statement must end in `unit`"
                                        ),
                                        tail,
                                    )
                                    .with_note(
                                        "assign the value or end the branch with a statement of type `unit`",
                                    ),
                                );
                            }
                        }
                        Type::Unit
                    }
                }
            }
            Expr::Try { expr, span } => self.check_try(expr, *span),
            Expr::Block(b) => self.check_block(b),
            Expr::Array { elems, span: _ } => {
                let types: Vec<Type> = elems.iter().map(|e| self.check_expr(e)).collect();
                let elem_t = self.merge_types(types);
                Type::Array(Box::new(elem_t))
            }
            Expr::Tuple { items, .. } => {
                // `()` is the unit value, not a zero-field tuple (#712):
                // match arms spell unit either way (`=> ()` / `=> {}`), and
                // both must unify with a unit-yielding arm.
                if items.is_empty() {
                    Type::Unit
                } else {
                    let types: Vec<Type> = items.iter().map(|e| self.check_expr(e)).collect();
                    Type::Tuple(types)
                }
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                span,
            } => {
                let it = self.check_expr(iter);
                let it = self.unifier.resolve(&it);
                let elem = match it {
                    Type::Array(elem) => *elem,
                    Type::Range(elem) => *elem,
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot iterate a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot iterate a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                };
                self.push_scope();
                self.define_at(&var.name, elem, var.span);
                if let Some(f) = filter {
                    let ft = self.check_expr(f);
                    let ft = self.unifier.resolve(&ft);
                    if let Err(e) = self.unifier.unify(&ft, &Type::Bool) {
                        self.report_mismatch(e, f.span());
                    }
                }
                let body_t = self.check_expr(body);
                self.pop_scope();
                Type::Array(Box::new(body_t))
            }
            Expr::Dict { entries, span: _ } => {
                let mut key_types = Vec::new();
                let mut val_types = Vec::new();
                for (k, v) in entries {
                    key_types.push(self.check_expr(k));
                    val_types.push(self.check_expr(v));
                }
                let key_t = self.merge_types(key_types);
                let val_t = self.merge_types(val_types);
                Type::Dict(Box::new(key_t), Box::new(val_t))
            }
            Expr::Break { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`break` outside of a loop", *span));
                }
                Type::Unit
            }
            Expr::Continue { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`continue` outside of a loop", *span));
                }
                Type::Unit
            }
            Expr::Variant { name, arg, span } => {
                let arg_t = arg.as_ref().map(|a| self.check_expr(a));
                match (name.as_str(), arg_t) {
                    ("ok", Some(t)) => {
                        Type::Result(Box::new(t), Box::new(self.unifier.fresh_var()))
                    }
                    ("ok", None) => {
                        self.errors
                            .push(error_at("`.ok` requires an argument", *span));
                        Type::Result(Box::new(Type::Unit), Box::new(self.unifier.fresh_var()))
                    }
                    ("err", Some(e)) => {
                        Type::Result(Box::new(self.unifier.fresh_var()), Box::new(e))
                    }
                    ("err", None) => {
                        self.errors
                            .push(error_at("`.err` requires an argument", *span));
                        Type::Result(Box::new(self.unifier.fresh_var()), Box::new(Type::Unit))
                    }
                    ("some", Some(t)) => Type::Option(Box::new(t)),
                    ("some", None) => {
                        self.errors
                            .push(error_at("`.some` requires an argument", *span));
                        Type::Option(Box::new(Type::Unit))
                    }
                    ("none", None) => Type::Option(Box::new(self.unifier.fresh_var())),
                    ("none", Some(_)) => {
                        self.errors
                            .push(error_at("`.none` takes no argument", *span));
                        Type::Option(Box::new(self.unifier.fresh_var()))
                    }
                    (other, _) => {
                        self.errors
                            .push(error_at(format!("unknown variant `.{other}`"), *span));
                        Type::Unit
                    }
                }
            }
        }
    }

    fn check_unary(&mut self, op: UnOp, expr: &Expr, span: Span) -> Type {
        let t = self.check_expr(expr);
        let t = self.unifier.resolve(&t);
        match op {
            UnOp::Not => match t {
                Type::Bool => Type::Bool,
                Type::Var(id) => {
                    self.unifier.bind(id, Type::Bool);
                    Type::Bool
                }
                other => {
                    self.errors
                        .push(error_at(format!("expected `bool`, found `{other}`"), span));
                    Type::Bool
                }
            },
            UnOp::BitNot => match t {
                Type::Int => Type::Int,
                Type::Var(id) => {
                    self.unifier.bind(id, Type::Int);
                    Type::Int
                }
                other => {
                    self.errors.push(error_at(
                        format!(
                            "bitwise `~` requires an `int` operand, found `{other}`\n\
                             hint: use `!` for boolean negation"
                        ),
                        span,
                    ));
                    Type::Error
                }
            },
            UnOp::Pos | UnOp::Neg => match t {
                Type::Int => Type::Int,
                Type::Float => Type::Float,
                Type::Named(n) => {
                    if self.has_bound(&n, zz_span_ast::ast::TraitBound::Num) {
                        Type::Named(n)
                    } else {
                        self.errors.push(error_at(
                            format!(
                                "generic parameter `{n}` needs a `Num` bound for `{}`; write `func ...<{n}: Num>`",
                                op.symbol()
                            ),
                            span,
                        ));
                        Type::Int
                    }
                }
                Type::Var(id) => {
                    self.unifier.bind(id, Type::Int);
                    Type::Int
                }
                other => {
                    self.errors.push(error_at(
                        format!("cannot negate a value of type `{other}`"),
                        span,
                    ));
                    Type::Int
                }
            },
        }
    }

    fn check_binary(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        match op {
            BinOp::And | BinOp::Or => {
                let lt = self.check_expr(left);
                let lhint = match left {
                    Expr::Str { value, .. } if Self::looks_like_glob(value) => Some(value.as_str()),
                    _ => None,
                };
                self.ensure_bool(lt, left.span(), lhint);
                let rt = self.check_expr(right);
                let rhint = match right {
                    Expr::Str { value, .. } if Self::looks_like_glob(value) => Some(value.as_str()),
                    _ => None,
                };
                self.ensure_bool(rt, right.span(), rhint);
                Type::Bool
            }
            BinOp::Elvis => {
                let lt = self.check_expr(left);
                let lt_resolved = self.unifier.resolve(&lt);
                let rt = self.check_expr(right);
                let rt_resolved = self.unifier.resolve(&rt);
                match lt_resolved {
                    Type::Option(inner) => {
                        if let Err(e) = self.unifier.unify(&inner, &rt_resolved) {
                            self.report_mismatch(e, span);
                        }
                        *inner
                    }
                    Type::Result(inner, _err) => {
                        if let Err(e) = self.unifier.unify(&inner, &rt_resolved) {
                            self.report_mismatch(e, span);
                        }
                        *inner
                    }
                    _ => {
                        if let Err(e) = self.unifier.unify(&lt, &rt) {
                            self.report_mismatch(e, span);
                        }
                        lt
                    }
                }
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                use zz_span_ast::ast::TraitBound;
                let lt = self.check_expr(left);
                let lt = self.unifier.resolve(&lt);
                let rt = self.check_expr(right);
                let rt = self.unifier.resolve(&rt);
                // Which bound does this comparison require?
                let needed = match op {
                    BinOp::Eq | BinOp::Ne => TraitBound::Eq,
                    _ => TraitBound::Ord,
                };
                // Reject int/float mixed comparisons with a helpful message.
                match (&lt, &rt) {
                    (Type::Int, Type::Float) | (Type::Float, Type::Int) => {
                        let (from, to, cast_fn) = if matches!(&lt, Type::Int) {
                            (lt.clone(), rt.clone(), "float")
                        } else {
                            (lt.clone(), rt.clone(), "int")
                        };
                        let mut diag = error_at(
                            format!(
                                "cannot compare `{}` with `{}`. Use `{cast_fn}(x)` to cast",
                                lt, rt
                            ),
                            span,
                        );
                        // Suggest casting the left operand; the fixit points at the left
                        // expression span via `left.span` which is available as `lt`'s origin.
                        // We point at the comparison span; the note tells which side to cast.
                        diag = diag.with_note(format!(
                            "cast `{from}` to `{to}` with `{cast_fn}(...)` on the mismatched side"
                        ));
                        self.errors.push(diag);
                        Type::Bool
                    }
                    // Generic operands: allowed only when the parameter
                    // carries the required bound (`Eq` for ==/!=, `Ord` for
                    // ordering). The concrete type is pinned at the call site.
                    (Type::Named(a), Type::Named(b)) if a == b => {
                        if self.has_bound(a, needed) {
                            Type::Bool
                        } else {
                            self.errors.push(error_at(
                                format!(
                                    "generic parameter `{a}` needs a `{}` bound for `{}`; write `func ...<{a}: {}>`",
                                    needed.name(),
                                    op.symbol(),
                                    needed.name()
                                ),
                                span,
                            ));
                            Type::Bool
                        }
                    }
                    (Type::Named(n), _) | (_, Type::Named(n)) => {
                        if self.has_bound(n, needed) {
                            Type::Bool
                        } else {
                            self.errors.push(error_at(
                                format!(
                                    "generic parameter `{n}` needs a `{}` bound for `{}`; write `func ...<{n}: {}>`",
                                    needed.name(),
                                    op.symbol(),
                                    needed.name()
                                ),
                                span,
                            ));
                            Type::Bool
                        }
                    }
                    (Type::Var(_), t) => {
                        self.unifier.bind_var(&lt, t.clone());
                        Type::Bool
                    }
                    (t, Type::Var(_)) => {
                        self.unifier.bind_var(&rt, t.clone());
                        Type::Bool
                    }
                    _ => {
                        if let Err(e) = self.unifier.unify(&rt, &lt) {
                            self.report_mismatch(e, span);
                        }
                        Type::Bool
                    }
                }
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                self.check_arith(op, left, right, span)
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                self.check_bitwise(op, left, right, span)
            }
        }
    }

    /// Bitwise operators are strictly `(int, int) -> int`. Unlike
    /// [`Self::check_arith`] there is no float promotion, no string
    /// overload, and no generic `Num` path — a float bit-pattern op is
    /// almost always a bug, so it is a hard error with a cast hint.
    fn check_bitwise(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        let lt = self.check_expr(left);
        let lt = self.unifier.resolve(&lt);
        let rt = self.check_expr(right);
        let rt = self.unifier.resolve(&rt);
        match (&lt, &rt) {
            (Type::Int, Type::Int) => Type::Int,
            // Inference variables default to int (bitwise pins them).
            (Type::Var(_), Type::Int) => {
                self.unifier.bind_var(&lt, Type::Int);
                Type::Int
            }
            (Type::Int, Type::Var(_)) => {
                self.unifier.bind_var(&rt, Type::Int);
                Type::Int
            }
            (Type::Var(_), Type::Var(_)) => {
                self.unifier.bind_var(&lt, Type::Int);
                self.unifier.bind_var(&rt, Type::Int);
                Type::Int
            }
            _ => {
                // `Error` (earlier failure) and `Never` (divergent,
                // unreachable operand) suppress cascading errors —
                // same convention as `check_arith`.
                if !matches!(
                    (&lt, &rt),
                    (Type::Error, _) | (_, Type::Error) | (Type::Never, _) | (_, Type::Never)
                ) {
                    let mut diag = error_at(
                        format!(
                            "bitwise `{}` requires `int` operands, found `{lt}` and `{rt}`\n\
                             hint: use `int(x)` to cast, `&&`/`||` for boolean logic",
                            op.symbol()
                        ),
                        span,
                    );
                    let left_not_int = !matches!(&lt, Type::Int | Type::Var(_));
                    let right_not_int = !matches!(&rt, Type::Int | Type::Var(_));
                    if left_not_int && right_not_int {
                        diag = diag.with_note("cast both operands to `int` with `int(...)`");
                    } else if left_not_int {
                        diag = diag.with_note("cast the left operand to `int` with `int(...)`");
                    } else if right_not_int {
                        diag = diag.with_note("cast the right operand to `int` with `int(...)`");
                    }
                    self.errors.push(diag);
                }
                Type::Error
            }
        }
    }

    fn check_arith(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        use zz_span_ast::ast::TraitBound;
        let lt = self.check_expr(left);
        let lt = self.unifier.resolve(&lt);
        let rt = self.check_expr(right);
        let rt = self.unifier.resolve(&rt);
        match (&lt, &rt) {
            (Type::Int, Type::Int) => {
                // Sherlock: a literal zero divisor always traps at runtime —
                // fail at check time with the exact spot instead (#250-area).
                // Float division is excluded (`1.0 / 0.0` is `inf`, not an error).
                if matches!(op, BinOp::Div | BinOp::Rem) {
                    if let Expr::Int { value: 0, .. } = right {
                        let what = if op == BinOp::Div {
                            "division"
                        } else {
                            "remainder"
                        };
                        self.errors.push(error_at(
                            format!("integer {what} by zero (divisor is literal `0`)"),
                            right.span(),
                        ));
                        return Type::Error;
                    }
                }
                Type::Int
            }
            (Type::Str, Type::Str) if op == BinOp::Add => Type::Str,
            // Array concat: `[T] + [T] -> [T]`. Element types must match
            // exactly ([int] + [str] is a hard error; cast one side first).
            // An inference-var element (`[]`) unifies with the concrete side.
            (Type::Array(a), Type::Array(b)) if op == BinOp::Add => {
                self.check_array_concat(a, b, span)
            }
            (Type::Int, Type::Float) | (Type::Float, Type::Int) | (Type::Float, Type::Float) => {
                Type::Float
            }
            // Generic operands: allowed only when the parameter carries a
            // `Num` bound. The result keeps the generic type; the concrete
            // type is pinned at the call site via instantiation.
            (Type::Named(a), Type::Named(b)) if a == b => {
                if self.has_bound(a, TraitBound::Num) {
                    Type::Named(a.clone())
                } else {
                    self.errors.push(error_at(
                        format!(
                            "generic parameter `{a}` needs a `Num` bound for `{}`; write `func ...<{a}: Num>`",
                            op.symbol()
                        ),
                        span,
                    )
                    .with_code(ErrorCode(3)));
                    Type::Error
                }
            }
            (Type::Named(a), Type::Named(b)) => {
                self.errors.push(error_at(
                    format!(
                        "cannot apply `{}` to `{a}` and `{b}`: distinct generic parameters\n\
                         hint: use the same type parameter (`x: T, y: T`) or a concrete type",
                        op.symbol()
                    ),
                    span,
                ));
                Type::Error
            }
            (Type::Named(n), t) | (t, Type::Named(n))
                if self.has_bound(n, TraitBound::Num) && matches!(t, Type::Int | Type::Float) =>
            {
                Type::Named(n.clone())
            }
            (Type::Named(n), Type::Var(_)) | (Type::Var(_), Type::Named(n)) => {
                if self.has_bound(n, TraitBound::Num) {
                    Type::Named(n.clone())
                } else {
                    self.errors.push(error_at(
                        format!(
                            "generic parameter `{n}` needs a `Num` bound for `{}`; write `func ...<{n}: Num>`",
                            op.symbol()
                        ),
                        span,
                    ));
                    Type::Error
                }
            }
            (Type::Var(_), t) => {
                self.unifier.bind_var(&lt, t.clone());
                t.clone()
            }
            (t, Type::Var(_)) => {
                self.unifier.bind_var(&rt, t.clone());
                t.clone()
            }
            (a, b) => {
                // `Error` (earlier failure) and `Never` (divergent,
                // unreachable operand) both suppress cascading errors.
                if !matches!(
                    (&a, &b),
                    (Type::Error, _) | (_, Type::Error) | (Type::Never, _) | (_, Type::Never)
                ) {
                    let mut diag = error_at(
                        format!("cannot apply `{}` to `{}` and `{}`", op.symbol(), a, b),
                        span,
                    )
                    .with_code(ErrorCode(2));
                    // Suggest the conversion matching the other operand (#247).
                    let conv = match (&a, &b) {
                        (Type::Str, Type::Int) | (Type::Int, Type::Str) => Some("str"),
                        (Type::Str, Type::Float) | (Type::Float, Type::Str) => Some("str"),
                        (Type::Str, Type::Bool) | (Type::Bool, Type::Str) => Some("str"),
                        (Type::Int, Type::Float) | (Type::Float, Type::Int) => Some("float"),
                        _ => None,
                    };
                    if let Some(f) = conv {
                        diag = diag
                            .with_note(format!("use {f}(x) to convert, or \"{{x}}\"-format it"));
                    }
                    self.errors.push(diag);
                }
                Type::Error
            }
        }
    }

    /// Type-check array concatenation `[T] + [U]`: element types must match
    /// exactly, or one side can be unified with the other. Reports an error
    /// with cast hints when element types differ.
    fn check_array_concat(&mut self, a: &Box<Type>, b: &Box<Type>, span: Span) -> Type {
        let ea = self.unifier.resolve(a);
        let eb = self.unifier.resolve(b);
        match (&ea, &eb) {
            (Type::Var(_), Type::Var(_)) => {
                // Both sides unknown — can't pin element type yet.
                Type::Array(Box::new(self.unifier.fresh_var()))
            }
            (Type::Var(_), concrete) => {
                self.unifier.bind_var(a, concrete.clone());
                Type::Array(Box::new(concrete.clone()))
            }
            (concrete, Type::Var(_)) => {
                self.unifier.bind_var(b, concrete.clone());
                Type::Array(Box::new(concrete.clone()))
            }
            (_, _) if ea == eb => Type::Array(Box::new(ea)),
            (a2, b2) => {
                let mut diag = error_at(
                    format!(
                        "cannot apply `+` to `[{a2}]` and `[{b2}]`: element types differ\n\
                         hint: cast one side so both arrays share an element type",
                    ),
                    span,
                );
                diag = diag.with_note(format!(
                    "cast `[{a2}]` to `[{b2}]` or `[{b2}]` to `[{a2}]` with `{b2}(...)` / `{a2}(...)`"
                ));
                self.errors.push(diag);
                Type::Error
            }
        }
    }

    /// True when a `head.method(args)` call must be read as a method call
    /// even though `head.method` also names a seeded free function: the
    /// head is a genuine local value, the direct reading is already
    /// impossible (fewer args than its minimum), and the method reading
    /// fits with a receiver type matching the first parameter. Because
    /// diversion requires the direct reading to fail, previously-passing
    /// programs are untouched by construction.
    ///
    /// Motivating case: `db.exec(sql)` with a *local* `db` must not match
    /// the seeded `db.exec` free function at full arity (which rejects
    /// the receiver-implicit form); a top-level `db` instead resolves
    /// through the module-namespace path and is unaffected, as are all
    /// qualified module calls (their heads never live in value scope).
    fn method_shadow_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        named: &[(Ident, Expr)],
    ) -> bool {
        let Expr::Path { parts, .. } = callee else {
            return false;
        };
        if parts.len() != 2 {
            return false;
        }
        let Some(sig) = zz_checker_core::lookup_std(&self.funcs, &parts.join(".")).cloned() else {
            return false;
        };
        // The head must be a real local binding — module namespaces never
        // live in value scope. Walk scopes directly: `lookup_opt` would
        // also match the `joined` function entry itself.
        if !self.env.iter().rev().any(|s| s.contains_key(&parts[0])) {
            return false;
        }
        if sig.params.is_empty() {
            return false;
        }
        let provided = args.len() + named.len();
        let total = sig.params.len();
        let direct_min = total - sig.has_default.iter().filter(|&&d| d).count();
        // Divert only when the direct reading already fails...
        if provided >= direct_min {
            return false;
        }
        // ...and the method reading fits: within the non-receiver arity
        // window, with a receiver type matching the first parameter.
        // Compared structurally (no unification): binding inference vars
        // here could leak across the two readings.
        let method_total = total - 1;
        if provided > method_total {
            return false;
        }
        let method_min = method_total - sig.has_default.iter().skip(1).filter(|&&d| d).count();
        if provided < method_min {
            return false;
        }
        let recv_t = self
            .env
            .iter()
            .rev()
            .find_map(|s| s.get(&parts[0]).cloned())
            .map(|t| self.unifier.resolve(&t));
        let first_t = self.unifier.resolve(&sig.params[0].1);
        recv_t.is_some_and(|r| r == first_t)
    }

    /// True when a named receiver type carries its own `impl` method
    /// called `method`. `impl W { func get(self) }` registers `W.get`, so
    /// this is the dot-call form's own definition: it outranks a
    /// same-named receiver-implicit free function (`get(w)`), which keeps
    /// dot syntax from silently binding the free function and leaving the
    /// method unreachable (#500). The free function stays callable by
    /// name, so neither form is lost.
    fn receiver_impl_method(&self, recv: &Type, method: &str) -> bool {
        // Resolve first: a top-level binding's recorded type can still be a
        // unification variable at its use site, and reading the raw type
        // would silently skip the receiver's own method.
        let recv = self.unifier.resolve_deep(recv);
        match &recv {
            Type::Struct(name, _) | Type::Enum(name, _) => {
                zz_checker_core::lookup_std(&self.funcs, &format!("{name}.{method}")).is_some()
            }
            _ => false,
        }
    }

    /// Table-driven removed/renamed stdlib API check (see [`REMOVED_APIS`]).
    /// Returns `Some(type)` when `name` is a removed API — after checking its
    /// args (so nested errors surface) and reporting the rename hint — else
    /// `None`, so normal unknown-callee diagnostics proceed.
    fn check_removed_api(
        &mut self,
        name: &str,
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) -> Option<Type> {
        if self.funcs.contains_key(name) {
            return None;
        }
        let api = REMOVED_APIS.iter().find(|api| api.names.contains(&name))?;
        // Still check the args so nested errors inside them surface, then
        // stop: the name no longer resolves, and letting lookup run on would
        // add a confusing second error.
        for arg in args {
            self.check_expr(arg);
        }
        for (_, val) in named {
            self.check_expr(val);
        }
        self.errors.push(error_at(
            format!(
                "removed: `{name}` is gone — use {} instead",
                api.replacement
            ),
            span,
        ));
        Some(api.ret.to_type(&mut self.unifier))
    }

    /// Handle `print`/`println` builtin calls: variadic, checks each argument,
    /// warns about bare function values passed as arguments. Returns `Some(Unit)`
    /// if `name` is a print builtin and no local shadows it, `None` otherwise.
    fn check_call_print_builtin(
        &mut self,
        name: &str,
        args: &[Expr],
        named: &[(Ident, Expr)],
        _span: Span,
    ) -> Option<Type> {
        if !is_print_builtin(name) || self.local_lookup_opt(name).is_some() {
            return None;
        }
        self.used_names.insert(name.to_string());
        for arg in args {
            self.check_expr(arg);
        }
        for (_, val) in named {
            self.check_expr(val);
        }
        // A bare function value as any print argument is always a missing
        // `()` (`println(env.os)` would print the function itself instead
        // of calling it). Catch it here with the name attached.
        for arg_expr in args.iter().chain(named.iter().map(|(_, v)| v)) {
            // Resolve silently (lookup_opt, never check_expr: the argument
            // was already checked above and a second pass would duplicate
            // diagnostics).
            let (arg_t, fname) = match arg_expr {
                Expr::Ident { name: n, .. } => (self.lookup_opt(n), Some(n.clone())),
                Expr::Path { parts, .. } => {
                    let joined = parts.join(".");
                    (self.lookup_opt(&joined), Some(joined))
                }
                _ => (None, None),
            };
            let is_func = matches!(
                arg_t.as_ref().map(|t| self.unifier.resolve(t)),
                Some(Type::Func(_, _))
            ) || matches!(fname.as_deref(), Some(n)
                if zz_checker_core::lookup_std(&self.funcs, n).is_some()
                    && !self.is_math_const(n));
            if !is_func {
                continue;
            }
            let Some(fname) = fname else { continue };
            let mut diag = error_at(
                format!("cannot print function `{fname}`: call it with arguments"),
                arg_expr.span(),
            );
            diag = diag.with_note(format!("did you mean `{fname}()`?"));
            diag = diag.with_fixit(FixIt::safe(
                Span::new(arg_expr.span().end, arg_expr.span().end),
                "()".to_string(),
                "call function",
            ));
            self.errors.push(diag);
        }
        Some(Type::Unit)
    }

    /// Handle a `Field` callee (`obj.method(...)`): type-check the receiver,
    /// resolve the method signature against the receiver's type (builtins,
    /// struct/enum impls, embedded promotion), lint HTTP route/param usage,
    /// and delegate to `check_method_call`. Returns `Some(type)` if a method
    /// was dispatched, `None` to fall through to direct-name handling.
    fn check_call_field_callee(
        &mut self,
        obj: &Expr,
        method: String,
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) -> Option<Type> {
        let recv_t = self.check_expr(obj);
        let recv_t = self.unifier.resolve(&recv_t);
        // Phase 2.2: `std.http` route + param lint on typed receivers.
        if matches!(self.unifier.resolve(&recv_t), Type::HttpServer)
            && zz_http_routes::is_route_method(&method)
        {
            let root = match obj {
                Expr::Ident { name, .. } => Some(name.clone()),
                _ => None,
            };
            self.lint_http_route_method(root, &method, args, span);
        } else if method == "param" && matches!(self.unifier.resolve(&recv_t), Type::HttpRequest) {
            // Field-call args exclude the receiver: `req.param(name)`.
            self.lint_http_param(args.first());
        }
        // Dot-call order for a `Field` callee (`makePoint().get()`):
        // the receiver's own `impl` method (`{Type}.{method}`) wins
        // over a same-named receiver-implicit free function (#500).
        // The free signature is restored below when nothing matches,
        // so its mismatch diagnostics are unchanged.
        let impl_owned = self.receiver_impl_method(&recv_t, &method);
        let free_sig = zz_checker_core::lookup_std(&self.funcs, &method).cloned();
        let mut sig = if impl_owned { None } else { free_sig.clone() };
        if sig.is_none() {
            sig = self.resolve_method_sig(&recv_t, &method);
        }
        // Embedded promotion: `u.area()` dispatches to `Base.area`
        // when `User` embeds `Base`. The runtime passes the embedded
        // value as the receiver, so the outer type is not unified
        // against the method's receiver below.
        let mut promoted_recv: Option<Type> = None;
        if sig.is_none() {
            if let Type::Struct(sname, sargs) = self.unifier.resolve(&recv_t) {
                if let Some((defining, psig)) = self.find_struct_method(&sname, &method) {
                    promoted_recv = self
                        .promoted_method_receiver(&sname, &sargs, &defining)
                        .or_else(|| Some(Type::Struct(defining, Vec::new())));
                    sig = Some(psig);
                }
            }
        }
        // No method matched: restore the free signature so the call
        // reports a mismatch against it rather than falling through.
        if sig.is_none() {
            sig = free_sig;
        }
        sig.map(|sig| {
            self.check_method_call(&MethodCall {
                sig: &sig,
                recv_t: &recv_t,
                promoted_recv,
                method: &method,
                args,
                named,
                span,
            })
        })
    }

    /// Resolve a method signature for a typed receiver by looking up
    /// `TypeName.method` in the stdlib table. Covers scalars, Option/Result,
    /// Array/Bytes, Str, Response, Http*, net, json, sqlz, chan, task,
    /// opaque tags, structs, and enums.
    fn resolve_method_sig(&self, recv_t: &Type, method: &str) -> Option<zz_checker_core::FuncSig> {
        match recv_t {
            Type::Str => {
                zz_checker_core::lookup_std(&self.funcs, &format!("str.{method}")).cloned()
            }
            Type::Array(_) => {
                zz_checker_core::lookup_std(&self.funcs, &format!("vec.{method}")).cloned()
            }
            Type::Bytes => {
                zz_checker_core::lookup_std(&self.funcs, &format!("bytes.{method}")).cloned()
            }
            Type::Option(_) => {
                zz_checker_core::lookup_std(&self.funcs, &format!("option.{method}")).cloned()
            }
            Type::Result(_, _) => {
                zz_checker_core::lookup_std(&self.funcs, &format!("result.{method}")).cloned()
            }
            Type::Int => {
                zz_checker_core::lookup_std(&self.funcs, &format!("int.{method}")).cloned()
            }
            Type::Float => {
                zz_checker_core::lookup_std(&self.funcs, &format!("float.{method}")).cloned()
            }
            Type::Bool => {
                zz_checker_core::lookup_std(&self.funcs, &format!("bool.{method}")).cloned()
            }
            Type::Response => {
                zz_checker_core::lookup_std(&self.funcs, &format!("http.{method}")).cloned()
            }
            Type::HttpRequest => {
                zz_checker_core::lookup_std(&self.funcs, &format!("http.{method}")).cloned()
            }
            Type::TcpStream => {
                zz_checker_core::lookup_std(&self.funcs, &format!("net.{method}")).cloned()
            }
            Type::TcpListener => {
                zz_checker_core::lookup_std(&self.funcs, &format!("net.{method}")).cloned()
            }
            Type::HttpServer => {
                zz_checker_core::lookup_std(&self.funcs, &format!("http.{method}")).cloned()
            }
            Type::Json => {
                zz_checker_core::lookup_std(&self.funcs, &format!("json.{method}")).cloned()
            }
            Type::Db => {
                zz_checker_core::lookup_std(&self.funcs, &format!("sqlz.{method}")).cloned()
            }
            Type::Chan => {
                zz_checker_core::lookup_std(&self.funcs, &format!("chan.{method}")).cloned()
            }
            Type::TaskJoin => {
                zz_checker_core::lookup_std(&self.funcs, &format!("task.{method}")).cloned()
            }
            Type::Opaque(tag) => {
                zz_checker_core::lookup_std(&self.funcs, &format!("{tag}.{method}")).cloned()
            }
            Type::Struct(sname, _) => {
                let sig =
                    zz_checker_core::lookup_std(&self.funcs, &format!("{sname}.{method}")).cloned();
                if sig.is_some() {
                    return sig;
                }
                if let Some((ns, _)) = sname.rsplit_once('.') {
                    return zz_checker_core::lookup_std(&self.funcs, &format!("{ns}.{method}"))
                        .cloned();
                }
                None
            }
            Type::Enum(ename, _) => {
                let sig =
                    zz_checker_core::lookup_std(&self.funcs, &format!("{ename}.{method}")).cloned();
                if sig.is_some() {
                    return sig;
                }
                if let Some((ns, _)) = ename.rsplit_once('.') {
                    return zz_checker_core::lookup_std(&self.funcs, &format!("{ns}.{method}"))
                        .cloned();
                }
                None
            }
            _ => None,
        }
    }

    fn check_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) -> Type {
        // Selective-import alias, resolved FIRST so the result flows
        // through the direct generic-instantiation path below: a bare
        // `squared` from `import m(squared)` becomes `m.squared` — but
        // ONLY on total miss. Locals, seed entries and synthetic Decls
        // all take precedence; this path exists for generic functions,
        // which have no value binding to find. Bare *uses* still error.
        let rewritten;
        let callee: &Expr = match callee {
            Expr::Ident { name, span }
                if !self.funcs.contains_key(name)
                    && !self.env.iter().any(|s| s.contains_key(name)) =>
            {
                match self.import_aliases.get(name).cloned() {
                    Some(qualified) => {
                        self.used_names.insert(name.clone());
                        rewritten = Expr::Path {
                            parts: qualified.split('.').map(str::to_string).collect(),
                            span: *span,
                        };
                        &rewritten
                    }
                    None => callee,
                }
            }
            c => c,
        };
        // Direct call of a named function: bypass `lookup` so generic
        // functions are instantiated here rather than rejected as values.
        let direct_name = match callee {
            Expr::Ident { name, .. } => Some(name.clone()),
            Expr::Path { parts, .. } => Some(parts.join(".")),
            Expr::Field { obj, name, .. } => {
                match self.check_call_field_callee(obj, name.clone(), args, named, span) {
                    Some(t) => return t,
                    None => None,
                }
            }
            _ => None,
        };
        if let Some(name) = &direct_name {
            // Mark the callee as used so the unused-private-function warning
            // doesn't fire on legitimately-called functions.
            if self.funcs.contains_key(name) {
                self.used_names.insert(name.clone());
            }
            // Removed/renamed stdlib APIs (table-driven): each entry maps old
            // spellings to a replacement hint and the type the removed call
            // used to produce, so downstream checking stays quiet.
            if let Some(ret) = self.check_removed_api(name, args, named, span) {
                return ret;
            }

            // Math constants are values, not functions: `math.PI()` is
            // always an error — use bare `math.PI`.
            if self.is_math_const(name) && self.funcs.contains_key(name) {
                self.used_names.insert(name.clone());
                // Still check the args so nested errors inside them surface.
                for arg in args {
                    self.check_expr(arg);
                }
                for (_, val) in named {
                    self.check_expr(val);
                }
                self.errors.push(error_at(
                    format!(
                        "cannot call `{name}`: it is a numeric constant value, remove the `()`"
                    ),
                    span,
                ));
                return Type::Float;
            }
            // sqlz foundation: `sqlz.query(sql)` / `sqlz.exec(sql)` take an
            // interpolated SQL string. `{expr}` segments are extracted as
            // bound parameters (never inlined), so verification here is:
            // receiver is `db`, SQL arg is `str`, each bound expr is a
            // scalar (int/float/str/bool). Return type unifies with the
            // caller's annotation (`let users: [User] = sqlz.query(...)`).
            //
            // `pg.query(db, sql)` / `pg.exec(db, sql)` (+ `postgres.*` and
            // `std.sqlz.postgres.*` spellings) and `my.query(db, sql)` /
            // `my.exec(db, sql)` (+ `mysql.*`, `std.sqlz.mysql.*`) are the
            // explicit-receiver free-function forms: the SQL is the SECOND
            // user arg.
            //
            // NOTE: this direct-name path only fires for qualified calls
            // where the leading component is the module namespace (not a
            // local). The common method form (`mydb.query(...)` on a
            // handle) is handled in the Path-method branch below, which
            // treats the receiver as implicit.
            let is_pg_call = name == "pg.query"
                || name == "pg.exec"
                || name == "postgres.query"
                || name == "postgres.exec"
                || name == "std.sqlz.postgres.query"
                || name == "std.sqlz.postgres.exec"
                || name == "my.query"
                || name == "my.exec"
                || name == "mysql.query"
                || name == "mysql.exec"
                || name == "std.sqlz.mysql.query"
                || name == "std.sqlz.mysql.exec";
            if name == "sqlz.query"
                || name == "std.sqlz.query"
                || name == "sqlz.exec"
                || name == "std.sqlz.exec"
                || name == "sqlz.transaction"
                || name == "std.sqlz.transaction"
                || is_pg_call
            {
                if let Some(sig) = zz_checker_core::lookup_std(&self.funcs, name).cloned() {
                    // Only take this path when the leading component really
                    // is the module namespace (NOT a local variable).
                    // Otherwise fall through to method dispatch.
                    let recv_is_module = match callee {
                        Expr::Path { parts, .. } => {
                            parts.len() >= 2 && self.lookup_opt(&parts[0]).is_none()
                        }
                        _ => false,
                    };
                    if recv_is_module {
                        self.used_names.insert(name.clone());
                        let (ps, ret, subs) = self.instantiate(&sig);
                        let pnames: Vec<String> =
                            sig.params.iter().map(|(n, _)| n.clone()).collect();
                        self.check_args_against(
                            Some(Self::short_name(name)),
                            &pnames,
                            &ps,
                            &sig.has_default,
                            args,
                            named,
                            span,
                        );
                        // Explicit-receiver forms (`pg.query(db, sql)`,
                        // `sqlz.query(db, sql)`) carry the SQL second;
                        // the bare method-namespace form carries it first.
                        // Skip for transaction — second arg is a closure, not SQL.
                        let is_tx_call =
                            name == "sqlz.transaction" || name == "std.sqlz.transaction";
                        if !is_tx_call {
                            let sql_idx = if is_pg_call || args.len() >= 2 { 1 } else { 0 };
                            if let Some(sql_arg) = args.get(sql_idx) {
                                self.verify_sql_params(sql_arg, span);
                            }
                        }
                        self.validate_bounds(&sig, &subs, span);
                        return ret;
                    }
                }
            }
            // Phase 2.2: `std.http` route + param lint for qualified calls.
            // Runs before generic arg checking so the route's own params are
            // registered before handler bodies are checked. `s.route(...)`
            // arrives as a Path callee (receiver-implicit); `http.route...`
            // as a module call — `lint_http_path_call` sorts out the form.
            if let Expr::Path { parts, .. } = callee {
                self.lint_http_path_call(parts, args, span);
            }
            // NOTE: bare-`Ident` callees are deliberately NOT linted here:
            // genuine selective imports (`import std.http(route)`) are
            // rewritten to qualified `Path` callees at the top of
            // `check_call`, so a bare `route`/`param` reaching this point
            // is user code (e.g. a `@route` decorator) — linting it would
            // false-positive (see `syntax/decorators.zz`).
            // `print`/`println` are variadic (#713): accept any number of
            // positional or named values and check each expression. Like
            // Python/JS, values render space-separated; zero args is legal.
            // A local binding shadows the builtin (`println := |x| ...`);
            // keep the normal call path for it.
            if let Some(t) = self.check_call_print_builtin(name, args, named, span) {
                return t;
            }
            if !self.method_shadow_call(callee, args, named) {
                if let Some(sig) = zz_checker_core::lookup_std(&self.funcs, name).cloned() {
                    self.used_names.insert(name.clone());
                    let (ps, ret, subs) = self.instantiate(&sig);
                    if name == "input" {
                        if args.len() + named.len() > 1 {
                            self.errors.push(error_at(
                                format!(
                                    "expected 0 or 1 arguments, found {}",
                                    args.len() + named.len()
                                ),
                                span,
                            ));
                        } else if args.len() + named.len() == 1 {
                            let arg_expr = if !args.is_empty() {
                                &args[0]
                            } else {
                                &named[0].1
                            };
                            let at = self.check_expr(arg_expr);
                            if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                                self.report_mismatch(e, arg_expr.span());
                            }
                        }
                        return ret;
                    }
                    if name == "range" {
                        let total = args.len() + named.len();
                        if total == 0 || total > 3 {
                            self.errors.push(error_at(
                                format!("range expects 1, 2, or 3 arguments, found {total}"),
                                span,
                            ));
                        } else {
                            for arg in args {
                                let at = self.check_expr(arg);
                                if let Err(e) = self.unifier.unify(&at, &Type::Int) {
                                    self.report_mismatch(e, arg.span());
                                }
                            }
                            for (_, val) in named {
                                let at = self.check_expr(val);
                                if let Err(e) = self.unifier.unify(&at, &Type::Int) {
                                    self.report_mismatch(e, val.span());
                                }
                            }
                        }
                        return ret;
                    }
                    let pnames: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
                    self.check_args_against(
                        Some(Self::short_name(name)),
                        &pnames,
                        &ps,
                        &sig.has_default,
                        args,
                        named,
                        span,
                    );
                    self.validate_bounds(&sig, &subs, span);
                    return ret;
                }
            }
            // Smart suggestion for unknown qualified function calls with a
            // one-edit fixit when unambiguous (e.g. std.env.get_var → std.env.get).
            // Only applies to qualified paths — bare identifiers go to Sherlock
            // below (note only, no fixit). Skips when the head segment is a
            // local variable, so method dispatch gets first crack at
            // `arr.push()`-style calls.
            let head = name.split('.').next().unwrap_or(name);
            let head_is_local = self.env.iter().rev().any(|s| s.contains_key(head));
            if !self.funcs.contains_key(name)
                && matches!(callee, Expr::Path { .. })
                && name.starts_with("std.")
                && !head_is_local
            {
                let candidates: Vec<String> = self
                    .funcs
                    .keys()
                    .flat_map(|k| {
                        let mut v = vec![k.clone()];
                        if let Some(bare) = k.rsplit('.').next() {
                            if bare != k {
                                v.push(bare.to_string());
                            }
                        }
                        v
                    })
                    .collect();
                let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
                let all = suggest_all(name, &refs);
                if !all.is_empty() {
                    let is_unambiguous = all.len() == 1 && all[0].1 <= 1;
                    let callee_span = match callee {
                        Expr::Path { span, .. } => *span,
                        _ => span,
                    };
                    if is_unambiguous {
                        self.errors.push(
                            error_at(format!("unknown function `{name}`"), callee_span)
                                .with_note(format!("did you mean `{}`?", all[0].0))
                                .with_fixit(FixIt::safe(
                                    callee_span,
                                    all[0].0.to_string(),
                                    "replace with suggested function",
                                )),
                        );
                    } else {
                        self.errors.push(
                            error_at(format!("unknown function `{name}`"), callee_span)
                                .with_note(format!("did you mean `{}`?", all[0].0)),
                        );
                    }
                    return Type::Unit;
                }
            }
        }
        // Method call: `p.dist()` resolves to `dist(p, ...)`.
        if let Expr::Path { parts, span: pspan } = callee {
            if parts.len() >= 2 {
                // First check if the full path resolves as a variable
                // (e.g. module-level closure `ns.f`). If so, treat it as
                // a regular call, not a method call.
                let joined = parts.join(".");
                // Enum construction (`Token.IntLit(1)`) reads as a call
                // but builds a value, not a function invocation. Takes
                // priority over method dispatch (no receiver exists) but
                // yields to real functions and locals: an exact `funcs`
                // entry or a shadowing value keeps its meaning.
                let enum_head = parts[..parts.len() - 1].join(".");
                let canonical_head = self.canonical_enum_name(&enum_head);
                if self.enums.contains_key(&canonical_head)
                    && !self.funcs.contains_key(&joined)
                    && self.lookup_opt(&enum_head).is_none()
                {
                    let variant = parts.last().cloned().unwrap_or_default();
                    if let Some(t) =
                        self.check_enum_construction(&canonical_head, &variant, args, named, span)
                    {
                        return t;
                    }
                }
                // Method on a constructed variant (`Token.Eof.is_eof()`
                // parses as a 3+-part path): the call arguments split —
                // the first fills a payload variant, the rest go to the
                // method (`Token.IntLit(1).add(2)` constructs with `1`,
                // calls `add` with `2`). Unit variants delegate to the
                // Field branch with a synthetic receiver.
                if parts.len() >= 3 {
                    let enum_head2 = parts[..parts.len() - 2].join(".");
                    let canonical_head2 = self.canonical_enum_name(&enum_head2);
                    if self.enums.contains_key(&canonical_head2)
                        && !self.funcs.contains_key(&joined)
                        && self.lookup_opt(&enum_head2).is_none()
                    {
                        let variant2 = parts[parts.len() - 2].clone();
                        let method2 = parts.last().cloned().unwrap_or_default();
                        let _pv = self.enum_variant_payload(&canonical_head2, &variant2, *pspan);
                        match _pv {
                            Some(Some(pty)) => {
                                if args.is_empty() {
                                    self.errors.push(error_at(
                                        format!(
                                            "variant `{canonical_head2}.{variant2}` holds a value: pass it before the method arguments (e.g. `{canonical_head2}.{variant2}(v).{method2}(...)`)"
                                        ),
                                        span,
                                    ));
                                    let (gen_vars, _) = self.fresh_enum_vars(&canonical_head2);
                                    return Type::Enum(canonical_head2, gen_vars);
                                }
                                // First argument fills the payload; the
                                // rest are the method's. Generic parameters
                                // instantiate fresh, as in construction.
                                let (gen_vars, gen_map) = self.fresh_enum_vars(&canonical_head2);
                                let at = self.check_expr(&args[0]);
                                let exp = zz_infer::subst(&pty, &gen_map);
                                if let Err(e) = self.unifier.unify(&at, &exp) {
                                    self.report_mismatch(e, args[0].span());
                                }
                                let recv_args: Vec<Type> = gen_vars
                                    .iter()
                                    .map(|v| self.unifier.resolve_deep(v))
                                    .collect();
                                let recv_t = Type::Enum(canonical_head2.clone(), recv_args);
                                let mut sig = self
                                    .funcs
                                    .get(&format!("{canonical_head2}.{method2}"))
                                    .cloned();
                                if sig.is_none() {
                                    if let Some((ns, _)) = canonical_head2.rsplit_once('.') {
                                        sig = zz_checker_core::lookup_std(
                                            &self.funcs,
                                            &format!("{ns}.{method2}"),
                                        )
                                        .cloned();
                                    }
                                }
                                if let Some(sig) = sig {
                                    self.used_names
                                        .insert(format!("{canonical_head2}.{method2}"));
                                    return self.check_method_call(&MethodCall {
                                        sig: &sig,
                                        recv_t: &recv_t,
                                        promoted_recv: None,
                                        method: &method2,
                                        args: &args[1..],
                                        named,
                                        span,
                                    });
                                }
                                self.errors.push(error_at(
                                    format!(
                                        "unknown method `{method2}` for enum `{canonical_head2}`"
                                    ),
                                    span,
                                ));
                                let (gen_vars, _) = self.fresh_enum_vars(&canonical_head2);
                                return Type::Enum(canonical_head2, gen_vars);
                            }
                            Some(None) => {
                                let recv = Expr::Path {
                                    parts: parts[..parts.len() - 1].to_vec(),
                                    span: *pspan,
                                };
                                let field_callee = Expr::Field {
                                    obj: Box::new(recv),
                                    name: parts.last().cloned().unwrap_or_default(),
                                    span: *pspan,
                                };
                                return self.check_call(&field_callee, args, named, span);
                            }
                            // Unknown variant: already reported inside.
                            None => {
                                let (gen_vars, _) = self.fresh_enum_vars(&canonical_head2);
                                return Type::Enum(canonical_head2, gen_vars);
                            }
                        }
                    }
                }
                // A `head.method` call on a genuine local value reads as
                // a method call when the free-function reading is already
                // impossible (see `method_shadow_call`) — e.g. `db.exec`
                // on a local `db`, never the seeded free function.
                let shadowed = self.method_shadow_call(callee, args, named);
                if !shadowed {
                    if let Some(var_ty) = self.lookup_opt(&joined) {
                        let callee_t = self.unifier.resolve(&var_ty);
                        // If the var is still an unresolved inference var, or is
                        // already known to be a Func/Named, treat as a variable
                        // call — not a method call on the first path component.
                        match &callee_t {
                            Type::Func(..) | Type::Named(..) => {
                                let pnames: Vec<String> =
                                    (0..args.len()).map(|i| format!("_{i}")).collect();
                                match callee_t {
                                    Type::Func(ps, ret) => {
                                        self.check_args_against(
                                            None,
                                            &pnames,
                                            &ps,
                                            &[],
                                            args,
                                            named,
                                            span,
                                        );
                                        return *ret;
                                    }
                                    Type::Named(ref nname) => {
                                        if let Some(sig) =
                                            zz_checker_core::lookup_std(&self.funcs, nname).cloned()
                                        {
                                            let (ps, ret, subs) = self.instantiate(&sig);
                                            let pnames: Vec<String> =
                                                sig.params.iter().map(|(n, _)| n.clone()).collect();
                                            self.check_args_against(
                                                Some(Self::short_name(nname)),
                                                &pnames,
                                                &ps,
                                                &sig.has_default,
                                                args,
                                                named,
                                                span,
                                            );
                                            self.validate_bounds(&sig, &subs, span);
                                            return ret;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Type::Var(_) => {
                                // Fresh var from recursive closure pre-binding.
                                // Build a Func type from the args and unify.
                                let arg_types: Vec<Type> =
                                    args.iter().map(|a| self.check_expr(a)).collect();
                                let ret_var = self.unifier.fresh_var();
                                let func_ty = Type::Func(arg_types, Box::new(ret_var.clone()));
                                if let Err(e) = self.unifier.unify(&var_ty, &func_ty) {
                                    self.report_mismatch(e, span);
                                }
                                return self.unifier.resolve(&ret_var);
                            }
                            _ => {}
                        }
                    }
                }
                let method = parts.last().unwrap();
                let recv_t = self.lookup_path(&parts[..parts.len() - 1], *pspan);
                let recv_t_resolved = self.unifier.resolve(&recv_t);
                // Dot-call order for `recv.method(...)`:
                //   1. the receiver's own `impl` method (`{Type}.{method}`);
                //   2. the receiver-implicit free function `method`
                //      (`p.dist()` ≡ `dist(p)`);
                //   3. the receiver-typed stdlib namespace (`str.`/`vec.`/…)
                //      and the cross-module `{ns}.{method}` fallback below.
                // (1) keeps a same-named free function from silently making
                // the method unreachable (#500); the free signature is
                // restored at the end when nothing matches, so its mismatch
                // diagnostics are unchanged.
                let impl_owned = self.receiver_impl_method(&recv_t_resolved, method);
                let free_sig = zz_checker_core::lookup_std(&self.funcs, method).cloned();
                let mut sig = if impl_owned { None } else { free_sig.clone() };
                if sig.is_none() {
                    match &recv_t_resolved {
                        Type::Str => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("str.{method}"))
                                    .cloned();
                        }
                        Type::Array(_) => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("vec.{method}"))
                                    .cloned();
                        }
                        Type::Option(_) => {
                            sig = zz_checker_core::lookup_std(
                                &self.funcs,
                                &format!("option.{method}"),
                            )
                            .cloned();
                        }
                        Type::Result(_, _) => {
                            sig = zz_checker_core::lookup_std(
                                &self.funcs,
                                &format!("result.{method}"),
                            )
                            .cloned();
                        }
                        Type::Int => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("int.{method}"))
                                    .cloned();
                        }
                        Type::Float => {
                            sig = zz_checker_core::lookup_std(
                                &self.funcs,
                                &format!("float.{method}"),
                            )
                            .cloned();
                        }
                        Type::Bool => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("bool.{method}"))
                                    .cloned();
                        }
                        Type::Response => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("http.{method}"))
                                    .cloned();
                        }
                        Type::HttpRequest => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("http.{method}"))
                                    .cloned();
                        }
                        Type::TcpStream => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("net.{method}"))
                                    .cloned();
                        }
                        Type::TcpListener => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("net.{method}"))
                                    .cloned();
                        }
                        Type::HttpServer => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("http.{method}"))
                                    .cloned();
                        }
                        Type::Json => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("json.{method}"))
                                    .cloned();
                        }
                        Type::Db => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("sqlz.{method}"))
                                    .cloned();
                        }
                        Type::Chan => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("chan.{method}"))
                                    .cloned();
                        }
                        Type::TaskJoin => {
                            sig =
                                zz_checker_core::lookup_std(&self.funcs, &format!("task.{method}"))
                                    .cloned();
                        }
                        Type::Opaque(tag) => {
                            sig = zz_checker_core::lookup_std(
                                &self.funcs,
                                &format!("{tag}.{method}"),
                            )
                            .cloned();
                        }
                        Type::Struct(sname, _) => {
                            // Try TypeName.method (impl block methods)
                            sig = zz_checker_core::lookup_std(
                                &self.funcs,
                                &format!("{sname}.{method}"),
                            )
                            .cloned();
                            if sig.is_none() {
                                // Try namespace.method (cross-module)
                                if let Some((ns, _)) = sname.rsplit_once('.') {
                                    sig = zz_checker_core::lookup_std(
                                        &self.funcs,
                                        &format!("{ns}.{method}"),
                                    )
                                    .cloned();
                                }
                            }
                        }
                        // Enum values erase to `Object`s, so `impl Enum`
                        // methods dispatch exactly like struct methods.
                        Type::Enum(ename, _) => {
                            sig = zz_checker_core::lookup_std(
                                &self.funcs,
                                &format!("{ename}.{method}"),
                            )
                            .cloned();
                            if sig.is_none() {
                                if let Some((ns, _)) = ename.rsplit_once('.') {
                                    sig = zz_checker_core::lookup_std(
                                        &self.funcs,
                                        &format!("{ns}.{method}"),
                                    )
                                    .cloned();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Embedded promotion (see the `Field`-callee branch above).
                let mut promoted_recv: Option<Type> = None;
                if sig.is_none() {
                    if let Type::Struct(sname, sargs) = self.unifier.resolve(&recv_t) {
                        if let Some((defining, psig)) = self.find_struct_method(&sname, method) {
                            // The runtime passes the embedded value itself
                            // as the receiver (with its own arguments).
                            promoted_recv = self
                                .promoted_method_receiver(&sname, &sargs, &defining)
                                .or_else(|| Some(Type::Struct(defining, Vec::new())));
                            sig = Some(psig);
                        }
                    }
                }
                if sig.is_none() {
                    sig = free_sig;
                }
                if let Some(sig) = sig {
                    // sqlz method form: `db.exec(sql)`, `db.query(sql)`,
                    // `db.transaction(fn(tx) { ... })` — receiver is
                    // implicit, so user args are matched against sig[1..].
                    if (*method == "query" || *method == "exec" || *method == "transaction")
                        && matches!(self.unifier.resolve(&recv_t), Type::Db)
                    {
                        let (ps, ret, subs) = self.instantiate(&sig);
                        if !ps.is_empty() {
                            if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
                                self.report_mismatch(e, *pspan);
                            }
                        }
                        let expected = ps.len().saturating_sub(1);
                        let actual = args.len() + named.len();
                        if actual != expected {
                            self.errors.push(error_at(
                                format!("expected {expected} argument(s), found {actual}"),
                                span,
                            ));
                        } else if expected >= 1 {
                            // Verify each user arg against sig[1..].
                            for (i, arg) in args.iter().enumerate() {
                                if ps.len() >= 2 + i {
                                    // Propagate expected types into
                                    // closures so their bodies can
                                    // resolve method calls on known
                                    // param types (e.g. `tx.exec`
                                    // when `tx: Db`).
                                    if let Expr::Closure {
                                        params,
                                        ret_ty,
                                        body,
                                        span: cspan,
                                    } = arg
                                    {
                                        let expected_t = self.unifier.resolve(&ps[1 + i]);
                                        let ep = if let Type::Func(ep, _) = &expected_t {
                                            Some(ep.as_slice())
                                        } else {
                                            None
                                        };
                                        let at = self.check_closure(
                                            params,
                                            ret_ty.as_ref(),
                                            body,
                                            *cspan,
                                            ep,
                                        );
                                        if let Err(e) = self.unifier.unify(&at, &ps[1 + i]) {
                                            self.report_mismatch(e, arg.span());
                                        }
                                    } else {
                                        let at = self.check_expr(arg);
                                        if let Err(e) = self.unifier.unify(&at, &ps[1 + i]) {
                                            self.report_mismatch(e, arg.span());
                                        }
                                    }
                                }
                            }
                            if *method == "query" || *method == "exec" {
                                self.verify_sql_params(&args[0], span);
                            }
                        }
                        self.validate_bounds(&sig, &subs, span);
                        return ret;
                    }
                    let (ps, ret, subs) = self.instantiate(&sig);
                    if ps.is_empty() {
                        self.errors.push(error_at(
                            format!("method `{method}` takes no arguments"),
                            span,
                        ));
                        return Type::Unit;
                    }
                    if let Some(promoted) = promoted_recv {
                        if let Err(e) = self.unifier.unify(&promoted, &ps[0]) {
                            self.report_mismatch(e, *pspan);
                        }
                    } else if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
                        self.report_mismatch(e, *pspan);
                    }
                    self.check_args_against(
                        None,
                        &sig.params[1..]
                            .iter()
                            .map(|(n, _)| n.clone())
                            .collect::<Vec<_>>(),
                        &ps[1..],
                        &[],
                        args,
                        named,
                        span,
                    );
                    self.validate_bounds(&sig, &subs, span);
                    return ret;
                }
            }
        }
        let callee_t = self.check_expr(callee);
        let callee_t = self.unifier.resolve(&callee_t);
        match callee_t {
            Type::Func(ps, ret) => {
                let pnames: Vec<String> = (0..ps.len()).map(|i| format!("_{i}")).collect();
                self.check_args_against(None, &pnames, &ps, &[], args, named, span);
                *ret
            }
            Type::Named(name) => match zz_checker_core::lookup_std(&self.funcs, &name).cloned() {
                Some(sig) => {
                    let (ps, ret, subs) = self.instantiate(&sig);
                    let param_names: Vec<String> =
                        sig.params.iter().map(|(n, _)| n.clone()).collect();
                    self.check_args_against(
                        Some(Self::short_name(&name)),
                        &param_names,
                        &ps,
                        &sig.has_default,
                        args,
                        named,
                        span,
                    );
                    self.validate_bounds(&sig, &subs, span);
                    ret
                }
                None => {
                    // Sherlock: suggest the closest known function so a typo
                    // (`lenght`, `get_var`) points at the fix instead of a dead end.
                    let mut diag = error_at(format!("unknown function `{name}`"), span);
                    let candidates: Vec<String> = self
                        .funcs
                        .keys()
                        .flat_map(|k| {
                            let mut v = vec![k.clone()];
                            if let Some(bare) = k.rsplit('.').next() {
                                if bare != k {
                                    v.push(bare.to_string());
                                }
                            }
                            v
                        })
                        .collect();
                    let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
                    let all = suggest_all(&name, &refs);
                    if let Some((suggestion, _)) = all.first() {
                        // Note only, deliberately: the suggestion names the
                        // *type* (`name`), not the callee value, and the
                        // span covers the whole call — replacing the callee
                        // with the function would silently drop a receiver,
                        // so no fixit is offered.
                        diag = diag.with_note(format!(
                            "did you mean to call `{suggestion}` instead of calling a value of type `{name}`?"
                        ));
                    }
                    self.errors.push(diag);
                    Type::Unit
                }
            },
            Type::Var(_) => {
                if !self.had_undefined_var {
                    self.errors.push(error_at(
                        "cannot call a value whose type could not be inferred",
                        span,
                    ));
                }
                Type::Error
            }
            Type::Error => Type::Error,
            other => {
                if !self.had_undefined_var {
                    self.errors.push(error_at(
                        format!("cannot call a value of type `{other}`"),
                        span,
                    ));
                }
                Type::Error
            }
        }
    }

    /// Check that the given positional and named arguments match the parameter
    /// types.  `has_default` indicates which trailing parameters have defaults;
    /// callers may omit those.
    fn check_method_call(&mut self, call: &MethodCall<'_>) -> Type {
        let (ps, ret, subs) = self.instantiate(call.sig);
        if ps.is_empty() {
            self.errors.push(error_at(
                format!("method `{}` takes no arguments", call.method),
                call.span,
            ));
            return Type::Unit;
        }
        if let Some(promoted) = &call.promoted_recv {
            if let Err(e) = self.unifier.unify(promoted, &ps[0]) {
                self.report_mismatch(e, call.span);
            }
        } else if let Err(e) = self.unifier.unify(call.recv_t, &ps[0]) {
            self.report_mismatch(e, call.span);
        }
        self.check_args_against(
            Some(call.method),
            &call.sig.params[1..]
                .iter()
                .map(|(n, _)| n.clone())
                .collect::<Vec<_>>(),
            &ps[1..],
            &[],
            call.args,
            call.named,
            call.span,
        );
        self.validate_bounds(call.sig, &subs, call.span);
        ret
    }

    /// Short display name for call diagnostics: `math.sin` → `sin`.
    fn short_name(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    /// Sherlock notes for an argument/parameter mismatch: name the
    /// parameter and its expected type so the error points at the fix,
    /// not just the conflict. Placeholder names (`_0`, from `func`
    /// values) still show the type.
    fn param_mismatch_notes(param_names: &[String], ps: &[Type], i: usize) -> Vec<String> {
        match (param_names.get(i), ps.get(i)) {
            (Some(name), Some(ty)) => {
                vec![format!("parameter `{name}` expects `{ty}`")]
            }
            _ => Vec::new(),
        }
    }

    // Eight args is the honest shape here (callee + params + args + span);
    // a struct would churn every call site for no checking benefit.
    #[allow(clippy::too_many_arguments)]
    fn check_args_against(
        &mut self,
        callee: Option<&str>,
        param_names: &[String],
        ps: &[Type],
        has_default: &[bool],
        args: &[Expr],
        named: &[(Ident, Expr)],
        span: Span,
    ) {
        if check_call_args::check_arg_count(
            self,
            callee,
            param_names,
            ps,
            has_default,
            args,
            named,
            span,
        ) {
            return;
        }

        let total_params = ps.len();
        let mut slots: Vec<Option<&Expr>> = vec![None; total_params];

        if check_call_args::assign_positional_slots(self, total_params, args, &mut slots) {
            return;
        }

        if check_call_args::check_named_args(self, param_names, named, &mut slots) {
            return;
        }

        check_call_args::check_filled_slots(self, param_names, ps, &slots);
    }

    /// Compile-time SQL verification for `db.query(sql)` / `db.exec(sql)`.
    ///
    /// The SQL arg is normally an interpolated string (`Fmt`): static text
    /// segments stay in the prepared statement, `{expr}` segments become
    /// `?N` bound parameters. This check ensures every bound expr is a
    /// bindable scalar (int/float/str/bool) and rejects format specs
    /// (`{x:.2f}` would render client-side, breaking parameterization).
    /// Static text is scanned for balanced quotes/parens as a cheap
    /// syntax sanity check; full schema verification happens at runtime
    /// against the live connection.
    fn verify_sql_params(&mut self, sql_arg: &Expr, span: Span) {
        let parts = match sql_arg {
            Expr::Fmt { parts, .. } => Some(parts.clone()),
            Expr::Str { .. } => None,
            Expr::Paren { expr, .. } => match expr.as_ref() {
                Expr::Fmt { parts, .. } => Some(parts.clone()),
                Expr::Str { .. } => None,
                _ => {
                    let at = self.check_expr(sql_arg);
                    if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                        self.report_mismatch(e, sql_arg.span());
                    }
                    return;
                }
            },
            _ => {
                let at = self.check_expr(sql_arg);
                if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                    self.report_mismatch(e, sql_arg.span());
                }
                return;
            }
        };
        let Some(parts) = parts else { return };
        let mut static_text = String::new();
        for part in &parts {
            match part {
                FmtPart::Text(t) => static_text.push_str(t),
                FmtPart::Expr(e, spec) => {
                    if let Some(s) = spec {
                        self.errors.push(error_at(
                            format!(
                                "format spec `:{s}` not allowed in SQL interpolation; use plain `{{...}}` so the value is bound as a parameter"
                            ),
                            e.span(),
                        ));
                    }
                    let bt = self.check_expr(e);
                    let rt = self.unifier.resolve(&bt);
                    match rt {
                        Type::Int
                        | Type::Float
                        | Type::Str
                        | Type::Bool
                        | Type::Var(_)
                        | Type::Error => {}
                        other => {
                            self.errors.push(error_at(
                                format!(
                                    "SQL parameter must be int, float, str, or bool, found `{other}`"
                                ),
                                e.span(),
                            ));
                        }
                    }
                }
            }
        }
        if let Some(msg) = check_sql_static(&static_text) {
            self.errors.push(error_at(msg, span));
        }
    }

    /// `expected_params`: when a closure is passed to a function with a
    /// known signature, the caller can provide the expected parameter types
    /// here. For each unannotated param, if an expected type exists, the
    /// fresh var is unified with it *before* checking the body — so the
    /// body sees known param types and can resolve method calls (e.g.
    /// `tx.exec(...)` when `tx: Db`).
    fn check_closure(
        &mut self,
        params: &[Param],
        ret_ty: Option<&Ty>,
        body: &Expr,
        _span: Span,
        expected_params: Option<&[Type]>,
    ) -> Type {
        self.push_scope();
        let mut ptypes = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let ty = match &p.ty {
                Some(t) => {
                    let gens = self.current_generics.clone();
                    self.ast_to_type(t, &gens)
                }
                None => {
                    if let Some(ep) = expected_params.and_then(|eps| eps.get(i)) {
                        // Bind the fresh var to the expected type so the
                        // body can resolve method calls on known types.
                        let fv = self.unifier.fresh_var();
                        if let Err(e) = self.unifier.unify(&fv, ep) {
                            self.report_mismatch(e, p.span);
                        }
                        fv
                    } else {
                        self.unifier.fresh_var()
                    }
                }
            };
            // Closure parameters are dictated by the callee's signature
            // (`.route(..., |req| ...)`, `map(|x| ...)`): they cannot be
            // removed, so an unread one is not a mistake worth warning
            // about. Named-function parameters keep the warning.
            self.define_binding_at(&p.name.name, ty.clone(), p.name.span, false);
            ptypes.push(ty);
        }
        // Allow `return` inside closures — same semantics as named functions.
        // `try` inside a closure without an explicit `-> Result/Option`
        // annotation is a compile error (innermost enclosing scope rule).
        if ret_ty.is_none() && Self::expr_contains_try(body) {
            self.errors.push(error_at(
                "closure must declare `-> Result<T, E>` to use `try`",
                body.span(),
            ));
        }
        let ret_var = self.unifier.fresh_var();
        let prev_ret = self.current_ret.replace(ret_var.clone());
        let prev_tail = std::mem::replace(&mut self.tail_is_value, true);
        let bt = self.check_expr(body);
        self.tail_is_value = prev_tail;
        self.current_ret = prev_ret;
        self.pop_scope();
        // Unify body type with the return var (from any `return` statements).
        if let Err(e) = self.unifier.unify(&ret_var, &bt) {
            self.report_mismatch(e, body.span());
        }
        let mut resolved_ret = self.unifier.resolve(&ret_var);
        // If a return type annotation is provided, unify it with the inferred return type.
        if let Some(ann) = ret_ty {
            let gens = self.current_generics.clone();
            let ann_type = self.ast_to_type(ann, &gens);
            if let Err(e) = self.unifier.unify(&ann_type, &resolved_ret) {
                self.report_mismatch(e, ann.span);
            }
            resolved_ret = self.unifier.resolve(&ann_type);
        }
        Type::Func(ptypes, Box::new(resolved_ret))
    }

    /// Innermost span of a match-guard subexpression the native backend
    /// cannot lower (`None` when the whole guard is in the scalar subset:
    /// idents, paths, int/float/bool literals, and binary/unary/paren
    /// combinations). Native guard lowering folds anything else to a
    /// constant while the VM evaluates it fully — a silent engine
    /// divergence the caller turns into a loud check-time error.
    fn native_unsupported_guard(e: &Expr) -> Option<Span> {
        match e {
            Expr::Ident { .. }
            | Expr::Path { .. }
            | Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. } => None,
            Expr::Paren { expr, .. } => Self::native_unsupported_guard(expr),
            Expr::Unary { expr, .. } => Self::native_unsupported_guard(expr),
            Expr::Binary { left, right, .. } => Self::native_unsupported_guard(left)
                .or_else(|| Self::native_unsupported_guard(right)),
            _ => Some(e.span()),
        }
    }

    fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[zz_span_ast::ast::MatchArm],
        span: Span,
    ) -> Type {
        let st = self.check_expr(scrutinee);
        let st = self.unifier.resolve(&st);
        self.check_exhaustive(&st, arms, span);
        // Sherlock: arms after a catch-all arm can never run. A guard
        // disqualifies the catch-all (it may fail and fall through), so
        // only guard-free `_`/binding patterns count. Warning, not error:
        // the program still runs, but the arm is dead code.
        fn pat_is_catchall(pat: &zz_span_ast::ast::Pattern) -> bool {
            match pat {
                zz_span_ast::ast::Pattern::Wildcard { .. } => true,
                zz_span_ast::ast::Pattern::Binding { .. } => true,
                zz_span_ast::ast::Pattern::Or { pats, .. } => pats.iter().any(pat_is_catchall),
                _ => false,
            }
        }
        let mut seen_catchall = false;
        for arm in arms {
            if seen_catchall {
                self.errors.push(
                    warning_at(
                        "unreachable match arm — a previous arm matches everything",
                        arm.pat.span(),
                    )
                    .with_note("remove this arm or move it above the catch-all arm"),
                );
            }
            if arm.guard.is_none() && pat_is_catchall(&arm.pat) {
                seen_catchall = true;
            }
        }
        let mut result: Option<Type> = None;
        let mut saw_arm = false;
        for arm in arms {
            saw_arm = true;
            self.push_scope();
            self.bind_pattern(&arm.pat, &st);
            // Check match guard: must resolve to bool
            if let Some(ref guard) = arm.guard {
                let gt = self.check_expr(guard);
                if let Err(e) = self.unifier.unify(&gt, &Type::Bool) {
                    self.report_mismatch(e, guard.span());
                } else if let Some(bad) = Self::native_unsupported_guard(guard) {
                    // Native builds lower guards to raw C scalars (idents,
                    // paths, scalar literals, binary/unary/paren combos).
                    // Anything else degrades to a constant there while the
                    // VM evaluates it fully — a silent engine divergence.
                    // Reject loudly so both engines agree.
                    self.errors.push(error_at(
                        "match guard uses an expression native builds cannot evaluate",
                        bad,
                    ).with_note(
                        "hint: guards support pattern bindings, literals, and operators only; move calls, strings, or field access into the arm body with if/else",
                    ));
                }
            }
            let bt = self.check_expr(&arm.body);
            self.pop_scope();
            // Sherlock: a bare `match` statement discards the match
            // value, and bare arm bodies never pass through `Stmt::Expr`
            // — warn here when an arm yields a discarded `Result` or
            // `Option`. Block arm bodies are covered by the normal
            // statement path (`direct_discard` is statement-level, so
            // used positions like call args stay silent).
            if self.direct_discard && !matches!(arm.body, Expr::Block(_)) {
                self.warn_if_discarded(&arm.body, &bt);
            }
            // `break`/`continue` arms diverge (never produce a value),
            // so they don't constrain the match's result type. Arms that
            // diverge via `return` (`Never`) vanish from the join the
            // same way: only arms that can yield values constrain each
            // other (join(Never, T) = T).
            if matches!(arm.body, Expr::Break { .. } | Expr::Continue { .. }) {
                continue;
            }
            if matches!(self.unifier.resolve(&bt), Type::Never) {
                continue;
            }
            match &result {
                Some(r) => {
                    if let Err(e) = self.unifier.unify(&bt, r) {
                        self.report_mismatch(e, arm.body.span());
                    }
                }
                None => result = Some(bt),
            }
        }
        // No arms (rejected by exhaustiveness above, but be total):
        // keep the old unit type. Arms that all diverge yield Never.
        if !saw_arm {
            return Type::Unit;
        }
        result.unwrap_or(Type::Never)
    }

    fn check_try(&mut self, expr: &Expr, span: Span) -> Type {
        let ot = self.check_expr(expr);
        let ot = self.unifier.resolve(&ot);
        let ret = match &self.current_ret {
            Some(r) => self.unifier.resolve(r),
            None => {
                self.errors.push(error_at(
                    "`?`/`try` can only be used inside a function returning `Result` or `Option`",
                    span,
                ));
                return Type::Error;
            }
        };
        match ot {
            Type::Option(t) => match &ret {
                Type::Option(_) => {
                    self.try_resolutions.insert(span, None);
                    *t
                }
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Option(t.clone()));
                    self.try_resolutions.insert(span, None);
                    *t
                }
                other => {
                    self.errors.push(
                        error_at(
                            format!("`?` on `Option` cannot propagate through a function returning `{other}`"),
                            span,
                        )
                        .with_note(format!(
                            "to use `?` here, change the return type to `Option<{t}>`"
                        )),
                    );
                    *t
                }
            },
            Type::Result(t, e) => match &ret {
                Type::Result(_, ret_e) => {
                    let ein = self.unifier.resolve(&e);
                    let eout = self.unifier.resolve(ret_e);
                    if ein == eout {
                        // Identity conversion: zero-cost, inlined away.
                        self.try_resolutions.insert(span, None);
                        if let Err(err) = self.unifier.unify(&e, ret_e) {
                            self.report_mismatch(err, span);
                        }
                    } else if matches!(ein, Type::Var(_)) || matches!(eout, Type::Var(_)) {
                        if let Err(err) = self.unifier.unify(&e, ret_e) {
                            self.report_mismatch(err, span);
                        }
                        self.try_resolutions.insert(span, None);
                    } else {
                        // Different error types: need a `convert_to_` impl.
                        let hits = self.find_converts(&ein, &eout);
                        match hits.len() {
                            1 => {
                                self.try_resolutions
                                    .insert(span, hits.into_iter().next().unwrap_or(None));
                            }
                            0 => {
                                self.errors.push(error_at(
                                    format!(
                                        "no conversion path for `try`: error type `{ein}` cannot convert to `{eout}`\n\
                                         hint: add `impl {ein} {{ func convert_to_{eout}(self) -> {eout} {{ ... }} }}`",
                                    ),
                                    span,
                                ));
                            }
                            _ => {
                                // Defensive: V1 registration rejects a second
                                // convert from the same source type, and the
                                // seeded-funcs fallback stops at the first hit,
                                // so multiple hits are currently unconstructible.
                                // Kept so a future relaxed registry still fails
                                // loudly at the `try` site per spec.
                                self.errors.push(error_at(
                                    format!(
                                        "ambiguous conversion for `try`: {} impls convert `{ein}` to `{eout}`",
                                        hits.len()
                                    ),
                                    span,
                                ));
                            }
                        }
                    }
                    *t
                }
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Result(t.clone(), e.clone()));
                    self.try_resolutions.insert(span, None);
                    *t
                }
                other => {
                    self.errors.push(
                        error_at(
                            format!("`?` on `Result` cannot propagate through a function returning `{other}`"),
                            span,
                        )
                        .with_note(format!(
                            "to use `?` here, change the return type to `Result<{t}, {e}>`"
                        )),
                    );
                    *t
                }
            },
            Type::Var(_) => {
                self.errors.push(error_at(
                    "cannot use `?` on a value whose type could not be inferred",
                    span,
                ));
                Type::Error
            }
            other => {
                self.errors.push(
                    error_at(format!("cannot use `?` on a value of type `{other}`"), span)
                        .with_note(format!(
                            "`?` unwraps `Result`/`Option`; `{other}` is neither — remove the `?`"
                        )),
                );
                Type::Error
            }
        }
    }

    /// True when an expression tree contains a `Try` (`?` / `try`) node.
    fn expr_contains_try(e: &Expr) -> bool {
        match e {
            Expr::Try { .. } => true,
            Expr::Binary { left, right, .. } => {
                Self::expr_contains_try(left) || Self::expr_contains_try(right)
            }
            Expr::Unary { expr, .. } => Self::expr_contains_try(expr),
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                Self::expr_contains_try(callee)
                    || args.iter().any(Self::expr_contains_try)
                    || named.iter().any(|(_, v)| Self::expr_contains_try(v))
            }
            Expr::Field { obj, .. } => Self::expr_contains_try(obj),
            Expr::Index { obj, index, .. } => {
                Self::expr_contains_try(obj) || Self::expr_contains_try(index)
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                Self::expr_contains_try(obj)
                    || start.as_ref().is_some_and(|s| Self::expr_contains_try(s))
                    || end.as_ref().is_some_and(|s| Self::expr_contains_try(s))
            }
            Expr::Array { elems, .. } => elems.iter().any(Self::expr_contains_try),
            Expr::Dict { entries, .. } => entries
                .iter()
                .any(|(k, v)| Self::expr_contains_try(k) || Self::expr_contains_try(v)),
            Expr::Tuple { items, .. } => items.iter().any(Self::expr_contains_try),
            Expr::If {
                cond, then, els, ..
            } => {
                Self::expr_contains_try(cond)
                    || then.stmts.iter().any(Self::stmt_contains_try)
                    || els.as_ref().is_some_and(|x| Self::expr_contains_try(x))
            }
            Expr::While { cond, body, .. } => {
                Self::expr_contains_try(cond) || body.stmts.iter().any(Self::stmt_contains_try)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                Self::expr_contains_try(scrutinee)
                    || arms.iter().any(|a| {
                        Self::expr_contains_try(&a.body)
                            || a.guard.as_ref().is_some_and(Self::expr_contains_try)
                    })
            }
            Expr::IfLet {
                value, then, els, ..
            } => {
                Self::expr_contains_try(value)
                    || then.stmts.iter().any(Self::stmt_contains_try)
                    || els.as_ref().is_some_and(|x| Self::expr_contains_try(x))
            }
            Expr::Block(b) => b.stmts.iter().any(Self::stmt_contains_try),
            // Nested closures/fns establish their own return scope and get
            // their own `check_closure` diagnostic — don't attribute their
            // `try` to the enclosing closure.
            Expr::Closure { .. } => false,
            Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
                FmtPart::Expr(x, _) => Self::expr_contains_try(x),
                _ => false,
            }),
            Expr::Paren { expr, .. } => Self::expr_contains_try(expr),
            Expr::StructInit { fields, .. } => {
                fields.iter().any(|(_, v)| Self::expr_contains_try(v))
            }
            Expr::Variant { arg, .. } => arg.as_ref().is_some_and(|a| Self::expr_contains_try(a)),
            Expr::ListComp {
                body, iter, filter, ..
            } => {
                Self::expr_contains_try(body)
                    || Self::expr_contains_try(iter)
                    || filter.as_ref().is_some_and(|f| Self::expr_contains_try(f))
            }
            Expr::Range { start, end, .. } => {
                Self::expr_contains_try(start) || Self::expr_contains_try(end)
            }
            _ => false,
        }
    }

    fn stmt_contains_try(s: &Stmt) -> bool {
        match s {
            Stmt::Expr(e) => Self::expr_contains_try(e),
            Stmt::Decl { value, .. } => Self::expr_contains_try(value),
            Stmt::Assign { value, target, .. } => {
                Self::expr_contains_try(value) || Self::expr_contains_try(target)
            }
            Stmt::Destructure { value, .. } => Self::expr_contains_try(value),
            Stmt::Return { value, .. } => value.as_ref().is_some_and(Self::expr_contains_try),
            Stmt::For { iter, body, .. } => {
                Self::expr_contains_try(iter) || body.stmts.iter().any(Self::stmt_contains_try)
            }
            Stmt::Defer { expr, .. } => Self::expr_contains_try(expr),
            // Nested named functions/methods own their return scope.
            Stmt::Func { .. } | Stmt::Impl { .. } => false,
            _ => false,
        }
    }

    /// Find conversion impl spans from `from` to `to`.
    /// Checks the local `convert_impls` registry plus seeded `ext_methods` /
    /// `funcs` (cross-module `convert_to_` methods) so multi-file programs work.
    fn find_converts(&self, from: &Type, to: &Type) -> Vec<Option<Span>> {
        let mut out = Vec::new();
        let from_s = from.to_string();
        let to_s = to.to_string();
        for c in &self.convert_impls {
            if c.from == *from && c.to == *to {
                out.push(Some(c.span));
            }
        }
        if !out.is_empty() {
            return out;
        }
        // Fallback: scan merged registries for `X.convert_to_Y` with matching sig.
        let mut seen_fn = std::collections::HashSet::new();
        for ((tkey, mname), (sig, mspan)) in &self.ext_methods {
            if !mname.starts_with("convert_to_") || sig.params.is_empty() {
                continue;
            }
            if sig.params[0].1 == *from && sig.ret == *to {
                let _ = tkey;
                if seen_fn.insert(sig.ret.to_string() + &from_s + &to_s) {
                    out.push(Some(*mspan));
                }
            }
        }
        if !out.is_empty() {
            return out;
        }
        for (fname, sig) in &self.funcs {
            if let Some((tkey, mname)) = fname.rsplit_once('.') {
                if !mname.starts_with("convert_to_") || sig.params.is_empty() {
                    continue;
                }
                let _ = tkey;
                if sig.params[0].1 == *from && sig.ret == *to {
                    out.push(None);
                    break;
                }
            }
        }
        out
    }

    // --- patterns ---------------------------------------------------------

    fn bind_pattern(&mut self, pat: &Pattern, ty: &Type) {
        match pat {
            Pattern::Wildcard { .. } => {}
            Pattern::Binding { name } => {
                self.define_at(&name.name, ty.clone(), name.span);
                // Record the binding-site span type: destructured (and
                // match-arm) names otherwise leave no span entry, so
                // lowering resolves their method calls untyped and
                // multi-namespace methods misresolve (#417).
                self.span_types
                    .insert(self.scope_key(name.span), ty.clone());
                // Top-level destructured names are module globals (like
                // top-level `Decl`): record them for `tp.bindings` so
                // downstream passes (native globals collection) can type
                // them. Nested positions have env.len() > 1 (blocks push
                // scopes) and are excluded, mirroring `Stmt::Decl`.
                if self.env.len() == 1 {
                    self.new_bindings.insert(name.name.clone(), ty.clone());
                }
            }
            Pattern::Literal { value, span } => {
                let lit_t = match value {
                    Lit::Int(_) => Type::Int,
                    Lit::Float(_) => Type::Float,
                    Lit::Str(_) => Type::Str,
                    Lit::Bool(_) => Type::Bool,
                };
                if let Err(e) = self.unifier.unify(&lit_t, ty) {
                    self.report_mismatch(e, *span);
                }
            }
            Pattern::Variant { name, arg, span } => {
                self.bind_pattern_variant(name, arg, *span, ty);
            }
            Pattern::Or { pats, span } => {
                if pats.is_empty() {
                    return;
                }
                // Snapshot current scope keys so we can diff what the
                // first alternative binds.
                let before: std::collections::HashSet<String> = self
                    .env
                    .last()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                self.bind_pattern(&pats[0], ty);
                let first_new: std::collections::HashMap<String, Type> = self
                    .env
                    .last()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(k, _)| !before.contains(k))
                    .collect();
                let first_names: std::collections::HashSet<String> =
                    first_new.keys().cloned().collect();
                for alt in &pats[1..] {
                    // Bind alternative in a throwaway scope (no unused
                    // warnings) to collect its bindings for comparison.
                    self.env.push(std::collections::HashMap::new());
                    self.defined_names.push(std::collections::HashMap::new());
                    self.const_env.push(std::collections::HashMap::new());
                    self.bind_pattern(alt, ty);
                    let alt_map = self.env.pop().unwrap_or_default();
                    self.defined_names.pop();
                    self.const_env.pop();
                    let alt_names: std::collections::HashSet<String> =
                        alt_map.keys().cloned().collect();
                    if alt_names != first_names {
                        self.errors.push(error_at(
                            "or-pattern alternatives must bind the same names",
                            *span,
                        ));
                        continue;
                    }
                    for (name, alt_ty) in alt_map {
                        if let Some(first_ty) = first_new.get(&name) {
                            if let Err(e) = self.unifier.unify(&alt_ty, first_ty) {
                                self.report_mismatch(e, alt.span());
                            }
                        }
                    }
                }
            }
            Pattern::Tuple { pats, span } => {
                let rt = self.unifier.resolve(ty);
                match rt {
                    Type::Tuple(inner_types) => {
                        if pats.len() != inner_types.len() {
                            self.errors.push(error_at(
                                format!(
                                    "expected tuple with {} elements, found {}",
                                    inner_types.len(),
                                    pats.len()
                                ),
                                *span,
                            ));
                        } else {
                            for (pat, inner_ty) in pats.iter().zip(inner_types.iter()) {
                                self.bind_pattern(pat, inner_ty);
                            }
                        }
                    }
                    Type::Var(_) => {
                        // If type is unknown, bind all patterns to fresh vars
                        let fv = self.unifier.fresh_var();
                        for pat in pats {
                            self.bind_pattern(pat, &fv);
                        }
                    }
                    other => {
                        self.errors.push(error_at(
                            format!(
                                "cannot destructure a value of type `{other}` into a tuple pattern"
                            ),
                            *span,
                        ));
                    }
                }
            }
        }
    }

    /// Bind a `.Variant` pattern: constrains the scrutinee type for
    /// builtin variants (`.ok`/`.err` → `Result`, `.some`/`.none` → `Option`),
    /// then binds payload names to the resolved inner type.
    fn bind_pattern_variant(
        &mut self,
        name: &str,
        arg: &Option<Box<Pattern>>,
        span: Span,
        ty: &Type,
    ) {
        // Sherlock: a builtin pattern constrains an uninferred
        // scrutinee — `.ok`/`.err` proves `Result`, `.some`/`.none`
        // proves `Option`. A later arm from the other family then
        // reports a real mismatch instead of passing silently, and
        // payloads bind to known types instead of fresh vars.
        if let Type::Var(id) = self.unifier.resolve(ty) {
            let constrained = match name {
                "ok" | "err" => Some(Type::Result(
                    Box::new(self.unifier.fresh_var()),
                    Box::new(self.unifier.fresh_var()),
                )),
                "some" | "none" => Some(Type::Option(Box::new(self.unifier.fresh_var()))),
                _ => None,
            };
            if let Some(ct) = constrained {
                self.unifier.bind(id, ct);
            }
        }
        let rt = self.unifier.resolve(ty);
        let inner = match (&rt, name) {
            (Type::Option(inner), "some") => self.bind_pattern_variant_option(inner, arg, span),
            (Type::Option(_), "none") => self.bind_pattern_variant_none(arg, span),
            (Type::Result(t, _), "ok") => match arg {
                Some(p) => Some((p.as_ref().clone(), (**t).clone())),
                None => {
                    self.errors.push(
                        error_at("`.ok` pattern requires an argument", span)
                        .with_note(
                            "`.ok` always carries a value — write `.ok(v)` to use it or `.ok(_)` to ignore it",
                        )
                        .with_note("run `zz fix` to rewrite this automatically")
                        .with_fixit(FixIt::safe(
                            span,
                            ".ok(_)",
                            "add wildcard payload",
                        )),
                    );
                    None
                }
            },
            (Type::Result(_, e), "err") => match arg {
                Some(p) => Some((p.as_ref().clone(), (**e).clone())),
                None => {
                    self.errors.push(
                        error_at("`.err` pattern requires an argument", span)
                        .with_note(
                            "`.err` always carries a value — write `.err(e)` to use it or `.err(_)` to ignore it",
                        )
                        .with_note("run `zz fix` to rewrite this automatically")
                        .with_fixit(FixIt::safe(
                            span,
                            ".err(_)",
                            "add wildcard payload",
                        )),
                    );
                    None
                }
            },
            (Type::Enum(ename, eargs), vname) => {
                self.bind_pattern_variant_enum(ename, vname, eargs, arg, span)
            }
            (Type::Var(_), _) => arg
                .as_ref()
                .map(|p| (p.as_ref().clone(), self.unifier.fresh_var())),
            (Type::Error, _) => {
                // Poisoned scrutinee: bind names as Error silently
                // so follow-on uses don't cascade (#246).
                if let Some(p) = arg {
                    self.bind_pattern(p, &Type::Error);
                }
                None
            }
            (other, vname) => {
                let mut diag = error_at(
                    format!("pattern `.{vname}` does not match a value of type `{other}`"),
                    span,
                );
                // Sherlock hints: point at the right variant family
                // for the scrutinee type (#247 follow-up).
                let hint: Option<String> = match (other, vname) {
                    (Type::Option(_), "ok") => {
                        Some("use `.some(x)` for the Option value".to_string())
                    }
                    (Type::Option(_), "err") => {
                        Some("Option has no `.err`; use `.some(x)` / `.none`".to_string())
                    }
                    (Type::Option(_), _) => {
                        Some("Option patterns are `.some(x)` / `.none`".to_string())
                    }
                    (Type::Result(_, _), "some") => {
                        Some("use `.ok(x)` for the Result value".to_string())
                    }
                    (Type::Result(_, _), "none") => Some(
                        "use `.err(e)` for the Result error; `.none` is an Option pattern"
                            .to_string(),
                    ),
                    (Type::Result(_, _), _) => {
                        Some("Result patterns are `.ok(x)` / `.err(e)`".to_string())
                    }
                    (Type::Bool, _) => Some("use `true` / `false` patterns for `bool`".to_string()),
                    _ => None,
                };
                if let Some(h) = hint {
                    diag = diag.with_note(h);
                }
                // Mismatch-first help when variant arms meet a plain
                // scalar scrutinee (user assumed Result/Option) (#247).
                if matches!(
                    other,
                    Type::Int | Type::Float | Type::Str | Type::Unit | Type::Bool
                ) {
                    diag = diag.with_note(format!(
                        "match on plain `{other}` needs no `.ok`/`.err` arms"
                    ));
                }
                self.errors.push(diag);
                // Bind the payload names as Error so their uses
                // don't cascade into `undefined variable` noise (#246).
                if let Some(p) = arg {
                    self.bind_pattern(p, &Type::Error);
                }
                None
            }
        };
        if let Some((p, inner)) = inner {
            self.bind_pattern(&p, &inner);
        }
    }

    /// Handle `.Variant` pattern on an enum scrutinee.
    fn bind_pattern_variant_enum(
        &mut self,
        ename: &str,
        vname: &str,
        eargs: &[Type],
        arg: &Option<Box<Pattern>>,
        span: Span,
    ) -> Option<(Pattern, Type)> {
        match self.enum_variant_payload(ename, vname, span) {
            Some(Some(pty)) => {
                // Substitute the scrutinee's arguments for
                // the enum's parameters (`Box<int>` + `T`
                // → `int`), exactly like generic struct
                // field access.
                let inner = self.subst_enum_payload(ename, eargs, &pty);
                match arg {
                    Some(p) => Some((p.as_ref().clone(), inner)),
                    None => {
                        self.errors.push(error_at(
                            format!(
                                "`.{vname}` pattern requires an argument (variant `{ename}.{vname}` holds a value)"
                            ),
                            span,
                        ));
                        None
                    }
                }
            }
            Some(None) => {
                if arg.is_some() {
                    self.errors.push(error_at(
                        format!(
                            "`.{vname}` pattern takes no argument (variant `{ename}.{vname}` holds no value)"
                        ),
                        span,
                    ));
                }
                None
            }
            // Unknown variant: already reported; bind
            // nothing to suppress cascades.
            None => None,
        }
    }

    /// Handle `.some(x)` pattern: binds payload name to Option's inner type.
    fn bind_pattern_variant_option(
        &mut self,
        inner: &Type,
        arg: &Option<Box<Pattern>>,
        span: Span,
    ) -> Option<(Pattern, Type)> {
        match arg {
            Some(p) => Some((p.as_ref().clone(), (*inner).clone())),
            None => {
                self.errors.push(
                    error_at("`.some` pattern requires an argument", span)
                        .with_note(
                            "`.some` always carries a value — write `.some(v)` to use it or `.some(_)` to ignore it",
                        )
                        .with_note("run `zz fix` to rewrite this automatically")
                        .with_fixit(FixIt::safe(
                            span,
                            ".some(_)",
                            "add wildcard payload",
                        )),
                );
                None
            }
        }
    }

    /// Handle `.none` pattern: rejects any argument.
    fn bind_pattern_variant_none(
        &mut self,
        arg: &Option<Box<Pattern>>,
        span: Span,
    ) -> Option<(Pattern, Type)> {
        if arg.is_some() {
            // The fixit only applies to a wildcard payload:
            // dropping `.none(x)` would orphan uses of `x`
            // in the arm body, so named payloads get the
            // error + notes and the user edits by hand.
            let mut diag = error_at("`.none` pattern takes no argument", span)
                .with_note("`.none` carries no value — write `.none`");
            if matches!(arg.as_deref(), Some(Pattern::Wildcard { .. })) {
                diag = diag
                    .with_note("run `zz fix` to rewrite this automatically")
                    .with_fixit(FixIt::safe(span, ".none", "drop payload"));
            }
            self.errors.push(diag);
        }
        None
    }

    fn check_exhaustive(&mut self, st: &Type, arms: &[zz_span_ast::ast::MatchArm], span: Span) {
        // Poisoned scrutinee: the root error is already reported; skip
        // exhaustiveness noise (#246).
        if matches!(self.unifier.resolve(st), Type::Error) {
            return;
        }
        // A wildcard or binding pattern matches every value, so either makes
        // the match exhaustive (a binding binds whatever the scrutinee holds).
        fn pat_is_wildcard(pat: &Pattern) -> bool {
            match pat {
                Pattern::Wildcard { .. } | Pattern::Binding { .. } => true,
                Pattern::Or { pats, .. } => pats.iter().any(pat_is_wildcard),
                _ => false,
            }
        }
        if arms.iter().any(|a| pat_is_wildcard(&a.pat)) {
            return;
        }
        // Enum exhaustiveness needs owned names (the signature table
        // can't lend `&str`s past the borrow), so enums take a separate
        // path from the static `&str` tables above.
        if let Type::Enum(ename, _) = st {
            let needs: Vec<String> = self
                .enums
                .get(ename)
                .map(|s| s.variants.iter().map(|(v, _)| v.clone()).collect())
                .unwrap_or_default();
            if needs.is_empty() {
                // Unknown enum (no registered signature): can't verify,
                // don't cascade.
                return;
            }
            let mut have: Vec<String> = Vec::new();
            for a in arms {
                pat_tags(&a.pat, &mut have);
            }
            let missing: Vec<&String> = needs.iter().filter(|n| !have.contains(n)).collect();
            if !missing.is_empty() {
                let missing_str = missing
                    .iter()
                    .map(|m| format!("`.{m}`"))
                    .collect::<Vec<_>>()
                    .join(" or ");
                let mut diag = error_at(
                    format!("non-exhaustive match: missing {missing_str} (or add a `_` arm)"),
                    span,
                )
                .with_code(ErrorCode(4));
                // Add a fixit for each missing arm, plus a wildcard arm.
                let mut alts: Vec<String> = Vec::new();
                for m in &missing {
                    alts.push(format!("`{m} => ()`"));
                }
                alts.push("`_ => ()`".to_string());
                if alts.len() == 1 {
                    diag = diag.with_fixit(FixIt::safe(
                        span,
                        format!("{} => () {{}}", missing[0]),
                        "add missing arm",
                    ));
                } else {
                    diag = diag.with_fixit(FixIt::ambiguous(
                        span,
                        format!("{} => () {{}}", missing[0]),
                        "add missing arm",
                        alts,
                    ));
                }
                diag = diag.with_note(format!(
                    "add arm(s) for {missing_str} or a `_` wildcard arm"
                ));
                self.errors.push(diag);
            }
            return;
        }
        let needs: Option<Vec<&str>> = match st {
            Type::Option(_) => Some(vec!["some", "none"]),
            Type::Result(_, _) => Some(vec!["ok", "err"]),
            Type::Bool => Some(vec!["true", "false"]),
            Type::Int | Type::Float | Type::Str | Type::Unit => {
                // Mismatch-first: variant arms on a plain scalar get their
                // mismatch errors from bind_pattern; skip the wildcard noise
                // here so the real error leads (#247).
                let has_variant = arms.iter().any(|a| {
                    fn has_variant_pat(pat: &Pattern) -> bool {
                        match pat {
                            Pattern::Variant { .. } => true,
                            Pattern::Or { pats, .. } => pats.iter().any(has_variant_pat),
                            _ => false,
                        }
                    }
                    has_variant_pat(&a.pat)
                });
                if !has_variant {
                    self.errors.push(error_at(
                        format!("match on `{st}` requires a `_` wildcard arm"),
                        span,
                    ));
                }
                return;
            }
            _ => return,
        };
        let Some(needs) = needs else { return };
        fn pat_tags(pat: &Pattern, out: &mut Vec<String>) {
            match pat {
                Pattern::Variant { name, .. } => out.push(name.clone()),
                Pattern::Literal {
                    value: Lit::Bool(b),
                    ..
                } => out.push(if *b { "true" } else { "false" }.to_string()),
                Pattern::Or { pats, .. } => {
                    for p in pats {
                        pat_tags(p, out);
                    }
                }
                _ => {}
            }
        }
        let mut have: Vec<String> = Vec::new();
        for a in arms {
            pat_tags(&a.pat, &mut have);
        }
        let missing: Vec<&str> = needs
            .iter()
            .filter(|n| !have.iter().any(|h| h == *n))
            .copied()
            .collect();
        if !missing.is_empty() {
            let missing = missing
                .iter()
                .map(|m| format!("`.{m}`"))
                .collect::<Vec<_>>()
                .join(" or ");
            self.errors.push(error_at(
                format!("non-exhaustive match: missing {missing} (or add a `_` arm)"),
                span,
            ));
        }
    }

    /// True if `pat` matches every value of its type: a wildcard, a binding,
    /// or an `or`-pattern containing one, or a tuple whose every element is
    /// itself a catch-all.
    fn pat_covers_all(&self, pat: &Pattern) -> bool {
        match pat {
            Pattern::Wildcard { .. } | Pattern::Binding { .. } => true,
            Pattern::Or { pats, .. } => pats.iter().any(|p| self.pat_covers_all(p)),
            Pattern::Tuple { pats, .. } => pats.iter().all(|p| self.pat_covers_all(p)),
            _ => false,
        }
    }

    /// Determine which top-level constructors of `ty` are NOT covered by
    /// `pats` (a list of unguarded patterns). Returns `None` when `ty` can't
    /// be analyzed conservatively (assume covered), or `Some(missing)` when
    /// constructors are provably absent. Nested constructors are checked
    /// recursively (Maranget-style specialization).
    fn missing_patterns(&self, pats: &[&Pattern], ty: &Type) -> Option<Vec<String>> {
        use zz_span_ast::ast::Lit;
        // Flatten `or`-patterns: `a | b` covers whatever `a` or `b` covers.
        let flat: Vec<&Pattern> = pats.iter().copied().flat_map(|p| self.or_alt(p)).collect();
        // Catch-all present → nothing missing.
        if flat.iter().any(|p| self.pat_covers_all(p)) {
            return Some(Vec::new());
        }
        match ty {
            Type::Enum(ename, _) => {
                return self.missing_patterns_enum(ename, &flat);
            }
            Type::Option(inner) => {
                let has_none = flat
                    .iter()
                    .any(|p| matches!(p, Pattern::Variant { name, .. } if name == "none"));
                let some_sub: Vec<&Pattern> = flat
                    .iter()
                    .filter(|p| matches!(p, Pattern::Variant { name, .. } if name == "some"))
                    .copied()
                    .collect();
                let mut missing = Vec::new();
                if !has_none {
                    missing.push(".none".to_string());
                }
                if !some_sub.is_empty() {
                    let inner_ty = &**inner;
                    let args: Vec<Pattern> =
                        some_sub.iter().map(|p| self.variant_arg(*p)).collect();
                    if let Some(m) =
                        self.missing_patterns(&args.iter().collect::<Vec<&Pattern>>(), inner_ty)
                    {
                        if !m.is_empty() {
                            missing.push(format!(".some ({})", m.join(", ")));
                        }
                    }
                } else if missing.is_empty() {
                    // `.none` present but no `.some` at all → missing .some.
                    missing.push(".some".to_string());
                }
                Some(missing)
            }
            Type::Result(ok, _err) => {
                let has_err = flat
                    .iter()
                    .any(|p| matches!(p, Pattern::Variant { name, .. } if name == "err"));
                let ok_sub: Vec<&Pattern> = flat
                    .iter()
                    .filter(|p| matches!(p, Pattern::Variant { name, .. } if name == "ok"))
                    .copied()
                    .collect();
                let mut missing = Vec::new();
                if !has_err {
                    missing.push(".err".to_string());
                }
                if !ok_sub.is_empty() {
                    let args: Vec<Pattern> = ok_sub.iter().map(|p| self.variant_arg(*p)).collect();
                    if let Some(m) =
                        self.missing_patterns(&args.iter().collect::<Vec<&Pattern>>(), ok)
                    {
                        if !m.is_empty() {
                            missing.push(format!(".ok ({})", m.join(", ")));
                        }
                    }
                } else if missing.is_empty() {
                    missing.push(".ok".to_string());
                }
                Some(missing)
            }
            Type::Bool => {
                let has_true = flat.iter().any(|p| {
                    matches!(
                        p,
                        Pattern::Literal {
                            value: Lit::Bool(b),
                            ..
                        } if *b
                    )
                });
                let has_false = flat.iter().any(|p| {
                    matches!(
                        p,
                        Pattern::Literal {
                            value: Lit::Bool(b),
                            ..
                        } if !*b
                    )
                });
                let mut missing = Vec::new();
                if !has_true {
                    missing.push("true".to_string());
                }
                if !has_false {
                    missing.push("false".to_string());
                }
                Some(missing)
            }
            Type::Tuple(_ts) => {
                // A tuple domain is covered only if a catch-all pattern is
                // present (handled by `pat_covers_all` above, which returns
                // early). Without one, literal tuple patterns cover only
                // finitely many points of an open domain, so the match is
                // non-exhaustive.
                Some(vec!["tuple pattern".to_string()])
            }
            Type::Int
            | Type::Float
            | Type::Str
            | Type::Unit
            | Type::Never
            | Type::Var(_)
            | Type::Named(_) => {
                let has_literal = flat.iter().any(|p| matches!(p, Pattern::Literal { .. }));
                if has_literal {
                    Some(vec!["`_` wildcard arm".to_string()])
                } else {
                    Some(Vec::new())
                }
            }
            _ => Some(Vec::new()), // opaque / function / etc.: assume covered
        }
    }

    /// Check missing patterns for an enum type: list variants with no
    /// matching arm, recursing into payload types.
    fn missing_patterns_enum(&self, ename: &str, flat: &[&Pattern]) -> Option<Vec<String>> {
        let variants: Vec<(String, Option<Type>)> = self
            .enums
            .get(ename)
            .map(|s| {
                s.variants
                    .iter()
                    .map(|(v, p)| (v.clone(), p.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if variants.is_empty() {
            // Unknown enum: can't verify, assume covered.
            return Some(Vec::new());
        }
        let mut missing = Vec::new();
        for (vname, payload) in &variants {
            let sub: Vec<&Pattern> = flat
                .iter()
                .filter(|p| matches!(p, Pattern::Variant { name, .. } if name == vname))
                .copied()
                .collect();
            if sub.is_empty() {
                missing.push(format!(".{vname}"));
                continue;
            }
            // Recurse: variant arms must cover the payload type.
            let payload_ty = payload.clone().unwrap_or(Type::Unit);
            let args: Vec<Pattern> = sub.iter().map(|p| self.variant_arg(*p)).collect();
            if let Some(m) =
                self.missing_patterns(&args.iter().collect::<Vec<&Pattern>>(), &payload_ty)
            {
                if !m.is_empty() {
                    missing.push(format!(".{vname} ({})", m.join(", ")));
                }
            }
        }
        Some(missing)
    }

    /// Flatten an `or`-pattern into its alternatives (non-or patterns pass
    /// through unchanged).
    fn or_alt<'a>(&self, pat: &'a Pattern) -> Vec<&'a Pattern> {
        match pat {
            Pattern::Or { pats, .. } => {
                let mut out = Vec::new();
                for p in pats {
                    out.extend(self.or_alt(p));
                }
                out
            }
            p => vec![p],
        }
    }

    /// Extract the argument pattern of a variant pattern `.V(arg)`, or a
    /// wildcard if the variant has no payload.
    fn variant_arg(&self, pat: &Pattern) -> Pattern {
        match pat {
            Pattern::Variant { arg: Some(a), .. } => (**a).clone(),
            _ => Pattern::Wildcard {
                span: Span::new(0, 0),
            },
        }
    }

    /// Check an enum construction `Enum.Variant(args)` where `enum_name`
    /// is the already-canonicalized enum and `variant` the trailing call
    /// path component. Returns the enum type on success.
    fn check_enum_construction(
        &mut self,
        enum_name: &str,
        variant: &str,
        args: &[zz_span_ast::ast::Expr],
        named: &[(zz_span_ast::ast::Ident, zz_span_ast::ast::Expr)],
        span: Span,
    ) -> Option<Type> {
        let sig = self.enums.get(enum_name).cloned()?;
        let Some((_, payload)) = sig.variants.iter().find(|(v, _)| v == variant) else {
            let known: Vec<&str> = sig.variants.iter().map(|(v, _)| v.as_str()).collect();
            self.errors.push(error_at(
                format!(
                    "unknown variant `{variant}` for enum `{enum_name}` (expected one of: {})",
                    known.join(", "),
                ),
                span,
            ));
            return Some(Type::Enum(
                enum_name.to_string(),
                sig.generics
                    .iter()
                    .map(|_| self.unifier.fresh_var())
                    .collect(),
            ));
        };
        // Generic parameters instantiate to fresh variables, filled in
        // by unifying the payload below (struct-literal rule).
        let (gen_vars, gen_map) = self.fresh_enum_vars(enum_name);
        match payload {
            Some(pty) => {
                if !named.is_empty() {
                    self.errors.push(error_at(
                        format!(
                            "variant `{enum_name}.{variant}` takes one positional argument, not named arguments"
                        ),
                        span,
                    ));
                }
                if args.len() != 1 {
                    self.errors.push(error_at(
                        format!(
                            "variant `{enum_name}.{variant}` takes 1 argument but {} given",
                            args.len(),
                        ),
                        span,
                    ));
                    // Still check the args so nested errors surface.
                    for arg in args {
                        self.check_expr(arg);
                    }
                } else {
                    let at = self.check_expr(&args[0]);
                    let exp = zz_infer::subst(pty, &gen_map);
                    if let Err(e) = self.unifier.unify(&at, &exp) {
                        self.report_mismatch(e, args[0].span());
                    }
                }
            }
            None => {
                if !args.is_empty() || !named.is_empty() {
                    self.errors.push(error_at(
                        format!(
                            "variant `{enum_name}.{variant}` takes no arguments but {} given",
                            args.len() + named.len(),
                        ),
                        span,
                    ));
                }
            }
        }
        // A value construction counts as using the enum's name (marks
        // imports used when the path is qualified).
        if enum_name.contains('.') {
            self.used_names.insert(enum_name.to_string());
        }
        // Generic parameters that are still unbound inference variables
        // (e.g. `Box.E` with no payload, or `Box.V(x)` with `x` untyped)
        // are left as-is here and defaulted to `unit` later at span-types
        // finalization via `default_variant_vars`. This is conservative:
        // it mirrors `.none` → `Option<unit>`, and any use that actually
        // constrains the generic fails loudly at check time rather than
        // silently weakening the type. Do NOT error here — single-pass
        // checking cannot see later uses that would pin the generic (e.g.
        // `Box.V(x)` followed by `b.get_or(0)`).
        Some(Type::Enum(
            enum_name.to_string(),
            gen_vars
                .iter()
                .map(|v| self.unifier.resolve_deep(v))
                .collect(),
        ))
    }

    fn check_func_body(&mut self, stmt: &Stmt, sig: &FuncSig) {
        let (name, params, body) = match stmt {
            Stmt::Func {
                name, params, body, ..
            } => (name, params, body),
            _ => {
                self.errors.push(error_at(
                    "internal: check_func_body called with a non-Func statement",
                    stmt.span(),
                ));
                return;
            }
        };
        self.push_scope();
        for (i, (pname, pty)) in sig.params.iter().enumerate() {
            // Bind with the source span when available (powers precise
            // diagnostics, e.g. the module-namespace shadow lint); the
            // signature table itself carries no spans.
            match params.get(i) {
                Some(p) => {
                    // The `self` receiver of an impl method may legitimately
                    // go unread (e.g. a constant method); don't flag it as an
                    // unused variable. Named-function parameters keep the
                    // warning.
                    let warn_unused = p.name.name != "self";
                    self.define_binding_at(&p.name.name, pty.clone(), p.name.span, warn_unused);
                }
                None => self.define(pname, pty.clone()),
            }
        }
        let prev_ret = self.current_ret.replace(sig.ret.clone());
        let prev_gen = std::mem::replace(&mut self.current_generics, sig.generics.clone());
        let prev_bounds = std::mem::replace(
            &mut self.current_bounds,
            sig.bounds.iter().cloned().collect(),
        );
        let body_t = self.check_named_fn_body(body);
        self.current_ret = prev_ret;
        self.current_generics = prev_gen;
        self.current_bounds = prev_bounds;
        self.pop_scope();
        // Dead-mutation lint (#278): value-typed params mutated without
        // ever being observed. Runs on the AST after the body checks so
        // it sees the final program even when type errors exist.
        zz_lints::value_lint::lint_dead_param_mutation(&sig.params, body, &mut self.errors);
        let _ = name;
        // A named function yields a value only through `return`, so its
        // fall-through type is `unit` unless every path diverges (`Never`,
        // e.g. ending in `if c { return 1 } else { return 2 }`). The body's
        // tail expression type is irrelevant here: it was discarded.
        //
        // A non-unit signature plus a fall-through is the missing-`return`
        // error — with an insertion fixit so `zz fix` can migrate the
        // corpus. An unresolved return (no annotation) is not "non-unit":
        // unifying it with `unit` is how the signature becomes `unit`.
        let resolved_body = self.unifier.resolve(&body_t);
        if matches!(resolved_body, Type::Error) {
            // The body already reported a type error: don't cascade a
            // return-type complaint on top of it.
            let _ = self.unifier.unify(&body_t, &sig.ret);
            return;
        }
        let resolved_ret = self.unifier.resolve(&sig.ret);
        // Implicit return type (no `->` annotation): warn when the body's
        // tail expression has a non-unit type, since it is silently
        // discarded and the function effectively returns `unit`.
        if matches!(resolved_ret, Type::Var(_))
            && !matches!(
                resolved_body,
                Type::Unit | Type::Void | Type::Never | Type::Error
            )
        {
            let last = body.stmts.last();
            let span = last.map(|s| s.span()).unwrap_or(body.span);
            let body_ty = self.unifier.resolve(&body_t);
            let diag = warning_at(
                format!(
                    "function has no `->` return type but its body yields `{body_ty}`; the last expression is discarded",
                ),
                span,
            )
            .with_note(
                "add an explicit `->` return type, or end the body with `return` / `()` to silence this warning",
            );
            self.errors.push(diag);
        }
        let ret_takes_value = !matches!(
            resolved_ret,
            Type::Unit | Type::Void | Type::Error | Type::Var(_)
        );
        let diverges = matches!(resolved_body, Type::Never);
        if ret_takes_value && !diverges {
            self.report_missing_return(body, &resolved_ret);
            return;
        }
        let fallthrough = if diverges { Type::Never } else { Type::Unit };
        if let Err(e) = self.unifier.unify(&fallthrough, &sig.ret) {
            self.report_mismatch(e, body.span);
        }
        definite_assignment::analyze_function_body(self, body);
    }

    /// Report a missing-return diagnostic with improved diagnostics:
    /// - unit-tail functions (println, match, etc.) get the message only, no fixit.
    /// - annotated non-unit functions that can fall through get error + fixit.
    /// - fixit is withheld for `if` without `else`, match arms, and `let`
    ///   bindings whose name is used later (intentional shadowings).
    fn report_missing_return(&mut self, body: &Block, ret_ty: &Type) {
        let last = body.stmts.last();
        let span = last.map(|s| s.span()).unwrap_or(body.span);
        // --- improved unit-tail detection using self.last_block_tail ---
        let unit_tail = match &self.last_block_tail {
            Some((_, ty)) => matches!(self.unifier.resolve(ty), Type::Unit | Type::Void),
            None => false,
        };
        // --- value-tail fixit position ---
        // withhold fixit for: if without else, match arms, let bindings
        // whose name is used later (intentional shadowings).
        let withhold_fixit = unit_tail
            || match last {
                Some(Stmt::Expr(Expr::If { els: None, .. })) => true,
                Some(Stmt::Expr(Expr::Match { .. })) => true,
                Some(Stmt::Decl { .. }) => {
                    let outer_names = body
                        .stmts
                        .iter()
                        .take(body.stmts.len() - 1)
                        .flat_map(Self::extract_names)
                        .collect::<std::collections::HashSet<_>>();
                    let let_name = Self::extract_name(last.unwrap());
                    outer_names.contains(&let_name.unwrap_or_default())
                }
                _ => false,
            };
        let value_tail = if withhold_fixit {
            None
        } else {
            let s = last.map(|stmt| stmt.span()).unwrap_or(body.span).start;
            Some(Span::new(s, s))
        };
        let mut diag = error_at(
            format!("function returns `{ret_ty}` but its body can fall through; add `return `"),
            span,
        )
        .with_note("named functions return a value only through `return`; the last expression is discarded")
        .with_code(ErrorCode(6));
        if let Some(insert_at) = value_tail {
            diag = diag.with_fixit(FixIt::safe(insert_at, "return ", "add `return`"));
        }
        self.errors.push(diag);
    }

    /// Extract an identifier name from a statement or expression for shadow-checking.
    fn extract_name(stmt: &Stmt) -> Option<String> {
        match stmt {
            Stmt::Decl { name, .. } => Some(name.name.clone()),
            Stmt::Expr(Expr::If { then, .. }) => Self::extract_name_from_block(then),
            Stmt::Expr(Expr::Match { .. }) => None,
            Stmt::Return {
                value: Some(Expr::Ident { name, .. }),
                ..
            } => Some(name.clone()),
            _ => None,
        }
    }

    fn extract_name_from_block(block: &Block) -> Option<String> {
        block.stmts.last().and_then(Self::extract_name)
    }

    /// Extract all identifier names from a statement (for shadow checking).
    fn extract_names(stmt: &Stmt) -> Vec<String> {
        match stmt {
            Stmt::Decl { name, .. } => vec![name.name.clone()],
            Stmt::Expr(Expr::If { then, .. }) => Self::extract_names_from_block(then),
            Stmt::For { body, .. } => Self::extract_names_from_block(body),
            Stmt::Return {
                value: Some(Expr::Ident { name, .. }),
                ..
            } => vec![name.clone()],
            _ => vec![],
        }
    }

    fn extract_names_from_block(block: &Block) -> Vec<String> {
        block.stmts.iter().flat_map(Self::extract_names).collect()
    }

    fn block_has_return(block: &Block) -> bool {
        block.stmts.iter().any(Self::stmt_has_return)
    }

    fn stmt_has_return(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Return { .. } => true,
            Stmt::For { body, .. } => Self::block_has_return(body),
            Stmt::Expr(Expr::While { body, .. }) => Self::block_has_return(body),
            Stmt::Expr(Expr::If { then, els, .. }) => {
                Self::block_has_return(then)
                    || els.as_ref().is_some_and(|e| Self::expr_has_return(e))
            }
            _ => false,
        }
    }

    fn expr_has_return(expr: &Expr) -> bool {
        match expr {
            Expr::Block(b) => Self::block_has_return(b),
            Expr::If { then, els, .. } => {
                Self::block_has_return(then)
                    || els.as_ref().is_some_and(|e| Self::expr_has_return(e))
            }
            _ => false,
        }
    }
}
