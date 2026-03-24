#include <alloca.h>
#include <ctype.h>
#include <inttypes.h>
#include <math.h>
#include <shim_unistd.h>
#include <signal.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

#include "errno.h"
#include <pthread.h>

extern "C" {

// Generic syscall wrapper
// Generic syscall wrapper
long syscall_2(long n, long arg0, long arg1) {
  register long x8 asm("x8") = n;
  register long x0 asm("x0") = arg0;
  register long x1 asm("x1") = arg1;
  asm volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1) : "memory", "cc");
  return x0;
}

long syscall_3(long n, long arg0, long arg1, long arg2) {
  register long x8 asm("x8") = n;
  register long x0 asm("x0") = arg0;
  register long x1 asm("x1") = arg1;
  register long x2 asm("x2") = arg2;
  asm volatile("svc #0"
               : "+r"(x0)
               : "r"(x8), "r"(x1), "r"(x2)
               : "memory", "cc");
  return x0;
}

// SYS_BLK_READ = 6
static long sys_blk_read(size_t sector, void *buf, size_t len) {
  return syscall_3(6, (long)sector, (long)buf, (long)len);
}

// SYS_LOG = 8
void shim_console_puts(const char *s) {
  size_t len = 0;
  while (s[len])
    len++;
  syscall_2(8, (long)s, (long)len);
}

// --- FILE Shim — dual-model support ---
//
// drive.img layout (physical sectors, 512 bytes each):
//   [  0 MB.. 512 MB)  FAT32           sector 0
//   [512 MB..   6 GB)  Qwen3-8B        sector MODEL_BASE_SECTOR  = 1048576
//   [  6 GB.. 6.5 GB)  Qwen3-0.6B      sector FAST_MODEL_BASE_SECTOR = 12582912
//   [  7 GB..7GB+10MB) Semantic store  (block_manager.rs)
//
// fopen("model.gguf", ...)       → main  8B model
// fopen("model_fast.gguf", ...)  → fast 0.6B model

typedef struct {
  size_t offset;
  size_t size;
  int valid;
  uint8_t *data;
} FILE_HANDLE;

static const size_t MODEL_BASE_SECTOR      = 1048576;   // 512 MB / 512
static const size_t FAST_MODEL_BASE_SECTOR = 12582912;  // 6 GB / 512
static const size_t MODEL_FILE_SIZE      = 5500ULL * 1024 * 1024; // 5.5 GB cap
static const size_t FAST_MODEL_FILE_SIZE =  512ULL * 1024 * 1024; // 512 MB cap

static FILE_HANDLE g_handles[2]      = {};
static uint8_t    *g_model_caches[2] = {};

typedef struct FILE FILE;

static FILE_HANDLE *fh(FILE *f) {
  if (f == (FILE *)&g_handles[0]) return &g_handles[0];
  if (f == (FILE *)&g_handles[1]) return &g_handles[1];
  return nullptr;
}

FILE *fopen(const char *filename, const char *mode) {
  (void)mode;
  if (!filename) return nullptr;

  int slot;
  size_t base_sector, file_size;

  if (strstr(filename, "model_fast.gguf")) {
    slot = 1; base_sector = FAST_MODEL_BASE_SECTOR; file_size = FAST_MODEL_FILE_SIZE;
  } else if (strstr(filename, "model.gguf")) {
    slot = 0; base_sector = MODEL_BASE_SECTOR;      file_size = MODEL_FILE_SIZE;
  } else {
    return nullptr;
  }

  g_handles[slot].offset = 0;
  g_handles[slot].size   = file_size;
  g_handles[slot].valid  = 1;

  if (!g_model_caches[slot]) {
    const char *tag = (slot == 0) ? "[shim] Loading main model...\n"
                                  : "[shim] Loading fast model...\n";
    shim_console_puts(tag);
    size_t alloc = ((file_size + 511) / 512) * 512;
    g_model_caches[slot] = (uint8_t *)malloc(alloc);
    if (!g_model_caches[slot]) {
      shim_console_puts("[shim] FATAL: model malloc failed\n");
      g_handles[slot].valid = 0;
      return nullptr;
    }
    sys_blk_read(base_sector, g_model_caches[slot], alloc);
    shim_console_puts("[shim] model loaded OK\n");
  }
  g_handles[slot].data = g_model_caches[slot];
  return (FILE *)&g_handles[slot];
}

int fclose(FILE *stream) {
  FILE_HANDLE *h = fh(stream);
  if (!h) return -1;
  h->valid = 0;
  return 0;
}

int remove(const char *pathname) { return 0; }
int fflush(FILE *stream) { return 0; }
int ferror(FILE *stream) { return 0; }
int fileno(FILE *stream) { return -1; }

long ftell(FILE *stream) {
  FILE_HANDLE *h = fh(stream);
  if (!h || !h->valid) return -1L;
  return (long)h->offset;
}

int fseek(FILE *stream, long offset, int whence) {
  FILE_HANDLE *h = fh(stream);
  if (!h || !h->valid) return -1;
  if      (whence == 0) h->offset  = (size_t)offset;
  else if (whence == 1) h->offset += (size_t)offset;
  else if (whence == 2) h->offset  = h->size + (size_t)offset;
  return 0;
}

static unsigned long g_malloc_count = 0;

size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream) {
  FILE_HANDLE *h = fh(stream);
  if (!h || !h->valid || !h->data) return 0;
  size_t total = size * nmemb;
  size_t avail = (h->offset < h->size) ? (h->size - h->offset) : 0;
  if (total > avail) total = avail;
  memcpy(ptr, h->data + h->offset, total);
  h->offset += total;
  return total / size;
}

size_t fwrite(const void *ptr, size_t size, size_t count, FILE *stream) {
  return 0;
}

// extern "C" void sys_console_puts(const char *s); // Defined above

