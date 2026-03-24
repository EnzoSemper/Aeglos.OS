#pragma once

#ifdef NDEBUG
#define assert(expression) ((void)0)
#else
#define assert(expression) ((void)0) /* TODO: Panic/Abort */
#endif
