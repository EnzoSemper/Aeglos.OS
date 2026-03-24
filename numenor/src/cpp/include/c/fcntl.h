#pragma once

#ifdef __cplusplus
extern "C" {
#endif

// Open flags (dummy values)
#define O_RDONLY 0
#define O_RDWR 1
#define O_CREAT 2

int open(const char *pathname, int flags, ...);

#ifdef __cplusplus
}
#endif