// vsnprintf implementation supporting format specifiers used by llama.cpp/GGML:
// %d, %i, %u, %x, %X, %p, %s, %c, %f, %g, %e, %%, %ld, %lu, %lx,
// %lld, %llu, %llx, %zu, %zd, %zx, PRIu32/PRIu64/PRId64
// Also supports width, zero-fill, and '-' left-align flags.
int vsnprintf(char *buffer, size_t buf_size, const char *format, va_list args) {
  // C standard: vsnprintf(NULL, 0, fmt, args) must return the length that
  // *would* have been written. Use a scratch buffer for the dry run.
  char scratch[4096];
  if (!buffer || buf_size == 0) {
    buffer = scratch;
    buf_size = sizeof(scratch);
  }

  char *p = buffer;
  char *end = buffer + buf_size - 1;

  for (const char *f = format; *f; ++f) {
    if (p >= end)
      break;

    if (*f != '%') {
      *p++ = *f;
      continue;
    }

    ++f;
    if (!*f)
      break;

    // Literal %
    if (*f == '%') {
      *p++ = '%';
      continue;
    }

    // Parse flags
    bool left_align = false;
    bool zero_pad = false;
    bool plus_sign = false;
    bool space_sign = false;
    bool hash_flag = false;
    while (*f == '-' || *f == '0' || *f == '+' || *f == ' ' || *f == '#') {
      if (*f == '-')
        left_align = true;
      if (*f == '0')
        zero_pad = true;
      if (*f == '+')
        plus_sign = true;
      if (*f == ' ')
        space_sign = true;
      if (*f == '#')
        hash_flag = true;
      ++f;
    }
    if (left_align)
      zero_pad = false;

    // Parse width
    int width = 0;
    if (*f == '*') {
      width = va_arg(args, int);
      ++f;
    } else {
      while (*f >= '0' && *f <= '9') {
        width = width * 10 + (*f - '0');
        ++f;
      }
    }

    // Parse precision
    int precision = -1;
    if (*f == '.') {
      ++f;
      precision = 0;
      if (*f == '*') {
        precision = va_arg(args, int);
        ++f;
      } else {
        while (*f >= '0' && *f <= '9') {
          precision = precision * 10 + (*f - '0');
          ++f;
        }
      }
    }

    // Parse length modifier: l, ll, z, h, hh, j, t, L, I64, I32
    int length = 0; // 0=int, 1=long, 2=long long, 3=size_t
    if (*f == 'l') {
      ++f;
      if (*f == 'l') {
        length = 2;
        ++f;
      } else {
        length = 1;
      }
    } else if (*f == 'z') {
      length = 3;
      ++f;
    } else if (*f == 'h') {
      ++f;
      if (*f == 'h')
        ++f; // hh -> treat as int
    } else if (*f == 'j' || *f == 't' || *f == 'L') {
      length = 2;
      ++f;
    } else if (*f == 'I') {
      // MSVC-style I64, I32
      if (f[1] == '6' && f[2] == '4') {
        length = 2;
        f += 3;
      } else if (f[1] == '3' && f[2] == '2') {
        length = 0;
        f += 3;
      }
    }

    if (!*f)
      break;

    char tmp[80];
    int n = 0;
    bool is_negative = false;

    switch (*f) {
    case 's': {
      const char *s = va_arg(args, const char *);
      if (!s)
        s = "(null)";
      int slen = 0;
      while (s[slen])
        slen++;
      if (precision >= 0 && slen > precision)
        slen = precision;
      int pad = (width > slen) ? width - slen : 0;
      if (!left_align) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      for (int i = 0; i < slen && p < end; i++)
        *p++ = s[i];
      if (left_align) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      break;
    }
    case 'c': {
      int ch = va_arg(args, int);
      *p++ = (char)ch;
      break;
    }
    case 'd':
    case 'i': {
      long long val;
      if (length == 2)
        val = va_arg(args, long long);
      else if (length == 1 || length == 3)
        val = va_arg(args, long);
      else
        val = va_arg(args, int);

      is_negative = val < 0;
      unsigned long long uval = is_negative
                                    ? (unsigned long long)(-(val + 1)) + 1
                                    : (unsigned long long)val;
      if (uval == 0)
        tmp[n++] = '0';
      else {
        while (uval) {
          tmp[n++] = (uval % 10) + '0';
          uval /= 10;
        }
      }

      // Apply width/padding
      char sign_char = 0;
      if (is_negative)
        sign_char = '-';
      else if (plus_sign)
        sign_char = '+';
      else if (space_sign)
        sign_char = ' ';

      int total = n + (sign_char ? 1 : 0);
      int pad = (width > total) ? width - total : 0;

      if (!left_align && !zero_pad) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      if (sign_char && p < end)
        *p++ = sign_char;
      if (!left_align && zero_pad) {
        while (pad-- > 0 && p < end)
          *p++ = '0';
      }
      while (n > 0 && p < end)
        *p++ = tmp[--n];
      if (left_align) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      break;
    }
    case 'u': {
      unsigned long long val;
      if (length == 2)
        val = va_arg(args, unsigned long long);
      else if (length == 1 || length == 3)
        val = (unsigned long long)va_arg(args, unsigned long);
      else
        val = (unsigned long long)va_arg(args, unsigned int);

      if (val == 0)
        tmp[n++] = '0';
      else {
        while (val) {
          tmp[n++] = (val % 10) + '0';
          val /= 10;
        }
      }

      int pad = (width > n) ? width - n : 0;
      if (!left_align && !zero_pad) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      if (!left_align && zero_pad) {
        while (pad-- > 0 && p < end)
          *p++ = '0';
      }
      while (n > 0 && p < end)
        *p++ = tmp[--n];
      if (left_align) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      break;
    }
    case 'x':
    case 'X': {
      unsigned long long val;
      if (length == 2)
        val = va_arg(args, unsigned long long);
      else if (length == 1 || length == 3)
        val = (unsigned long long)va_arg(args, unsigned long);
      else
        val = (unsigned long long)va_arg(args, unsigned int);

      const char *digits =
          (*f == 'X') ? "0123456789ABCDEF" : "0123456789abcdef";
      if (val == 0)
        tmp[n++] = '0';
      else {
        while (val) {
          tmp[n++] = digits[val & 0xF];
          val >>= 4;
        }
      }

      int extra = (hash_flag && val != 0) ? 2 : 0;
      int pad = (width > n + extra) ? width - n - extra : 0;
      if (!left_align && !zero_pad) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      if (hash_flag) {
        if (p < end)
          *p++ = '0';
        if (p < end)
          *p++ = (*f == 'X') ? 'X' : 'x';
      }
      if (!left_align && zero_pad) {
        while (pad-- > 0 && p < end)
          *p++ = '0';
      }
      while (n > 0 && p < end)
        *p++ = tmp[--n];
      if (left_align) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      break;
    }
    case 'p': {
      unsigned long val = (unsigned long)va_arg(args, void *);
      if (p + 2 < end) {
        *p++ = '0';
        *p++ = 'x';
      }
      if (val == 0) {
        tmp[n++] = '0';
      } else {
        while (val) {
          int d = val & 0xF;
          tmp[n++] = (d < 10) ? (d + '0') : (d - 10 + 'a');
          val >>= 4;
        }
      }
      while (n > 0 && p < end)
        *p++ = tmp[--n];
      break;
    }
    case 'f':
    case 'g':
    case 'e':
    case 'G':
    case 'E': {
      double val = va_arg(args, double);
      if (precision < 0)
        precision = 6;
      is_negative = val < 0;
      if (is_negative)
        val = -val;

      // Simple float formatting (no scientific notation for %e/%g)
      unsigned long long ipart = (unsigned long long)val;
      double frac = val - (double)ipart;

      // Integer part
      if (ipart == 0)
        tmp[n++] = '0';
      else {
        while (ipart) {
          tmp[n++] = (ipart % 10) + '0';
          ipart /= 10;
        }
      }

      char num_buf[80];
      int pos = 0;
      if (is_negative)
        num_buf[pos++] = '-';
      for (int i = n - 1; i >= 0; i--)
        num_buf[pos++] = tmp[i];

      if (precision > 0) {
        num_buf[pos++] = '.';
        for (int i = 0; i < precision && i < 20; i++) {
          frac *= 10.0;
          int digit = (int)frac;
          num_buf[pos++] = digit + '0';
          frac -= digit;
        }
      }
      num_buf[pos] = 0;

      int pad = (width > pos) ? width - pos : 0;
      if (!left_align) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      for (int i = 0; i < pos && p < end; i++)
        *p++ = num_buf[i];
      if (left_align) {
        while (pad-- > 0 && p < end)
          *p++ = ' ';
      }
      break;
    }
    default:
      // Unknown specifier, just emit it
      *p++ = *f;
      break;
    }
  }

  *p = '\0';
  return (int)(p - buffer);
}

