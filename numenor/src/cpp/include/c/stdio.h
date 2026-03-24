#pragma once
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FILE FILE;
extern FILE *stdin, *stdout, *stderr;

#define EOF (-1)
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

#include <stdarg.h> // for va_list

int printf(const char *format, ...);
int fprintf(FILE *stream, const char *format, ...);
int snprintf(char *str, size_t size, const char *format, ...);
int sscanf(const char *str, const char *format, ...);
int vsnprintf(char *str, size_t size, const char *format, va_list ap);
int sprintf(char *str, const char *format, ...);
int putchar(int c);
int fputc(int c, FILE *stream);
int fputs(const char *s, FILE *stream);

// File I/O stubs
FILE *fopen(const char *filename, const char *mode);
int fclose(FILE *stream);
int remove(const char *pathname);
int fflush(FILE *stream);
int ferror(FILE *stream);
int fileno(FILE *stream);
long ftell(FILE *stream);
int fseek(FILE *stream, long offset, int origin);
size_t fread(void *ptr, size_t size, size_t count, FILE *stream);
size_t fwrite(const void *ptr, size_t size, size_t count, FILE *stream);

#ifdef __cplusplus
}
#endif
