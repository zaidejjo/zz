//! Tier inventory for every stdlib entry.
//!
//! This is the committed classification the plan calls for: each qualified
//! name gets exactly one implementation tier. [`PURE_ZZ`], [`MATH_CONSTS`]
//! and [`VM_ONLY`] are the only hand-maintained tier lists; everything else
//! is derived from the relocated embedded-C / staticlib tables, so a name
//! cannot silently be both "native" and "pure ZZ".
//!
//! `regen` renders this data as `src/inventory.md` (checked in, drift-gated).

use crate::{ffi_impl, native_impl};

/// Where a stdlib entry is implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Implemented once in an embedded `.zz` source (VM + AOT ZZ lowering).
    PureZz,
    /// Embedded C runtime (`zz_codegen_runtime`), called through `native_impl`.
    CRuntime,
    /// Rust static library (`zz_native_rt`), called through `ffi_impl`.
    FfiStaticlib,
    /// Deliberately VM-only: AOT inlines or rejects it, no symbol exists.
    VmOnly,
    /// `std.math.*` numeric constant lowered as a float literal.
    Const,
    /// No tier claim: a gap the lockstep test reports.
    Unknown,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::PureZz => "PureZz",
            Tier::CRuntime => "CRuntime",
            Tier::FfiStaticlib => "FfiStaticlib",
            Tier::VmOnly => "VmOnly",
            Tier::Const => "Const",
            Tier::Unknown => "Unknown",
        }
    }
}