int printf(const char *format, ...) {
  static char
      buf[2048]; // Static buffer to avoid stack pointer issues in syscalls
  va_list args;
  va_start(args, format);
  int ret = vsnprintf(buf, sizeof(buf), format, args);
  va_end(args);
  shim_console_puts(buf);
  return ret;
}

int fprintf(FILE *stream, const char *format, ...) {
  // Always map to printer - we assume fprintf is only used for logging/stdout
  // in this context if (stream == stderr || stream == stdout) {
  char buf[2048];
  va_list args;
  va_start(args, format);
  int ret = vsnprintf(buf, sizeof(buf), format, args);
  va_end(args);
  shim_console_puts(buf);
  return ret;
  // }
  // return 0;
}

int snprintf(char *s, size_t n, const char *format, ...) {
  va_list args;
  va_start(args, format);
  int ret = vsnprintf(s, n, format, args);
  va_end(args);
  return ret;
}

// remove vsnprintf prototype if we implemented it above
// But vsnprintf was defined at line 55 previously.
// We replaced the block, so the old definition is gone.

int sprintf(char *s, const char *format, ...) {
  va_list args;
  va_start(args, format);
  int ret = vsnprintf(s, 65536, format, args); // Unsafe unbounded
  va_end(args);
  return ret;
}

int putchar(int c) {
  char tmp[2] = {(char)c, 0};
  shim_console_puts(tmp);
  return c;
}

int fputc(int c, FILE *stream) { return c; }

int fputs(const char *s, FILE *stream) {
  shim_console_puts(s);
  return 0;
}

void *memset(void *s, int c, size_t n) {
  unsigned char *p = (unsigned char *)s;
  unsigned char cv = (unsigned char)c;

  // Create a 64-bit pattern of the byte
  uint64_t c64 = cv;
  c64 |= c64 << 8;
  c64 |= c64 << 16;
  c64 |= c64 << 32;

  uint64_t *p64 = (uint64_t *)p;
  while (n >= 8) {
    *p64++ = c64;
    n -= 8;
  }

  p = (unsigned char *)p64;
  while (n--) {
    *p++ = cv;
  }

  return s;
}

void *memcpy(void *__restrict dest, const void *__restrict src, size_t n) {
  unsigned char *__restrict d = (unsigned char *)dest;
  const unsigned char *__restrict s = (const unsigned char *)src;

  // Bulk copy via 64-bit words unrolled
  uint64_t *__restrict d64 = (uint64_t *)d;
  const uint64_t *__restrict s64 = (const uint64_t *)s;

  while (n >= 32) {
    d64[0] = s64[0];
    d64[1] = s64[1];
    d64[2] = s64[2];
    d64[3] = s64[3];
    d64 += 4;
    s64 += 4;
    n -= 32;
  }

  while (n >= 8) {
    *d64++ = *s64++;
    n -= 8;
  }

  // Handle remaining odd bytes
  d = (unsigned char *)d64;
  s = (const unsigned char *)s64;
  while (n--) {
    *d++ = *s++;
  }

  return dest;
}

int strcmp(const char *s1, const char *s2) {
  while (*s1 && (*s1 == *s2)) {
    s1++;
    s2++;
  }
  return *(const unsigned char *)s1 - *(const unsigned char *)s2;
}

size_t strlen(const char *s) {
  size_t len = 0;
  while (*s++)
    len++;
  return len;
}

char *strcpy(char *dest, const char *src) {
  char *saved = dest;
  while ((*dest++ = *src++))
    ;
  return saved;
}

// Math stubs if libm exports fail (but we exported them from Rust)
// We might need to map pow -> powf for llama.cpp if it uses doubles?
// Rust exports: pow(f64), powf(f32).
// C++ symbols might be mangled or just use standard names.
// extern "C" in Rust prevents mangling, so 'powf' is available.

__attribute__((optnone)) void abort() {
  shim_console_puts("ABORT called!\n");
  while (1) {
    asm volatile("wfe");
  }
}

__attribute__((optnone)) void exit(int status) {
  shim_console_puts("EXIT called\n");
  while (1) {
    asm volatile("wfe");
  }
}

int abs(int n) { return (n < 0) ? -n : n; }

