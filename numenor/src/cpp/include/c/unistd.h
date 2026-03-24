#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

// Types defined in sys/types.h

int close(int fd);
ssize_t read(int fd, void *buf, size_t count);
off_t lseek(int fd, off_t offset, int whence);
int posix_memalign(void **memptr, size_t alignment, size_t size);
int getpagesize(void);

#define _SC_PAGESIZE 1
#define _SC_PAGE_SIZE _SC_PAGESIZE
#define _SC_PHYS_PAGES 2
long sysconf(int name);

#ifdef __cplusplus
}
#endif