/// Functions implemented in embedded `.zz` sources (bare and `std.*` spellings).
pub const PURE_ZZ: &[&str] = &[
    // str helpers
    "std.str.repeat",
    "std.str.count",
    "std.str.is_empty",
    "std.str.reverse",
    "std.str.pad_left",
    "std.str.pad_right",
    "str.repeat",
    "str.count",
    "str.is_empty",
    "str.reverse",
    "str.pad_left",
    "str.pad_right",
    // math helpers
    "std.math.sum",
    "std.math.product",
    "std.math.count",
    "std.math.min",
    "std.math.max",
    "std.math.is_even",
    "std.math.is_odd",
    "std.math.min_arr",
    "std.math.max_arr",
    "std.math.sum_f",
    "std.math.product_f",
    "std.math.mean_f",
    "std.math.median_f",
    // PNSA: precompiled native stdlib (compiled from ZZ source to
    // libzz_stdlib.a). VM calls via FFI wrappers; AOT links the archive.
    "std.math.factorial",
    "std.math.gcd",
    "std.math.lcm",
    "math.sum",
    "math.product",
    "math.count",
    "math.min",
    "math.max",
    "math.is_even",
    "math.is_odd",
    "math.min_arr",
    "math.max_arr",
    "math.sum_f",
    "math.product_f",
    "math.mean_f",
    "math.median_f",
    "math.factorial",
    "math.gcd",
    "math.lcm",
    // vec helpers
    "std.vec.fold",
    "std.vec.sum",
    "std.vec.product",
    "std.vec.min_val",
    "std.vec.max_val",
    "std.vec.sum_f",
    "std.vec.product_f",
    "std.vec.concat",
    "std.vec.flatten",
    "std.vec.index_of",
    "std.vec.last_index_of",
    "vec.fold",
    "vec.sum",
    "vec.product",
    "vec.min_val",
    "vec.max_val",
    "vec.sum_f",
    "vec.product_f",
    "vec.concat",
    "vec.flatten",
    "vec.index_of",
    "vec.last_index_of",
    // json helpers (pure-ZZ, compiled from zz/json/mod.zz)
    "std.json.validate",
    "std.json.parse_or",
    "std.json.parse_or_null",
    "std.json.path_exists",
    "std.json.path_get_or",
    "std.json.is_null",
    "std.json.is_bool",
    "std.json.is_number",
    "std.json.is_string",
    "std.json.is_array",
    "std.json.is_object",
    "std.json.is_empty",
    "json.validate",
    "json.parse_or",
    "json.parse_or_null",
    "json.path_exists",
    "json.path_get_or",
    "json.is_null",
    "json.is_bool",
    "json.is_number",
    "json.is_string",
    "json.is_array",
    "json.is_object",
    "json.is_empty",
    // regexp helpers (pure-ZZ, compiled from zz/regexp/mod.zz)
    "Regexp.new",
    "std.regexp.is_email",
    "regexp.is_email",
    // Duration helpers (pure-ZZ, compiled from zz/time/mod.zz)
    "time.micros",
    "time.millis",
    "time.secs",
    "time.to_micros",
    "time.to_millis",
    "time.to_secs",
    "time.to_nanos",
    "time.sleep",
    // Calendar dates (pure-ZZ, zz/time date section, dict-based)
    "std.time.make_date",
    "time.make_date",
    "std.time.epoch_fallback",
    "time.epoch_fallback",
    "std.time.is_leap",
    "time.is_leap",
    "std.time.days_in_month",
    "time.days_in_month",
    "std.time.date_valid",
    "time.date_valid",
    "std.time.days_from_civil",
    "time.days_from_civil",
    "std.time.civil_from_days",
    "time.civil_from_days",
    "std.time.parse_rfc3339",
    "time.parse_rfc3339",
    "std.time.pad2",
    "time.pad2",
    "std.time.format_rfc3339",
    "time.format_rfc3339",
    "std.time.to_epoch_days",
    "time.to_epoch_days",
    "std.time.from_epoch_days",
    "time.from_epoch_days",
    "std.time.add_days",
    "time.add_days",
    "std.time.diff_days",
    "time.diff_days",
    // map helpers (pure-ZZ, zz/collections/map.zz)
    "std.map.has",
    "map.has",
    "std.map.get_or",
    "map.get_or",
    "std.map.get_str",
    "map.get_str",
    "std.map.keys",
    "map.keys",
    "std.map.keys_str",
    "map.keys_str",
    "std.map.values",
    "map.values",
    "std.map.values_str",
    "map.values_str",
    "std.map.len",
    "map.len",
    "std.map.is_empty",
    "map.is_empty",
    "std.map.merge",
    "map.merge",
    "std.map.merge_str",
    "map.merge_str",
    "std.map.remove",
    "map.remove",
    // set helpers (pure-ZZ, zz/collections/set.zz)
    "std.set.has",
    "set.has",
    "std.set.has_int",
    "set.has_int",
    "std.set.insert",
    "set.insert",
    "std.set.insert_int",
    "set.insert_int",
    "std.set.remove",
    "set.remove",
    "std.set.remove_int",
    "set.remove_int",
    "std.set.union",
    "set.union",
    "std.set.union_int",
    "set.union_int",
    "std.set.intersect",
    "set.intersect",
    "std.set.intersect_int",
    "set.intersect_int",
    "std.set.diff",
    "set.diff",
    "std.set.len",
    "set.len",
    "std.set.is_empty",
    "set.is_empty",
    // dec helpers (pure-ZZ, zz/dec/mod.zz)
    "std.dec.is_valid",
    "dec.is_valid",
    "std.dec.scale_of",
    "dec.scale_of",
    "std.dec.pow10",
    "dec.pow10",
    "std.dec.to_scaled",
    "dec.to_scaled",
    "std.dec.from_scaled",
    "dec.from_scaled",
    "std.dec.trim_zeros",
    "dec.trim_zeros",
    "std.dec.add",
    "dec.add",
    "std.dec.sub",
    "dec.sub",
    "std.dec.mul",
    "dec.mul",
    "std.dec.cmp",
    "dec.cmp",
    "std.dec.eq",
    "dec.eq",
    "std.dec.lt",
    "dec.lt",
    "std.dec.gt",
    "dec.gt",
    "std.dec.format",
    "dec.format",
    // builders (pure-ZZ, zz/bytes/mod.zz)
    "str.builder",
    "std.str.builder",
    "str.push_part",
    "std.str.push_part",
    "str.finish",
    "std.str.finish",
    "str.builder_len",
    "std.str.builder_len",
    "str.join_parts",
    "std.str.join_parts",
    "bytes.builder",
    "std.bytes.builder",
    "bytes.push_byte",
    "std.bytes.push_byte",
    "bytes.extend",
    "std.bytes.extend",
    "bytes.len_of",
    "std.bytes.len_of",
    "bytes.from_ints",
    "std.bytes.from_ints",
    // csv helpers (pure-ZZ, zz/csv/mod.zz)
    "std.csv.delim_first",
    "csv.delim_first",
    "std.csv.json_escape",
    "csv.json_escape",
    "std.csv.parse",
    "csv.parse",
    "std.csv.parse_delim",
    "csv.parse_delim",
    "std.csv.needs_quote",
    "csv.needs_quote",
    "std.csv.escape_cell",
    "csv.escape_cell",
    "std.csv.stringify",
    "csv.stringify",
    "std.csv.stringify_delim",
    "csv.stringify_delim",
    "std.csv.header",
    "csv.header",
    "std.csv.records",
    "csv.records",
    "std.csv.len",
    "csv.len",
    "std.csv.get_cell",
    "csv.get_cell",
    "std.csv.validate",
    "csv.validate",
    "std.csv.to_json",
    "csv.to_json",
    // property testing (pure-ZZ, compiled from zz/property/mod.zz)
    "std.property.seed",
    "property.seed",
    "std.property.norm_state",
    "property.norm_state",
    "std.property.step",
    "property.step",
    "std.property.stream",
    "property.stream",
    "std.property.int_in",
    "property.int_in",
    "std.property.pick",
    "property.pick",
    "std.property.boolean",
    "property.boolean",
    "std.property.alpha_char",
    "property.alpha_char",
    "std.property.alpha_str",
    "property.alpha_str",
    "std.property.shrink_int",
    "property.shrink_int",
    "std.property.shrink_str",
    "property.shrink_str",
    "std.property.shrink_value",
    "property.shrink_value",
    "std.property.check",
    "property.check",
    "std.property.for_all_int",
    "property.for_all_int",
    // ArgsParser constructor (pure-ZZ, compiled from zz/args/mod.zz)
    "ArgsParser.new",
    // path helpers (pure-ZZ, compiled from zz/path/mod.zz)
    "std.path.join",
    "std.path.join_all",
    "std.path.normalize",
    "std.path.basename",
    "std.path.dirname",
    "std.path.is_absolute",
    "std.path.extension",
    "path.join",
    "path.join_all",
    "path.normalize",
    "path.basename",
    "path.dirname",
    "path.is_absolute",
    "path.extension",
    // http helpers (pure-ZZ, compiled from zz/http/mod.zz)
    "std.http.ok",
    "std.http.created",
    "std.http.not_found",
    "std.http.redirect",
    "http.ok",
    "http.created",
    "http.not_found",
    "http.redirect",
    "std.http.cors",
    "http.cors",
    "std.http.secure_headers",
    "http.secure_headers",
    "std.http.secure_header_dict",
    "http.secure_header_dict",
    "std.http.csrf_token",
    "http.csrf_token",
    "std.http.csrf_check",
    "http.csrf_check",
    "std.http.request_id",
    "http.request_id",
    // colors helpers (pure-ZZ, compiled from zz/colors/mod.zz)
    "std.colors.black",
    "std.colors.red",
    "std.colors.green",
    "std.colors.yellow",
    "std.colors.blue",
    "std.colors.magenta",
    "std.colors.cyan",
    "std.colors.white",
    "std.colors.bright_black",
    "std.colors.bright_red",
    "std.colors.bright_green",
    "std.colors.bright_yellow",
    "std.colors.bright_blue",
    "std.colors.bright_magenta",
    "std.colors.bright_cyan",
    "std.colors.bright_white",
    "std.colors.bg_black",
    "std.colors.bg_red",
    "std.colors.bg_green",
    "std.colors.bg_yellow",
    "std.colors.bg_blue",
    "std.colors.bg_magenta",
    "std.colors.bg_cyan",
    "std.colors.bg_white",
    "std.colors.bg_bright_black",
    "std.colors.bg_bright_red",
    "std.colors.bg_bright_green",
    "std.colors.bg_bright_yellow",
    "std.colors.bg_bright_blue",
    "std.colors.bg_bright_magenta",
    "std.colors.bg_bright_cyan",
    "std.colors.bg_bright_white",
    "std.colors.bold",
    "std.colors.dim",
    "std.colors.italic",
    "std.colors.underline",
    "std.colors.blink",
    "std.colors.reverse",
    "std.colors.strikethrough",
    "std.colors.reset",
    "std.colors.strip",
    "std.colors.clamp255",
    "std.colors.color256",
    "std.colors.bg_256",
    "std.colors.rgb",
    "std.colors.bg_rgb",
    "std.colors.hex_val",
    "std.colors.hex_byte",
    "std.colors.hex",
    "std.colors.hex6",
    "std.colors.hex3",
    "colors.black",
    "colors.red",
    "colors.green",
    "colors.yellow",
    "colors.blue",
    "colors.magenta",
    "colors.cyan",
    "colors.white",
    "colors.bright_black",
    "colors.bright_red",
    "colors.bright_green",
    "colors.bright_yellow",
    "colors.bright_blue",
    "colors.bright_magenta",
    "colors.bright_cyan",
    "colors.bright_white",
    "colors.bg_black",
    "colors.bg_red",
    "colors.bg_green",
    "colors.bg_yellow",
    "colors.bg_blue",
    "colors.bg_magenta",
    "colors.bg_cyan",
    "colors.bg_white",
    "colors.bg_bright_black",
    "colors.bg_bright_red",
    "colors.bg_bright_green",
    "colors.bg_bright_yellow",
    "colors.bg_bright_blue",
    "colors.bg_bright_magenta",
    "colors.bg_bright_cyan",
    "colors.bg_bright_white",
    "colors.bold",
    "colors.dim",
    "colors.italic",
    "colors.underline",
    "colors.blink",
    "colors.reverse",
    "colors.strikethrough",
    "colors.reset",
    "colors.strip",
    "colors.clamp255",
    "colors.color256",
    "colors.bg_256",
    "colors.rgb",
    "colors.bg_rgb",
    "colors.hex_val",
    "colors.hex_byte",
    "colors.hex",
    "colors.hex6",
    "colors.hex3",
];