// Ctype stubs
int isspace(int c) {
  return (c == ' ' || c == '\t' || c == '\n' || c == '\v' || c == '\f' ||
          c == '\r');
}
int tolower(int c) { return (c >= 'A' && c <= 'Z') ? (c + 32) : c; }
int isalpha(int c) { return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'); }
int isdigit(int c) { return (c >= '0' && c <= '9'); }
int isalnum(int c) { return isalpha(c) || isdigit(c); }
int isxdigit(int c) {
  return isdigit(c) || (c >= 'A' && c <= 'F') || (c >= 'a' && c <= 'f');
}
int isupper(int c) { return (c >= 'A' && c <= 'Z'); }
int isprint(int c) { return c >= 32 && c < 127; }

// Math stubs - using builtins ensures they link to potential intrinsics or libm
// if available Note: We avoid 'using namespace std;' to prevent conflicts if we
// include <cmath>

// Math functions provided by compiler_builtins/libm

// Math functions provided by compiler_builtins/libm

// Shell Sort (O(N^1.5) or better, much faster than Selection Sort)
void qsort(void *base, size_t nmemb, size_t size,
           int (*compar)(const void *, const void *)) {
  if (nmemb < 2 || size == 0)
    return;

  char *b = (char *)base;
  char *tmp = (char *)__builtin_alloca(size);

  for (size_t gap = nmemb / 2; gap > 0; gap /= 2) {
    for (size_t i = gap; i < nmemb; i++) {
      // temp = a[i]
      memcpy(tmp, b + i * size, size);

      size_t j;
      for (j = i; j >= gap; j -= gap) {
        if (compar(b + (j - gap) * size, tmp) > 0) {
          memcpy(b + j * size, b + (j - gap) * size, size);
        } else {
          break;
        }
      }
      memcpy(b + j * size, tmp, size);
    }
  }
}

// End of duplicates

div_t div(int numer, int denom) { return {numer / denom, numer % denom}; }
ldiv_t ldiv(long numer, long denom) { return {numer / denom, numer % denom}; }
lldiv_t lldiv(long long numer, long long denom) {
  return {numer / denom, numer % denom};
}

// Stubs for llama-grammar and llama-quant
struct llama_grammar;
struct llama_model_quantize_params;

struct llama_grammar *
llama_grammar_init(const struct llama_grammar_element **rules, size_t n_rules,
                   size_t start_rule_index) {
  return NULL;
}
void llama_grammar_free(struct llama_grammar *grammar) {}
struct llama_grammar *llama_grammar_copy(const struct llama_grammar *grammar) {
  return NULL;
}
void llama_sample_grammar(struct llama_context *ctx,
                          struct llama_token_data_array *candidates,
                          const struct llama_grammar *grammar) {}
void llama_grammar_accept_token(struct llama_context *ctx,
                                struct llama_grammar *grammar, int32_t token) {}

uint32_t
llama_model_quantize(const char *fname_inp, const char *fname_out,
                     const struct llama_model_quantize_params *params) {
  return 1; // Error
}

// POSIX Stubs
struct stat;
int open(const char *pathname, int flags, ...) { return -1; }
int close(int fd) { return 0; }
long read(int fd, void *buf, size_t count) { return -1; }
long lseek(int fd, long offset, int whence) { return -1; }
int stat(const char *pathname, struct stat *statbuf) {
  // printf("[shim] stat(%s)\n", pathname);
  return -1;
}
int fstat(int fd, struct stat *statbuf) { return -1; }
// int posix_memalign(void **memptr, size_t alignment, size_t size) { ... }
// defined below in Allocator Stubs
int getpagesize(void) { return 4096; }

// Signal stubs
void (*signal(int sig, void (*func)(int)))(int) { return SIG_ERR; }
int raise(int sig) { return 0; }

long sysconf(int name) {
  if (name == _SC_PAGE_SIZE)
    return 4096;
  if (name == _SC_PHYS_PAGES)
    return 1024 * 1024 * 1024 / 4096; // 1GB
  return -1;
}

// --- IO Stubs ---
FILE *stdout = (FILE *)1;
FILE *stderr = (FILE *)2;

int puts(const char *s) {
  printf("%s\n", s);
  return 0;
}

// Model loading support
struct FakeFile {
  bool is_model;
  size_t pos;
  size_t size;
};
const size_t MODEL_OFFSET = 32 * 1024 * 1024;
const size_t SECTOR_SIZE = 512;

// --- C++ ABI Stubs ---
extern "C" {
void __cxa_pure_virtual() {
  printf("pure virtual function called\n");
  abort();
}
void __cxa_deleted_virtual() {
  printf("deleted virtual function called\n");
  abort();
}
void __cxa_atexit() {} // Ignore atexit
__attribute__((optnone)) void _Unwind_Resume() {
  shim_console_puts("_Unwind_Resume called!\n");
  while (1) {
    asm volatile("wfe");
  }
}

__attribute__((optnone)) void __abort_message(const char *msg) {
  shim_console_puts("abort_message: ");
  shim_console_puts(msg);
  shim_console_puts("\n");
  while (1) {
    asm volatile("wfe");
  }
}
}

// --- GGML Stubs ---
extern "C" {
void ggml_critical_section_start() {}
void ggml_critical_section_end() {}
}

namespace std {
void __throw_length_error(const char *msg) {
  printf("length_error: %s\n", msg);
  abort();
}
} // namespace std

} // extern "C"

// --- Static Initialization Guards (Single Threaded) ---
extern "C" {
int __cxa_guard_acquire(long long *guard_object) {
  if (*((char *)guard_object) == 0) {
    return 1; // Needs initialization
  }
  return 0; // Already initialized
}
void __cxa_guard_release(long long *guard_object) {
  *((char *)guard_object) = 1; // Initialized
}
void __cxa_guard_abort(long long *guard_object) {
  *((char *)guard_object) = 0; // Reset
}

// --- Exception Handling Personality (Stub) ---
__attribute__((optnone)) void __gxx_personality_v0() {
  shim_console_puts("__gxx_personality_v0 called!\n");
  while (1) {
    asm volatile("wfe");
  }
}
}

// --- Minimal Math Implementation (approx) ---
extern "C" {

// Math functions provided by Rust/compiler_builtins: sqrtf, sqrt, expf, logf,
// fabs, fabsf, tanhf, erff.

float expm1f(float x) { return expf(x) - 1.0f; }

// Pthread stubs
int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start_routine)(void *), void *arg) {
  return 11; // EAGAIN
}
int pthread_join(pthread_t thread, void **retval) { return 0; }
int pthread_mutex_init(pthread_mutex_t *mutex, const void *attr) { return 0; }
int pthread_mutex_destroy(pthread_mutex_t *mutex) { return 0; }
int pthread_mutex_lock(pthread_mutex_t *mutex) { return 0; }
int pthread_mutex_unlock(pthread_mutex_t *mutex) { return 0; }
// Cond
int pthread_cond_init(pthread_cond_t *cond, const void *attr) { return 0; }
int pthread_cond_destroy(pthread_cond_t *cond) { return 0; }
int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex) {
  return 0;
}
int pthread_cond_signal(pthread_cond_t *cond) { return 0; }
int pthread_cond_broadcast(pthread_cond_t *cond) { return 0; }

} // extern "C"

// --- dlfcn Stubs ---
extern "C" {
void *dlopen(const char *filename, int flags) { return nullptr; }
int dlclose(void *handle) { return 0; }
void *dlsym(void *handle, const char *symbol) { return nullptr; }
char *dlerror(void) { return (char *)"Dynamic loading not supported"; }
}

// RTTI stubs provided by libcxxabi/private_typeinfo.cpp
#include <typeinfo>

