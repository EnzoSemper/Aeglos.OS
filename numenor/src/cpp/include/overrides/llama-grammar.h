#pragma once

#include "llama.h"

struct llama_grammar {
  // Dummy content, we don't use it
  int dummy;
};

// We don't need the complex Internal grammar structures if we stub the API.
// But we might need declarations if llama-sampling uses particular fields?
// Usually llama-sampling treats it as opaque pointer or calls helper functions.
