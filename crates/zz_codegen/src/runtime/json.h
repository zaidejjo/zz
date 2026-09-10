// ZZ native runtime — native JSON parsing, stringifying, and utilities.
//
// Parses recursive JSON into plain zz_values (unit=null, bool, int/float,
// str, array, dict) matching the VM's grammar and error messages, and
// serializes zz_values back to compact JSON text.

#ifndef ZZ_RUNTIME_JSON_H
#define ZZ_RUNTIME_JSON_H

#include "core.h"
#include "strings.h"

#ifdef __cplusplus
extern "C" {
#endif

// Wrap a raw value as a JSON value (mirrors the VM's `Value::Json`).
// The payload is heap-allocated and refcounted via the same path as
// Option/Result variants.
zz_value zz_json_wrap(zz_value inner);

// Unwrap a ZZ_JSON payload (or pass through plain values).
zz_value zz_json_unwrap(zz_value v);

// Compact JSON text of a value (malloc'd). Unwraps ZZ_JSON payloads.
char *json_to_cstr(zz_value v);

// Compact JSON serializer (matches VM to_json_string).
void json_serialize(SB *sb, zz_value v);

// ---- json natives ------------------------------------------------------
zz_value zz_json_parse(zz_value s, int *err);
zz_value zz_json_stringify(zz_value v, int *err);
zz_value zz_json_null(zz_value unused, int *err);
zz_value zz_json_get(zz_value j, zz_value key, int *err);
zz_value zz_json_unwrap_plain(zz_value j);
zz_value zz_json_as_str(zz_value j, int *err);
zz_value zz_json_as_int(zz_value j, int *err);
zz_value zz_json_as_float(zz_value j, int *err);
zz_value zz_json_as_bool(zz_value j, int *err);

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_JSON_H