// --- Exception handling stubs with correct signatures ---
extern "C" {
void __cxa_free_exception(void *ptr) noexcept { free(ptr); }
void *__cxa_allocate_exception(size_t thrown_size) noexcept {
  return malloc(thrown_size);
}
__attribute__((optnone)) void __cxa_throw(void *thrown_exception,
                                          std::type_info *tinfo,
                                          void (*dest)(void *)) {
  shim_console_puts("__cxa_throw called!\n");
  while (1) {
    asm volatile("wfe");
  }
}
__attribute__((optnone)) void *
__cxa_begin_catch(void *exceptionObject) noexcept {
  shim_console_puts("__cxa_begin_catch called!\n");
  while (1) {
    asm volatile("wfe");
  }
}
__attribute__((optnone)) void __cxa_end_catch() {
  shim_console_puts("__cxa_end_catch called!\n");
  while (1) {
    asm volatile("wfe");
  }
}
}

// --- Allocator ---
// Fast bump allocator with a large arena to avoid per-allocation syscalls.
// Small allocations come from the arena; large ones fall back to kernel.
extern "C" {

static const size_t ARENA_SIZE = 256 * 1024 * 1024; // 256 MB arena
static const size_t BUMP_THRESHOLD = 65536; // Allocs > 64KB go to kernel
static uint8_t *arena_base = nullptr;
static size_t arena_used = 0;

static void arena_init() {
  if (!arena_base) {
    // Single syscall to get the entire arena
    arena_base = (uint8_t *)syscall_2(9, (long)ARENA_SIZE, 0);
    if (!arena_base) {
      shim_console_puts("[shim] FATAL: arena alloc failed\n");
    }
    arena_used = 0;
  }
}

void *malloc(size_t size) {
  g_malloc_count++;
  if (size == 0) size = 1;

  // Large allocations go directly to kernel
  if (size > BUMP_THRESHOLD) {
    return (void *)syscall_2(9, (long)size, 0);
  }

  arena_init();

  // Align to 16 bytes
  size_t aligned_size = (size + 15) & ~(size_t)15;
  if (arena_base && arena_used + aligned_size <= ARENA_SIZE) {
    void *ptr = arena_base + arena_used;
    arena_used += aligned_size;
    return ptr;
  }

  // Arena exhausted, fall back to kernel
  return (void *)syscall_2(9, (long)size, 0);
}

void free(void *ptr) {
  // Bump allocator: ignore frees for arena memory (it's never reclaimed)
  if (arena_base && ptr >= arena_base && ptr < arena_base + ARENA_SIZE) {
    return; // no-op for bump-allocated memory
  }
  // SYS_FREE = 10
  syscall_2(10, (long)ptr, 0);
}

void *realloc(void *ptr, size_t size) {
  if (!ptr) return malloc(size);
  if (size == 0) { free(ptr); return nullptr; }

  // For arena pointers, just malloc new + copy
  if (arena_base && ptr >= arena_base && ptr < arena_base + ARENA_SIZE) {
    void *new_ptr = malloc(size);
    if (new_ptr) {
      // We don't know the old size exactly, so copy up to 'size' bytes.
      // This is safe because bump memory stays valid.
      memcpy(new_ptr, ptr, size);
    }
    return new_ptr;
  }

  // SYS_REALLOC = 11
  return (void *)syscall_2(11, (long)ptr, (long)size);
}

void *aligned_alloc(size_t alignment, size_t size) {
  if (size <= BUMP_THRESHOLD && alignment <= 16) {
    return malloc(size); // Already 16-byte aligned
  }
  // SYS_ALIGNED_ALLOC = 12
  return (void *)syscall_2(12, (long)alignment, (long)size);
}

int posix_memalign(void **memptr, size_t alignment, size_t size) {
  void *ptr = aligned_alloc(alignment, size);
  if (!ptr)
    return 12; // ENOMEM
  *memptr = ptr;
  return 0;
}

// Calloc uses malloc + memset
void *calloc(size_t nmemb, size_t size) {
  size_t total = nmemb * size;
  void *p = malloc(total);
  if (p)
    memset(p, 0, total);
  return p;
}
}

namespace std {

[[noreturn]] void terminate() noexcept {
  printf("std::terminate called\n");
  abort();
}

typedef void (*new_handler)();
new_handler get_new_handler() noexcept { return nullptr; }
} // namespace std

// Ensure std::exception and std::bad_alloc vtables/methods comprise
#include <exception>
#include <new>

namespace std {
// exception::~exception() is usually defined in libcxx source, but link fails.
// We define it here if missing.
// Note: libcxx headers might define these as inline or default.
// If undefined symbol error persists, we implement them.

exception::~exception() noexcept {}

const char *exception::what() const noexcept { return "std::exception"; }

bad_alloc::bad_alloc() noexcept {}
bad_alloc::~bad_alloc() noexcept {}
const char *bad_alloc::what() const noexcept { return "std::bad_alloc"; }

bad_array_new_length::bad_array_new_length() noexcept {}
bad_array_new_length::~bad_array_new_length() noexcept {}
const char *bad_array_new_length::what() const noexcept {
  return "std::bad_array_new_length";
}

} // namespace std
extern "C" {
// Simple strncmp
int strncmp(const char *s1, const char *s2, size_t n) {
  while (n && *s1 && (*s1 == *s2)) {
    s1++;
    s2++;
    n--;
  }
  if (n == 0)
    return 0;
  return *(const unsigned char *)s1 - *(const unsigned char *)s2;
}

// Simple memchr
void *memchr(const void *s, int c, size_t n) {
  const unsigned char *p = (const unsigned char *)s;
  while (n--) {
    if (*p == (unsigned char)c)
      return (void *)p;
    p++;
  }
  return NULL;
}

// Simple strtol (stub - supports base 10 only partially)
long strtol(const char *nptr, char **endptr, int base) {
  long result = 0;
  int sign = 1;
  while (isspace(*nptr))
    nptr++;
  if (*nptr == '-') {
    sign = -1;
    nptr++;
  } else if (*nptr == '+') {
    nptr++;
  }

  // Very basic parsing
  while (isdigit(*nptr)) {
    result = result * 10 + (*nptr - '0');
    nptr++;
  }
  if (endptr)
    *endptr = (char *)nptr;
  return sign * result;
}

// strerror stub
char *strerror(int errnum) { return (char *)"Unknown error"; }

// clock_gettime stub

int clock_gettime(int clk_id, struct timespec *tp) {
  if (tp) {
    tp->tv_sec = 0;
    tp->tv_nsec = 0;
  }
  return 0;
}

long lroundf(float x) { return (long)(x + (x >= 0 ? 0.5f : -0.5f)); }

// getenv stub
char *getenv(const char *name) { return nullptr; }

// atoi stub
int atoi(const char *nptr) { return (int)strtol(nptr, nullptr, 10); }

// strstr stub
char *strstr(const char *haystack, const char *needle) {
  if (!*needle)
    return (char *)haystack;
  for (; *haystack; haystack++) {
    const char *h = haystack;
    const char *n = needle;
    while (*h && *n && *h == *n) {
      h++;
      n++;
    }
    if (!*n)
      return (char *)haystack;
  }
  return nullptr;
}

} // extern "C"

