//! IR type language: a serializable mirror of the checker's [`Type`].
//!
//! v1 lowers every signature slot to [`IrType::Unknown`]; the table,
//! codec, and verifier support the full language so the AOT slice can
//! populate real signatures without a format change.
//!
//! [`Type`]: zz_checker::Type

use crate::StrId;
use crate::TypeId;

/// Serializable type. Struct/enum/union shapes keep names (and generic
/// arguments, erased at runtime) so backends can resolve layouts.
#[derive(Debug, Clone, PartialEq)]
pub enum IrType {
    /// No information yet (v1 signature default).
    Unknown,
    Unit,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    Tuple(Vec<super::TypeId>),
    Option(super::TypeId),
    Result(super::TypeId, super::TypeId),
    Array(super::TypeId),
    Dict(super::TypeId, super::TypeId),
    Func(Vec<super::TypeId>, super::TypeId),
    Range(super::TypeId),
    Json,
    Db,
    HttpServer,
    TcpStream,
    TcpListener,
    Response,
    HttpRequest,
    Opaque(StrId),
    Chan,
    TaskJoin,
    Struct(StrId, Vec<super::TypeId>),
    Enum(StrId, Vec<super::TypeId>),
    Union(Vec<super::TypeId>),
    Named(StrId),
    Var(u32),
    Error,
    /// C `void` (extern signatures only).
    Void,
    Ptr {
        mutable: bool,
        inner: super::TypeId,
    },
}

impl IrType {
    /// Stable tag byte for the codec. Append-only: never reuse a tag.
    pub fn tag(&self) -> u8 {
        match self {
            IrType::Unknown => 0,
            IrType::Unit => 1,
            IrType::Bool => 2,
            IrType::Int => 3,
            IrType::Float => 4,
            IrType::Str => 5,
            IrType::Bytes => 6,
            IrType::Tuple(_) => 7,
            IrType::Option(_) => 8,
            IrType::Result(_, _) => 9,
            IrType::Array(_) => 10,
            IrType::Dict(_, _) => 11,
            IrType::Func(_, _) => 12,
            IrType::Range(_) => 13,
            IrType::Json => 14,
            IrType::Db => 15,
            IrType::HttpServer => 16,
            IrType::TcpStream => 17,
            IrType::TcpListener => 18,
            IrType::Response => 19,
            IrType::HttpRequest => 20,
            IrType::Opaque(_) => 21,
            IrType::Chan => 22,
            IrType::TaskJoin => 23,
            IrType::Struct(_, _) => 24,
            IrType::Enum(_, _) => 25,
            IrType::Union(_) => 26,
            IrType::Named(_) => 27,
            IrType::Var(_) => 28,
            IrType::Error => 29,
            IrType::Void => 30,
            IrType::Ptr { .. } => 31,
        }
    }

    /// Collect nested type-table ids (for the verifier's ordering check).
    pub fn collect_ids(&self, out: &mut Vec<TypeId>) {
        match self {
            IrType::Tuple(items) | IrType::Union(items) | IrType::Func(items, _) => {
                out.extend(items.iter().copied());
                if let IrType::Func(_, ret) = self {
                    out.push(*ret);
                }
            }
            IrType::Option(i) | IrType::Array(i) | IrType::Range(i) => out.push(*i),
            IrType::Result(a, b) | IrType::Dict(a, b) => {
                out.push(*a);
                out.push(*b);
            }
            IrType::Struct(_, args) | IrType::Enum(_, args) => out.extend(args.iter().copied()),
            IrType::Ptr { inner, .. } => out.push(*inner),
            _ => {}
        }
    }

    /// Short stable name for `zz dis`.
    pub fn name(&self) -> &'static str {
        match self {
            IrType::Unknown => "unknown",
            IrType::Unit => "unit",
            IrType::Bool => "bool",
            IrType::Int => "int",
            IrType::Float => "float",
            IrType::Str => "str",
            IrType::Bytes => "bytes",
            IrType::Tuple(_) => "tuple",
            IrType::Option(_) => "option",
            IrType::Result(_, _) => "result",
            IrType::Array(_) => "array",
            IrType::Dict(_, _) => "dict",
            IrType::Func(_, _) => "func",
            IrType::Range(_) => "range",
            IrType::Json => "json",
            IrType::Db => "db",
            IrType::HttpServer => "httpserver",
            IrType::TcpStream => "tcpstream",
            IrType::TcpListener => "tcplistener",
            IrType::Response => "response",
            IrType::HttpRequest => "httprequest",
            IrType::Opaque(_) => "opaque",
            IrType::Chan => "chan",
            IrType::TaskJoin => "taskjoin",
            IrType::Struct(_, _) => "struct",
            IrType::Enum(_, _) => "enum",
            IrType::Union(_) => "union",
            IrType::Named(_) => "named",
            IrType::Var(_) => "var",
            IrType::Error => "error",
            IrType::Void => "void",
            IrType::Ptr { .. } => "ptr",
        }
    }
}
