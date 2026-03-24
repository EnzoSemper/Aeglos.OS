#pragma once

#ifdef __cplusplus
extern "C" {
#endif

// Compiler builtin for alloca
#define alloca(size) __builtin_alloca(size)

#ifdef __cplusplus
}
#endif