// sscanf is implemented in the POSIX shim completions block below.

// Call C++ global constructors from .init_array
extern "C" {
extern void (*__init_array_start[])(void);
extern void (*__init_array_end[])(void);

void call_global_ctors(void) {
  size_t count = (size_t)(__init_array_end - __init_array_start);
  for (size_t i = 0; i < count; i++) {
    __init_array_start[i]();
  }
}
}

// Strong operator new override (replaces weak definition from libc++ new.cpp)
#include <new>
void *operator new(unsigned long size) {
  if (size == 0)
    size = 1;
  void *p = malloc(size);
  if (!p) {
    shim_console_puts("[shim] operator new FAILED - abort\n");
    abort();
  }
  return p;
}
void *operator new[](unsigned long size) { return ::operator new(size); }
void operator delete(void *ptr) noexcept { free(ptr); }
void operator delete[](void *ptr) noexcept { free(ptr); }
void operator delete(void *ptr, unsigned long) noexcept { free(ptr); }
void operator delete[](void *ptr, unsigned long) noexcept { free(ptr); }

// ═══════════════════════════════════════════════════════════════════════════════
// POSIX shim completions — functions missing from the original shim.
// Goal: allow porting additional C/C++ libraries without modification.
// ═══════════════════════════════════════════════════════════════════════════════

extern "C" {

// ── Memory ───────────────────────────────────────────────────────────────────

void *memmove(void *dest, const void *src, size_t n) {
    unsigned char *d = (unsigned char *)dest;
    const unsigned char *s = (const unsigned char *)src;
    if (d == s || n == 0) return dest;
    if (d < s || d >= s + n) {
        // No overlap or dest is before src — forward copy
        for (size_t i = 0; i < n; i++) d[i] = s[i];
    } else {
        // Overlap with dest ahead — backward copy
        for (size_t i = n; i > 0; i--) d[i-1] = s[i-1];
    }
    return dest;
}

int memcmp(const void *s1, const void *s2, size_t n) {
    const unsigned char *a = (const unsigned char *)s1;
    const unsigned char *b = (const unsigned char *)s2;
    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i]) return (int)a[i] - (int)b[i];
    }
    return 0;
}

// ── String construction ───────────────────────────────────────────────────────

char *strcat(char *dest, const char *src) {
    char *p = dest;
    while (*p) p++;
    while ((*p++ = *src++));
    return dest;
}

char *strncat(char *dest, const char *src, size_t n) {
    char *p = dest;
    while (*p) p++;
    while (n-- && (*p = *src++)) p++;
    *p = '\0';
    return dest;
}

char *strncpy(char *dest, const char *src, size_t n) {
    size_t i;
    for (i = 0; i < n && src[i]; i++) dest[i] = src[i];
    for (; i < n; i++) dest[i] = '\0';
    return dest;
}

char *strdup(const char *s) {
    size_t len = strlen(s) + 1;
    char *p = (char *)malloc(len);
    if (p) memcpy(p, s, len);
    return p;
}

char *strndup(const char *s, size_t n) {
    size_t len = 0;
    while (len < n && s[len]) len++;
    char *p = (char *)malloc(len + 1);
    if (p) { memcpy(p, s, len); p[len] = '\0'; }
    return p;
}

// ── String search ─────────────────────────────────────────────────────────────

char *strchr(const char *s, int c) {
    for (; *s; s++) if ((unsigned char)*s == (unsigned char)c) return (char*)s;
    return (c == '\0') ? (char*)s : nullptr;
}

char *strrchr(const char *s, int c) {
    const char *last = nullptr;
    for (; *s; s++) if ((unsigned char)*s == (unsigned char)c) last = s;
    if (c == '\0') return (char*)s;
    return (char*)last;
}

char *strtok_r(char *str, const char *delim, char **saveptr) {
    if (!str) str = *saveptr;
    // Skip leading delimiters
    while (*str && strchr(delim, *str)) str++;
    if (!*str) { *saveptr = str; return nullptr; }
    char *start = str;
    while (*str && !strchr(delim, *str)) str++;
    if (*str) { *str++ = '\0'; }
    *saveptr = str;
    return start;
}

static char *strtok_state = nullptr;
char *strtok(char *str, const char *delim) {
    return strtok_r(str, delim, &strtok_state);
}

// ── Case-insensitive string comparison ───────────────────────────────────────

int strcasecmp(const char *s1, const char *s2) {
    while (*s1 && *s2) {
        int d = tolower((unsigned char)*s1) - tolower((unsigned char)*s2);
        if (d) return d;
        s1++; s2++;
    }
    return tolower((unsigned char)*s1) - tolower((unsigned char)*s2);
}

int strncasecmp(const char *s1, const char *s2, size_t n) {
    for (; n; n--, s1++, s2++) {
        int d = tolower((unsigned char)*s1) - tolower((unsigned char)*s2);
        if (d) return d;
        if (!*s1) break;
    }
    return 0;
}

// ── Number conversion (unified internal helper) ───────────────────────────────

// Internal: parse unsigned integer of any base (2–36).
static unsigned long long _strtoull_base(const char *p, char **endptr, int base) {
    while (isspace((unsigned char)*p)) p++;
    int neg = 0;
    if (*p == '-') { neg = 1; p++; }
    else if (*p == '+') p++;

    if (base == 0) {
        if (p[0] == '0' && (p[1] == 'x' || p[1] == 'X')) { base = 16; p += 2; }
        else if (p[0] == '0') { base = 8; p++; }
        else base = 10;
    } else if (base == 16 && p[0] == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
    }

    unsigned long long val = 0;
    const char *start = p;
    for (;;) {
        int d;
        if (*p >= '0' && *p <= '9') d = *p - '0';
        else if (*p >= 'a' && *p <= 'z') d = *p - 'a' + 10;
        else if (*p >= 'A' && *p <= 'Z') d = *p - 'A' + 10;
        else break;
        if (d >= base) break;
        val = val * (unsigned)base + (unsigned)d;
        p++;
    }
    if (endptr) *endptr = (char*)(p == start ? (p - (base == 16 ? 2 : 0)) : p);
    return neg ? (unsigned long long)(-(long long)val) : val;
}