/// `std.math.*` numeric constants: real `Float` values, not zero-arg functions.
pub const MATH_CONSTS: &[&str] = &[
    "std.math.PI",
    "std.math.E",
    "std.math.TAU",
    "std.math.SQRT_2",
    "std.math.SQRT_1_2",
    "std.math.LN_2",
    "std.math.LN_10",
    "std.math.LOG10_E",
    "std.math.LOG2_E",
    "std.math.INF",
    "std.math.NAN",
];

/// Deliberate VM-only AOT gaps: inlined by codegen, so no C/FFI symbol exists.
pub const VM_ONLY: &[&str] = &["std.sqlz.transaction", "sqlz.transaction"];

pub fn is_pure_zz(name: &str) -> bool {
    PURE_ZZ.contains(&name)
}

pub fn is_const(name: &str) -> bool {
    MATH_CONSTS.contains(&name)
}

pub fn is_vm_only(name: &str) -> bool {
    VM_ONLY.contains(&name)
}

/// Classify one qualified name. `None` means the inventory missed it; the
/// `inventory_covers_every_signature` test fails with the list.
pub fn tier_of(name: &str) -> Option<Tier> {
    if ffi_impl(name).is_some() {
        return Some(Tier::FfiStaticlib);
    }
    if native_impl(name).is_some() {
        return Some(Tier::CRuntime);
    }
    if is_const(name) {
        return Some(Tier::Const);
    }
    if is_pure_zz(name) {
        return Some(Tier::PureZz);
    }
    if is_vm_only(name) {
        return Some(Tier::VmOnly);
    }
    None
}

