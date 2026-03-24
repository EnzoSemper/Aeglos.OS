#pragma once
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
  int quot, rem;
} div_t;
typedef struct {
  long quot, rem;
} ldiv_t;
typedef struct {
  long long quot, rem;
} lldiv_t;

div_t div(int numer, int denom);
ldiv_t ldiv(long numer, long denom);
lldiv_t lldiv(long long numer, long long denom);

int posix_memalign(void **memptr, size_t alignment, size_t size);

// Basic malloc functions
void *malloc(size_t size);
void free(void *ptr);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void *aligned_alloc(size_t alignment, size_t size); // C11

// String conversions
long strtol(const char *nptr, char **endptr, int base);
unsigned long strtoul(const char *nptr, char **endptr, int base);
long long strtoll(const char *nptr, char **endptr, int base);
unsigned long long strtoull(const char *nptr, char **endptr, int base);
int atoi(const char *nptr);
double atof(const char *nptr);
float strtof(const char *nptr, char **endptr);
double strtod(const char *nptr, char **endptr);
long double strtold(const char *nptr, char **endptr);

void abort(void);
void exit(int status);
int abs(int j);
char *getenv(const char *name); // often used

#ifdef __cplusplus
}
#endif
