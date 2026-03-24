#pragma once

#ifdef __cplusplus
extern "C" {
#endif

typedef int sig_atomic_t;

#define SIGINT 2
#define SIGILL 4
#define SIGABRT 6
#define SIGFPE 8
#define SIGSEGV 11
#define SIGTERM 15

typedef void (*__sighandler_t)(int);
#define SIG_DFL ((__sighandler_t)0)
#define SIG_ERR ((__sighandler_t) - 1)
#define SIG_IGN ((__sighandler_t)1)

void (*signal(int sig, void (*func)(int)))(int);
int raise(int sig);

#ifdef __cplusplus
}
#endif
