#pragma once

#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

struct stat {
  long st_size;
  int st_mode;
  long st_mtime;
};

// Mode constants
#define S_IFMT 0170000
#define S_IFREG 0100000
#define S_IFDIR 0040000

#define S_ISREG(m) (((m) & S_IFMT) == S_IFREG)
#define S_ISDIR(m) (((m) & S_IFMT) == S_IFDIR)

int stat(const char *pathname, struct stat *statbuf);
int fstat(int fd, struct stat *statbuf);

#ifdef __cplusplus
}
#endif