/// Human-readable implementation location for the inventory report.
pub fn impl_loc(name: &str) -> &'static str {
    if let Some(sym) = ffi_impl(name) {
        return sym;
    }
    if let Some(sym) = native_impl(name) {
        return sym;
    }
    if is_const(name) {
        return "stdlib_consts";
    }
    if is_pure_zz(name) {
        return pure_zz_module(name);
    }
    "AOT-inlined"
}

fn pure_zz_module(name: &str) -> &'static str {
    let short = name.strip_prefix("std.").unwrap_or(name);
    let head = short
        .split('.')
        .next()
        .unwrap_or(short)
        .to_ascii_lowercase();
    match head.as_str() {
        "str" => "zz/str/mod.zz",
        "math" => "zz/math/mod.zz",
        "vec" => "zz/collections/vec.zz",
        "json" => "zz/json/mod.zz",
        "regexp" => "zz/regexp/mod.zz",
        "time" => "zz/time/mod.zz",
        "args" => "zz/args/mod.zz",
        "colors" => "zz/colors/mod.zz",
        "path" => "zz/path/mod.zz",
        "http" => "zz/http/mod.zz",
        "map" => "zz/collections/map.zz",
        "set" => "zz/collections/set.zz",
        "dec" => "zz/dec/mod.zz",
        "bytes" => "zz/bytes/mod.zz",
        "csv" => "zz/csv/mod.zz",
        "property" => "zz/property/mod.zz",
        _ => "embedded .zz",
    }
}

/// One classified signature key.
#[derive(Clone, Debug)]
pub struct InventoryRow {
    pub name: String,
    pub tier: Tier,
    pub impl_loc: &'static str,
}

/// Every signature key in the table, sorted, with its tier.
pub fn rows() -> Vec<InventoryRow> {
    let mut names: Vec<String> = crate::build().into_keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| InventoryRow {
            tier: tier_of(&name).unwrap_or(Tier::Unknown),
            impl_loc: impl_loc(&name),
            name,
        })
        .collect()
}

/// Markdown report written by `regen` to `src/inventory.md`.
pub fn markdown() -> String {
    use std::collections::BTreeMap;
    let rows = rows();
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for row in &rows {
        *counts.entry(row.tier.as_str()).or_default() += 1;
    }
    let mut out = String::from(
        "# Stdlib inventory (generated)\n\n\
         Generated by `cargo run -p zz_stdlib_meta --bin regen`; do not edit.\n\
         One row per checker signature, with its implementation tier.\n\n",
    );
    out.push_str(&format!("Total entries: {}\n\n", rows.len()));
    out.push_str("| tier | entries |\n|---|---:|\n");
    for (tier, count) in counts {
        out.push_str(&format!("| {tier} | {count} |\n"));
    }
    out.push_str("\n| tier | name | implementation |\n|---|---|---|\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | `{}` | `{}` |\n",
            row.tier.as_str(),
            row.name,
            row.impl_loc
        ));
    }
    out
}
