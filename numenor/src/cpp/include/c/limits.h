#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#define PATH_MAX 4096
#define NAME_MAX 255
#define SEM_VALUE_MAX 32767
#define CHAR_BIT 8

#include <stdint.h>
#define INT_MAX __INT_MAX__
#define INT_MIN (-__INT_MAX__ - 1)
#define UINT_MAX (__INT_MAX__ * 2U + 1U)
#define LLONG_MAX __LONG_LONG_MAX__
#define LLONG_MIN (-__LONG_LONG_MAX__ - 1LL)
#define ULLONG_MAX (__LONG_LONG_MAX__ * 2ULL + 1ULL)

#ifdef __cplusplus
}
#endif
