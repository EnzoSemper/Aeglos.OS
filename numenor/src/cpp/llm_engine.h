#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Initialize the LLM engine
void llm_init();

// Run inference (buffered — returns when full response is ready).
// Returns number of bytes written to output buffer.
size_t llm_infer(const char* prompt, size_t prompt_len, char* output, size_t output_len);

// Run streaming inference — calls callback for each generated token piece.
// userdata is passed through to callback unchanged.
// Returns total bytes delivered to callback.
typedef void (*llm_token_cb)(const char *piece, int len, void *userdata);
size_t llm_infer_streaming(const char* prompt, size_t prompt_len,
                           llm_token_cb callback, void *userdata);
size_t llm_fast_infer_streaming(const char* prompt, size_t prompt_len,
                                llm_token_cb callback, void *userdata);

// Clear conversation history (start fresh multi-turn session).
void llm_history_clear(void);

// Generate embedding vector for text.
// Writes up to out_dim float32 values into out[].
// Returns actual number of floats written (0 on failure).
int llm_embedding(const char* text, int text_len, float* out, int out_dim);

// Run inference on the fast (small) model — lower latency, less capable.
// Falls back to the main model if the fast model failed to load.
// Returns number of bytes written to output buffer.
size_t llm_fast_infer(const char* prompt, size_t prompt_len,
                      char* output, size_t output_len);

// Returns the embedding dimension of the currently-loaded model (0 if none).
int llm_get_embedding_dim(void);

#ifdef __cplusplus
}
#endif
