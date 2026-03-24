#pragma once
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef long time_t;
typedef long clock_t;

struct timespec {
  time_t tv_sec;
  long tv_nsec;
};

#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCKS_PER_SEC 1000000

struct tm {
  int tm_sec;
  int tm_min;
  int tm_hour;
  int tm_mday;
  int tm_mon;
  int tm_year;
  int tm_wday;
  int tm_yday;
  int tm_isdst;
};

time_t time(time_t *tloc);
double difftime(time_t time1, time_t time0);
time_t mktime(struct tm *tm);
size_t strftime(char *s, size_t max, const char *format, const struct tm *tm);
struct tm *gmtime(const time_t *timep);
struct tm *localtime(const time_t *timep);

int clock_gettime(int clk_id, struct timespec *tp);
clock_t clock(void);

#ifdef __cplusplus
}
#endif