// Fix base-10-only strtol to support full base detection.
// (Replaces the earlier stub that only did decimal.)
long strtol_full(const char *nptr, char **endptr, int base) {
    return (long)_strtoull_base(nptr, endptr, base);
}

unsigned long strtoul(const char *nptr, char **endptr, int base) {
    return (unsigned long)_strtoull_base(nptr, endptr, base);
}

long long strtoll(const char *nptr, char **endptr, int base) {
    return (long long)_strtoull_base(nptr, endptr, base);
}

unsigned long long strtoull(const char *nptr, char **endptr, int base) {
    return _strtoull_base(nptr, endptr, base);
}

double strtod(const char *nptr, char **endptr) {
    const char *p = nptr;
    while (isspace((unsigned char)*p)) p++;
    int neg = 0;
    if (*p == '-') { neg = 1; p++; }
    else if (*p == '+') p++;

    double val = 0.0;
    while (isdigit((unsigned char)*p)) { val = val * 10.0 + (*p - '0'); p++; }
    if (*p == '.') {
        p++;
        double scale = 0.1;
        while (isdigit((unsigned char)*p)) {
            val += (*p - '0') * scale;
            scale *= 0.1;
            p++;
        }
    }
    if (*p == 'e' || *p == 'E') {
        p++;
        int eneg = 0;
        if (*p == '-') { eneg = 1; p++; } else if (*p == '+') p++;
        int exp = 0;
        while (isdigit((unsigned char)*p)) { exp = exp * 10 + (*p - '0'); p++; }
        double mult = 1.0;
        for (int i = 0; i < exp; i++) mult *= 10.0;
        val = eneg ? val / mult : val * mult;
    }
    if (endptr) *endptr = (char*)p;
    return neg ? -val : val;
}

float strtof(const char *nptr, char **endptr) {
    return (float)strtod(nptr, endptr);
}

long atol(const char *nptr) { return (long)_strtoull_base(nptr, nullptr, 10); }
long long atoll(const char *nptr) { return (long long)_strtoull_base(nptr, nullptr, 10); }
double atof(const char *nptr) { return strtod(nptr, nullptr); }

// ── ctype completions ─────────────────────────────────────────────────────────

int toupper(int c) { return (c >= 'a' && c <= 'z') ? (c - 32) : c; }
int islower(int c) { return (c >= 'a' && c <= 'z'); }
int iscntrl(int c) { return (c >= 0 && c < 32) || c == 127; }
int ispunct(int c) { return isprint(c) && !isalnum(c) && c != ' '; }
int isblank(int c) { return c == ' ' || c == '\t'; }
int isascii(int c) { return (unsigned)c < 128; }

// ── Time functions using PL031 RTC ────────────────────────────────────────────

static uint32_t rtc_epoch_shim() {
    // SYS_GET_RTC = 31
    long ret;
    register long x8 asm("x8") = 31;
    register long x0 asm("x0") = 0;
    asm volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory");
    return (uint32_t)x0;
}

typedef long time_t;
time_t time(time_t *t) {
    time_t epoch = (time_t)rtc_epoch_shim();
    if (t) *t = epoch;
    return epoch;
}

typedef long clock_t;
// CLOCKS_PER_SEC is usually 1e6; return timer ticks as microseconds (rough)
clock_t clock() {
    uint64_t cnt;
    asm volatile("mrs %0, cntpct_el0" : "=r"(cnt));
    // AArch64 timer freq is typically 62.5 MHz on QEMU; approximate as µs
    return (clock_t)(cnt / 62);
}

// ── Process / scheduling ──────────────────────────────────────────────────────

typedef int pid_t;
pid_t getpid()  { return 1; } // Numenor is always TID 1
pid_t getppid() { return 0; }

int sched_yield() {
    // SYS_YIELD = 3
    register long x8 asm("x8") = 3;
    register long x0 asm("x0") = 0;
    asm volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory");
    return 0;
}

int usleep(unsigned int usec) { (void)usec; sched_yield(); return 0; }

struct timespec;
int nanosleep(const struct timespec *req, struct timespec *rem) {
    (void)req; (void)rem; sched_yield(); return 0;
}

// ── pthread completions ───────────────────────────────────────────────────────

// pthread_t is already defined via pthread.h; return a sentinel value.
pthread_t pthread_self()  { return (pthread_t)1; }
int pthread_equal(pthread_t a, pthread_t b) { return a == b; }
int pthread_setname_np(pthread_t, const char*) { return 0; }

// pthread_attr helpers (attrs are ignored on bare metal — no real threads)
int pthread_attr_init(pthread_attr_t *a)            { if(a) memset(a,0,sizeof(*a)); return 0; }
int pthread_attr_destroy(pthread_attr_t *a)          { (void)a; return 0; }
int pthread_attr_setstacksize(pthread_attr_t *a, size_t s) { (void)a; (void)s; return 0; }
int pthread_attr_getstacksize(const pthread_attr_t *a, size_t *s) { (void)a; if(s)*s=65536; return 0; }
int pthread_attr_setdetachstate(pthread_attr_t *a, int d) { (void)a; (void)d; return 0; }
int pthread_attr_getdetachstate(const pthread_attr_t *a, int *d) { (void)a; if(d)*d=0; return 0; }
int pthread_attr_setstackaddr(pthread_attr_t *a, void *p) { (void)a; (void)p; return 0; }
int pthread_attr_getstackaddr(const pthread_attr_t *a, void **p) { (void)a; if(p)*p=nullptr; return 0; }
int pthread_attr_setstack(pthread_attr_t *a, void *p, size_t s) { (void)a;(void)p;(void)s; return 0; }
int pthread_attr_getstack(const pthread_attr_t *a, void **p, size_t *s) {
    (void)a; if(p)*p=nullptr; if(s)*s=65536; return 0;
}

// pthread_key (TLS keys — single-threaded stub: one global slot per key)
#define MAX_PTHREAD_KEYS 16
static void *_key_vals[MAX_PTHREAD_KEYS];
typedef unsigned int pthread_key_t;
int pthread_key_create(pthread_key_t *key, void (*destr)(void*)) {
    (void)destr;
    for (unsigned k = 0; k < MAX_PTHREAD_KEYS; k++) {
        if (!_key_vals[k]) { *key = k; return 0; }
    }
    return 11; // EAGAIN
}
int pthread_key_delete(pthread_key_t key) { _key_vals[key] = nullptr; return 0; }
void *pthread_getspecific(pthread_key_t key) { return _key_vals[key]; }
int pthread_setspecific(pthread_key_t key, const void *val) {
    _key_vals[key] = (void*)val; return 0;
}

