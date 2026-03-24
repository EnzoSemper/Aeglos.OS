#ifndef _SCHED_H
#define _SCHED_H

#ifdef __cplusplus
extern "C" {
#endif

// Stub for cpu_set_t and sched functions
typedef struct {
  unsigned long __bits[1];
} cpu_set_t;

#define CPU_SET(cpu, cpusetp)
#define CPU_ZERO(cpusetp)
#define CPU_ISSET(cpu, cpusetp) 0
#define CPU_COUNT(cpusetp) 1

int sched_yield(void);
int sched_getaffinity(int pid, size_t cpusetsize, cpu_set_t *mask);
int sched_setaffinity(int pid, size_t cpusetsize, const cpu_set_t *mask);

#ifdef __cplusplus
}
#endif

#endif
