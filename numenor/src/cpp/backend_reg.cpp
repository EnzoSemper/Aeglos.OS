// Backend Registry Implementation
// Since ggml-backend-reg.cpp is excluded, we implement the registry here.

#include "ggml-backend.h"
#include "ggml-cpu.h"
#include <cstring>
#include <vector>

extern "C" void shim_console_puts(const char *s);

static std::vector<ggml_backend_reg_t> g_backends;

extern "C" {

void ggml_backend_register(ggml_backend_reg_t reg) {
  if (!reg)
    return;
  // Check duplicates
  for (auto r : g_backends) {
    if (r == reg)
      return;
    if (strcmp(ggml_backend_reg_name(r), ggml_backend_reg_name(reg)) == 0)
      return;
  }
  shim_console_puts("backend_reg: registered backend\n");
  g_backends.push_back(reg);
}

size_t ggml_backend_reg_count() { return g_backends.size(); }

ggml_backend_reg_t ggml_backend_reg_get(size_t index) {
  if (index >= g_backends.size())
    return nullptr;
  return g_backends[index];
}

ggml_backend_reg_t ggml_backend_reg_by_name(const char *name) {
  for (auto r : g_backends) {
    if (strcmp(ggml_backend_reg_name(r), name) == 0)
      return r;
  }
  return nullptr;
}

// Register CPU backend on load_all
void ggml_backend_load_all() {
  shim_console_puts("[backend] ggml_backend_load_all: registering CPU backend\n");
  ggml_backend_register(ggml_backend_cpu_reg());
}
void ggml_backend_load_all_from_path(const char *dir_path) {
  (void)dir_path;
  ggml_backend_load_all();
}

ggml_backend_reg_t ggml_backend_load(const char *path) {
  (void)path;
  return nullptr;
}

void ggml_backend_unload(ggml_backend_reg_t reg) { (void)reg; }

// Stub for device_register if needed, or remove if not used
void ggml_backend_device_register(ggml_backend_dev_t device) { (void)device; }

size_t ggml_backend_dev_count() {
  size_t count = 0;
  for (auto reg : g_backends) {
    count += ggml_backend_reg_dev_count(reg);
  }
  return count;
}

ggml_backend_dev_t ggml_backend_dev_get(size_t index) {
  for (auto reg : g_backends) {
    size_t n = ggml_backend_reg_dev_count(reg);
    if (index < n) {
      return ggml_backend_reg_dev_get(reg, index);
    }
    index -= n;
  }
  return nullptr;
}

ggml_backend_dev_t ggml_backend_dev_by_name(const char *name) {
  for (size_t i = 0; i < ggml_backend_dev_count(); i++) {
    ggml_backend_dev_t dev = ggml_backend_dev_get(i);
    if (dev && strcmp(ggml_backend_dev_name(dev), name) == 0) {
      return dev;
    }
  }
  return nullptr;
}

ggml_backend_dev_t ggml_backend_dev_by_type(enum ggml_backend_dev_type type) {
  for (size_t i = 0; i < ggml_backend_dev_count(); i++) {
    ggml_backend_dev_t dev = ggml_backend_dev_get(i);
    if (dev && ggml_backend_dev_type(dev) == type) {
      return dev;
    }
  }
  return nullptr;
}

ggml_backend_t ggml_backend_init_best(void) {
  ggml_backend_dev_t dev =
      ggml_backend_dev_by_type(GGML_BACKEND_DEVICE_TYPE_CPU); // Prioritize CPU
  if (!dev) {
    // Try any
    dev = ggml_backend_dev_get(0);
  }
  if (!dev)
    return nullptr;
  return ggml_backend_dev_init(dev, nullptr);
}

ggml_backend_t ggml_backend_init_by_name(const char *name, const char *params) {
  ggml_backend_dev_t dev = ggml_backend_dev_by_name(name);
  if (!dev)
    return nullptr;
  return ggml_backend_dev_init(dev, params);
}

ggml_backend_t ggml_backend_init_by_type(enum ggml_backend_dev_type type,
                                         const char *params) {
  ggml_backend_dev_t dev = ggml_backend_dev_by_type(type);
  if (!dev)
    return nullptr;
  return ggml_backend_dev_init(dev, params);
}
}