// pthread_once
typedef int pthread_once_t;
int pthread_once(pthread_once_t *once, void (*init)()) {
    if (*once == 0) { *once = 1; init(); }
    return 0;
}

// ── POSIX semaphores ──────────────────────────────────────────────────────────

// sem_t: single-threaded stub (no real blocking needed)
typedef struct { volatile int val; } sem_t;
int sem_init(sem_t *sem, int pshared, unsigned int value) {
    (void)pshared; if (sem) sem->val = (int)value; return 0;
}
int sem_destroy(sem_t *sem) { (void)sem; return 0; }
int sem_post(sem_t *sem) { if (sem) sem->val++; return 0; }
int sem_wait(sem_t *sem) {
    if (!sem) return -1;
    while (sem->val <= 0) sched_yield(); // spin (no real concurrency)
    sem->val--;
    return 0;
}
int sem_trywait(sem_t *sem) {
    if (!sem || sem->val <= 0) return -1;
    sem->val--;
    return 0;
}

// ── FILE stream completions ───────────────────────────────────────────────────

int feof(FILE *stream) {
    FILE_HANDLE *h = fh(stream);
    if (!h || !h->valid) return 1;
    return (h->offset >= h->size) ? 1 : 0;
}

void clearerr(FILE *stream) { (void)stream; }

void rewind(FILE *stream) {
    FILE_HANDLE *h = fh(stream);
    if (h && h->valid) h->offset = 0;
}

char *fgets(char *s, int size, FILE *stream) {
    if (!s || size <= 0) return nullptr;
    FILE_HANDLE *h = fh(stream);
    if (!h || !h->valid || !h->data) return nullptr;
    if (h->offset >= h->size) return nullptr;
    int i = 0;
    while (i < size - 1 && h->offset < h->size) {
        char c = (char)h->data[h->offset++];
        s[i++] = c;
        if (c == '\n') break;
    }
    s[i] = '\0';
    return i > 0 ? s : nullptr;
}

// ── sscanf — minimal but complete implementation ───────────────────────────────
// Supports: %d %i %u %x %X %f %g %e %s %c %% %n
// Length modifiers: l ll h hh z
// Width specifier.

int vsscanf(const char *str, const char *fmt, va_list ap) {
    const char *sp = str;
    int matched = 0;

    for (const char *f = fmt; *f; f++) {
        if (*f != '%') {
            if (isspace((unsigned char)*f)) {
                while (isspace((unsigned char)*sp)) sp++;
            } else {
                if (*sp == *f) sp++;
                else return matched;
            }
            continue;
        }

        f++; // skip '%'
        if (!*f) break;
        if (*f == '%') { if (*sp == '%') { sp++; } else return matched; continue; }

        // Parse width
        int width = 0;
        while (*f >= '0' && *f <= '9') { width = width * 10 + (*f - '0'); f++; }

        // Parse length modifier
        int len = 0; // 0=int, 1=long, 2=llong, 3=size_t
        if (*f == 'l') { f++; if (*f == 'l') { len = 2; f++; } else len = 1; }
        else if (*f == 'h') { f++; if (*f == 'h') f++; } // treat hh/h as int
        else if (*f == 'z') { len = 3; f++; }

        if (!*f) break;

        while (isspace((unsigned char)*sp)) sp++;

        switch (*f) {
        case 'd': case 'i': {
            char *end;
            long long val = (long long)_strtoull_base(sp, &end, (*f == 'i') ? 0 : 10);
            if (end == sp) return matched;
            if (len == 2) *va_arg(ap, long long*) = val;
            else if (len >= 1) *va_arg(ap, long*) = (long)val;
            else *va_arg(ap, int*) = (int)val;
            sp = end; matched++;
            break;
        }
        case 'u': case 'x': case 'X': {
            char *end;
            int base = (*f == 'u') ? 10 : 16;
            unsigned long long val = _strtoull_base(sp, &end, base);
            if (end == sp) return matched;
            if (len == 2) *va_arg(ap, unsigned long long*) = val;
            else if (len >= 1) *va_arg(ap, unsigned long*) = (unsigned long)val;
            else *va_arg(ap, unsigned int*) = (unsigned int)val;
            sp = end; matched++;
            break;
        }
        case 'f': case 'g': case 'e':
        case 'F': case 'G': case 'E': {
            char *end;
            double val = strtod(sp, &end);
            if (end == sp) return matched;
            if (len >= 1) *va_arg(ap, double*) = val;
            else *va_arg(ap, float*) = (float)val;
            sp = end; matched++;
            break;
        }
        case 's': {
            if (!*sp) return matched;
            char *out = va_arg(ap, char*);
            int n = 0;
            int lim = width > 0 ? width : 0x7FFFFFFF;
            while (*sp && !isspace((unsigned char)*sp) && n < lim) {
                *out++ = *sp++; n++;
            }
            *out = '\0';
            matched++;
            break;
        }
        case 'c': {
            char *out = va_arg(ap, char*);
            int lim = width > 0 ? width : 1;
            for (int i = 0; i < lim && *sp; i++) *out++ = *sp++;
            matched++;
            break;
        }
        case 'n': {
            *va_arg(ap, int*) = (int)(sp - str);
            break; // %n does not increment matched
        }
        default:
            break;
        }
    }
    return matched;
}

// Replace the earlier sscanf stub with this complete version.
// The previous sscanf only handled "blk.%d." — this handles the full format.
int sscanf(const char *str, const char *format, ...) {
    va_list ap;
    va_start(ap, format);
    int ret = vsscanf(str, format, ap);
    va_end(ap);
    return ret;
}

// ── locale stubs ──────────────────────────────────────────────────────────────

struct lconv {
    char *decimal_point;
    char *thousands_sep;
    // ... (only the fields callers typically touch)
};
static struct lconv _lconv = { (char*)".", (char*)"" };
struct lconv *localeconv() { return &_lconv; }
char *setlocale(int cat, const char *loc) { (void)cat; (void)loc; return (char*)"C"; }

// ── environ ───────────────────────────────────────────────────────────────────

char **environ = nullptr;

// ── Wide-char stubs (for completeness) ───────────────────────────────────────

typedef unsigned int wchar_t_shim;
size_t wcslen(const wchar_t *s) {
    size_t n = 0; while (s[n]) n++; return n;
}
int wctomb(char *s, wchar_t wc) {
    if (!s) return 0;
    if ((unsigned)wc < 0x80) { *s = (char)wc; return 1; }
    return -1;
}
int mbtowc(wchar_t *pwc, const char *s, size_t n) {
    if (!s || n == 0) return 0;
    if (pwc) *pwc = (wchar_t)(unsigned char)*s;
    return *s ? 1 : 0;
}

} // extern "C"
