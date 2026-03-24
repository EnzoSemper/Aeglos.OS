#pragma once
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int mbstate_t;
typedef int wint_t;
#define WEOF ((wint_t) - 1)

size_t mbrtowc(wchar_t *pwc, const char *s, size_t n, mbstate_t *ps);
size_t wcrtomb(char *s, wchar_t wc, mbstate_t *ps);
int mbsinit(const mbstate_t *ps);

#ifdef __cplusplus
}
#endif
