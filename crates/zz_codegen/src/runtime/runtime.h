// ZZ native runtime — value model and operations.
//
// Umbrella header: includes the modular sub-headers in dependency order.
// The AOT backend concatenates this header (plus the sub-headers) and the
// runtime .c files into a single translation unit, so the `#include`
// directives below are stripped at assembly time.

#ifndef ZZ_RUNTIME_H
#define ZZ_RUNTIME_H

#include "core.h"
#include "memory.h"
#include "strings.h"
#include "collections.h"
#include "json.h"

#endif // ZZ_RUNTIME_H