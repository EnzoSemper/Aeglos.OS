#include "llm_engine.h"
#include "ggml-backend.h"
#include "llama.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>

// Direct syscall for debug output (known working)
extern "C" void shim_console_puts(const char *s);

// ── Global State ──
static llama_model *g_model = nullptr;
static llama_context *g_ctx = nullptr;
static const llama_vocab *g_vocab = nullptr;
static llama_sampler *g_sampler = nullptr;
static llama_context *g_emb_ctx = nullptr; // Embedding-only context

// ── Fast (small) model — Qwen3-0.6B, loaded from "model_fast.gguf" ──
static llama_model   *g_fast_model   = nullptr;
static llama_context *g_fast_ctx     = nullptr;
static const llama_vocab *g_fast_vocab = nullptr;
static llama_sampler *g_fast_sampler = nullptr;

static void dbg(const char *msg) { shim_console_puts(msg); }

// ── memmem shim (not always available on bare metal) ─────────────────────────
static void *aeglos_memmem(const void *hay, size_t hlen,
                           const void *ndl, size_t nlen) {
    if (nlen == 0) return (void *)hay;
    if (hlen < nlen) return nullptr;
    const char *h = (const char *)hay;
    const char *n = (const char *)ndl;
    for (size_t i = 0; i <= hlen - nlen; i++) {
        if (memcmp(h + i, n, nlen) == 0) return (void *)(h + i);
    }
    return nullptr;
}

// ── Conversation history ──────────────────────────────────────────────────────
// Rolling buffer of prior user/assistant turns in ChatML format.
// Each entry: "<|im_start|>user\nPROMPT<|im_end|>\n<|im_start|>assistant\nREPLY<|im_end|>\n"
// When buffer is full, the oldest complete turn is discarded.

#define HIST_SIZE 4096
static char  g_history[HIST_SIZE];
static int   g_history_len = 0;

// Append a completed user+assistant exchange to history.
// Drops oldest turn(s) if needed to make room.
static void history_append(const char *user, int ulen,
                           const char *asst, int alen) {
    // Build candidate entry
    static char entry[2048];
    int elen = snprintf(entry, sizeof(entry), "<|im_start|>user\n");
    if (elen < 0) elen = 0;
    int ucopy = ulen < (int)sizeof(entry) - elen - 1
              ? ulen : (int)sizeof(entry) - elen - 1;
    if (ucopy > 0) { memcpy(entry + elen, user, ucopy); elen += ucopy; }
    int rest = snprintf(entry + elen, sizeof(entry) - elen,
                        "<|im_end|>\n<|im_start|>assistant\n");
    if (rest > 0) elen += rest;
    int acopy = alen < (int)sizeof(entry) - elen - 1
              ? alen : (int)sizeof(entry) - elen - 1;
    if (acopy > 0) { memcpy(entry + elen, asst, acopy); elen += acopy; }
    int tail = snprintf(entry + elen, sizeof(entry) - elen, "<|im_end|>\n");
    if (tail > 0) elen += tail;
    if (elen <= 0 || elen >= (int)sizeof(entry)) return;

    // Evict oldest turn(s) until there is room
    const char *needle = "<|im_start|>user\n";
    int nlen = (int)strlen(needle);
    while (g_history_len + elen > HIST_SIZE) {
        char *next = nullptr;
        if (g_history_len > nlen) {
            next = (char *)aeglos_memmem(g_history + nlen,
                                         g_history_len - nlen, needle, nlen);
        }
        if (!next) { g_history_len = 0; break; }
        int drop = (int)(next - g_history);
        memmove(g_history, g_history + drop, g_history_len - drop);
        g_history_len -= drop;
    }
    if (g_history_len + elen <= HIST_SIZE) {
        memcpy(g_history + g_history_len, entry, elen);
        g_history_len += elen;
    }
}

extern "C" void llm_history_clear(void) {
    g_history_len = 0;
    memset(g_history, 0, sizeof(g_history));
}

// ── RTC helpers ──────────────────────────────────────────────────────────────

static uint32_t rtc_read_epoch(void) {
    // PL031 RTC on QEMU virt — data register at offset 0 gives Unix epoch secs.
    // Mapped at PA 0x09010000 + KERNEL_VA_OFFSET (TTBR1, same as all MMIO).
    volatile uint32_t *dr = (volatile uint32_t *)(
        (uintptr_t)0x09010000ULL + (uintptr_t)0xFFFF000000000000ULL);
    return *dr;
}

// Decomposes a Unix epoch into calendar fields.
// All fields are written through the pointers provided.
static void rtc_decompose(uint32_t epoch,
    unsigned *out_year, unsigned *out_month, unsigned *out_day,
    unsigned *out_h,    unsigned *out_m,     unsigned *out_s) {
    *out_s = epoch % 60;
    *out_m = (epoch / 60) % 60;
    *out_h = (epoch / 3600) % 24;
    unsigned days = epoch / 86400;
    unsigned year = 1970;
    for (;;) {
        unsigned diy = ((year%4==0 && year%100!=0) || year%400==0) ? 366 : 365;
        if (days < diy) break;
        days -= diy;
        year++;
    }
    int lp = ((year%4==0 && year%100!=0) || year%400==0);
    unsigned dim[12] = {31,(unsigned)(lp?29:28),31,30,31,30,31,31,30,31,30,31};
    unsigned month = 1, mday = 1;
    for (int i = 0; i < 12; i++) {
        if (days < dim[i]) { month = (unsigned)(i+1); mday = days+1; break; }
        days -= dim[i];
    }
    *out_year  = year;
    *out_month = month;
    *out_day   = mday;
}

// Custom log callback — routes llama.cpp/GGML logs to UART
static void bare_metal_log(enum ggml_log_level level, const char *text,
                           void *user_data) {
  (void)level;
  (void)user_data;
  if (text) {
    shim_console_puts(text);
  }
}

void llm_init() {
  dbg("[Numenor] llm_init entered\r\n");

  // Set custom log callback BEFORE anything else
  llama_log_set(bare_metal_log, nullptr);
  dbg("[Numenor] log callback set\r\n");

  llama_backend_init();
  dbg("[Numenor] backend_init done\r\n");

  // Load backends (registers CPU backend)
  ggml_backend_load_all();
  dbg("[Numenor] backends loaded\r\n");

  // ── Load Model ──
  struct llama_model_params mparams = llama_model_default_params();
  mparams.use_mmap = false;      // No mmap on bare metal
  mparams.check_tensors = false; // Disable heavy CPU FP validation
  dbg("[Numenor] default params done, loading model...\r\n");

  g_model = llama_model_load_from_file("model.gguf", mparams);
  if (!g_model) {
    dbg("[Numenor] FATAL: model load failed!\r\n");
    return;
  }
  dbg("[Numenor] Model loaded OK\r\n");

  g_vocab = llama_model_get_vocab(g_model);
  dbg("[Numenor] Got vocab\r\n");

  // ── Create Context ──
  struct llama_context_params cparams = llama_context_default_params();
  cparams.n_ctx = 2048;   // extended context window (was 512)
  cparams.n_batch = 512;  // prompt batch (must be >= max prompt tokens)
  cparams.n_ubatch = 128; // micro-batch for decode
  cparams.n_threads = 1;  // single-threaded: bare-metal pthread shim is limited
  cparams.n_threads_batch = 1;

  dbg("[Numenor] Creating context...\r\n");
  g_ctx = llama_init_from_model(g_model, cparams);
  if (!g_ctx) {
    dbg("[Numenor] FATAL: context creation failed!\r\n");
    return;
  }
  dbg("[Numenor] Context created\r\n");

  // ── Create Sampler Chain ──
  struct llama_sampler_chain_params sparams =
      llama_sampler_chain_default_params();
  g_sampler = llama_sampler_chain_init(sparams);
  llama_sampler_chain_add(g_sampler, llama_sampler_init_temp(0.8f));
  llama_sampler_chain_add(g_sampler, llama_sampler_init_top_k(40));
  llama_sampler_chain_add(g_sampler, llama_sampler_init_top_p(0.9f, 1));
  llama_sampler_chain_add(g_sampler, llama_sampler_init_dist(42));

  // ── Create Embedding Context ──
  // Separate llama_context with embeddings=true and mean pooling.
  // Uses same model weights (no extra disk I/O).
  struct llama_context_params epar = llama_context_default_params();
  epar.n_ctx          = 512;
  epar.n_batch        = 512;
  epar.n_threads      = 1;
  epar.n_threads_batch = 1;
  epar.embeddings     = true;
  epar.pooling_type   = LLAMA_POOLING_TYPE_MEAN;

  dbg("[Numenor] Creating embedding context...\r\n");
  g_emb_ctx = llama_init_from_model(g_model, epar);
  if (!g_emb_ctx) {
    dbg("[Numenor] WARNING: embedding context creation failed (RAG vectors disabled)\r\n");
    // Not fatal — inference still works, just no vector RAG.
  } else {
    dbg("[Numenor] Embedding context ready\r\n");
  }

  // ── Load Fast Model (Qwen3-0.6B) ──────────────────────────────────────────
  dbg("[Numenor] Loading fast model (model_fast.gguf)...\r\n");
  g_fast_model = llama_model_load_from_file("model_fast.gguf", mparams);
  if (!g_fast_model) {
    dbg("[Numenor] WARNING: fast model not found — all queries use 8B model\r\n");
  } else {
    g_fast_vocab = llama_model_get_vocab(g_fast_model);
    struct llama_context_params fpar = llama_context_default_params();
    fpar.n_ctx          = 1024;
    fpar.n_batch        = 256;
    fpar.n_ubatch       = 64;
    fpar.n_threads      = 1;
    fpar.n_threads_batch = 1;
    g_fast_ctx = llama_init_from_model(g_fast_model, fpar);
    if (!g_fast_ctx) {
      dbg("[Numenor] WARNING: fast model context failed\r\n");
    } else {
      struct llama_sampler_chain_params fspar = llama_sampler_chain_default_params();
      g_fast_sampler = llama_sampler_chain_init(fspar);
      llama_sampler_chain_add(g_fast_sampler, llama_sampler_init_temp(0.7f));
      llama_sampler_chain_add(g_fast_sampler, llama_sampler_init_top_k(40));
      llama_sampler_chain_add(g_fast_sampler, llama_sampler_init_top_p(0.9f, 1));
      llama_sampler_chain_add(g_fast_sampler, llama_sampler_init_dist(42));
      dbg("[Numenor] Fast model ready\r\n");
    }
  }

  dbg("[Numenor] Engine ready!\r\n");
}

size_t llm_infer(const char *prompt, size_t prompt_len, char *output,
                 size_t output_len) {
  if (!g_model || !g_ctx || !g_sampler) {
    const char *err = "Error: Engine not initialized";
    size_t len = strlen(err);
    if (len >= output_len)
      len = output_len - 1;
    memcpy(output, err, len);
    output[len] = '\0';
    return len;
  }

  // ── Clear KV cache for fresh inference ──
  llama_memory_clear(llama_get_memory(g_ctx), true);
  llama_sampler_reset(g_sampler);

  // ── Build ChatML buffer ──
  unsigned yr, mo, dy, hh, mm, ss;
  rtc_decompose(rtc_read_epoch(), &yr, &mo, &dy, &hh, &mm, &ss);

  static char chat_buf[6144]; // larger to fit history
  int pos = 0;

  // Part 1: system turn (includes tool manifest)
  int p1 = snprintf(chat_buf, sizeof(chat_buf),
      "<|im_start|>system\n"
      "You are Aeglos, an AI assistant running directly on bare-metal hardware "
      "inside the Aeglos OS microkernel — no Linux, no userspace beneath you. "
      "Current date and time: %04u-%02u-%02u %02u:%02u:%02u UTC. "
      "Answer questions directly and helpfully. Be concise and accurate.\n\n"
      "You have access to OS tools. To call a tool, output [[NAME:arg]] on its own "
      "line and stop — the system will execute it and show you the result in the next turn.\n"
      "Available tools:\n"
      "  [[FETCH:https://url]]        HTTP/S GET — returns response body\n"
      "  [[POST:https://url body]]    HTTPS POST with body\n"
      "  [[DNS:hostname]]             Resolve hostname to IP\n"
      "  [[PING:hostname]]            ICMP ping — returns RTT ms or timeout\n"
      "  [[LS:path]]                  List directory (default /)\n"
      "  [[CAT:path]]                 Read a file\n"
      "  [[SAVE:path content]]        Write content to a file\n"
      "  [[MEM_STORE:text]]           Store text in semantic memory\n"
      "  [[MEM_QUERY:query]]          Semantic similarity search\n"
      "  [[MEM_SEARCH:tag]]           Tag-based memory search\n"
      "  [[STATS]]                    System stats (RAM, CPU, tasks)\n"
      "Only invoke a tool when the user needs real-time data or file operations. "
      "Never fabricate tool results."
      "<|im_end|>\n",
      yr, mo, dy, hh, mm, ss);
  if (p1 > 0 && p1 < (int)sizeof(chat_buf)) pos = p1;

  // Part 2: conversation history (prior turns)
  if (g_history_len > 0) {
    int hcopy = g_history_len < (int)sizeof(chat_buf) - pos - 1
              ? g_history_len : (int)sizeof(chat_buf) - pos - 1;
    if (hcopy > 0) { memcpy(chat_buf + pos, g_history, hcopy); pos += hcopy; }
  }

  // Part 3: current user turn header
  static const char user_hdr[] = "<|im_start|>user\n";
  int hlen = sizeof(user_hdr) - 1;
  if (pos + hlen < (int)sizeof(chat_buf)) {
    memcpy(chat_buf + pos, user_hdr, hlen); pos += hlen;
  }

  // Part 4: user prompt
  int ulen = (int)prompt_len < (int)sizeof(chat_buf) - pos - 1
           ? (int)prompt_len : (int)sizeof(chat_buf) - pos - 1;
  if (ulen > 0) { memcpy(chat_buf + pos, prompt, ulen); pos += ulen; }

  // Part 5: closing user turn + assistant header
  static const char suffix[] = "<|im_end|>\n<|im_start|>assistant\n";
  int slen = sizeof(suffix) - 1;
  int chat_len = pos;
  if (chat_len + slen < (int)sizeof(chat_buf) - 1) {
    memcpy(chat_buf + chat_len, suffix, slen);
    chat_len += slen;
  }
  chat_buf[chat_len] = '\0';

  // ── Tokenize ──
  printf("[Numenor] Tokenizing prompt (%d chars)...\n", chat_len);

  llama_token tokens[2048];
  int32_t n_tokens =
      llama_tokenize(g_vocab, chat_buf, chat_len, tokens, 512,
                     false, // add_special — ChatML has its own special tokens
                     true   // parse_special — parse <|im_start|> etc.
      );

  if (n_tokens < 0) {
    snprintf(output, output_len, "Error: Tokenization failed (%d)", n_tokens);
    return strlen(output);
  }

  printf("[Numenor] Tokens: %d\n", n_tokens);

  // ── Decode prompt ──
  struct llama_batch batch = llama_batch_get_one(tokens, n_tokens);
  int32_t ret = llama_decode(g_ctx, batch);
  if (ret != 0) {
    snprintf(output, output_len, "Error: Decode failed (%d)", ret);
    return strlen(output);
  }

  printf("[Numenor] Prompt processed. Generating...\n");

  // ── Auto-regressive generation ──
  size_t out_pos = 0;
  const int max_gen = 512;   // extended generation length (was 256)

  for (int i = 0; i < max_gen; i++) {
    llama_token new_token = llama_sampler_sample(g_sampler, g_ctx, -1);

    // Check end-of-generation
    if (llama_vocab_is_eog(g_vocab, new_token)) {
      break;
    }

    // Convert token to text
    char piece[64];
    int32_t n_chars =
        llama_token_to_piece(g_vocab, new_token, piece, sizeof(piece),
                             0,   // lstrip
                             true // special
        );

    if (n_chars > 0) {
      if (out_pos + (size_t)n_chars >= output_len - 1)
        break;
      memcpy(output + out_pos, piece, n_chars);
      out_pos += n_chars;
    }

    // Feed token back for next step
    struct llama_batch next = llama_batch_get_one(&new_token, 1);
    if (llama_decode(g_ctx, next) != 0) {
      printf("[Numenor] Decode error at token %d\n", i);
      break;
    }
  }

  output[out_pos] = '\0';
  printf("[Numenor] Generated %d chars.\n", (int)out_pos);

  // Store exchange in conversation history
  if (out_pos > 0)
    history_append(prompt, (int)prompt_len, output, (int)out_pos);

  return out_pos;
}

// ── Fast inference ────────────────────────────────────────────────────────────
// Uses the small 0.6B model for low-latency responses.
// Falls back to llm_infer() if the fast model failed to load.
size_t llm_fast_infer(const char *prompt, size_t prompt_len,
                      char *output, size_t output_len) {
  // Fallback: if fast model not available, route to main model.
  if (!g_fast_model || !g_fast_ctx || !g_fast_sampler) {
    dbg("[Numenor] fast model unavailable — falling back to 8B\r\n");
    return llm_infer(prompt, prompt_len, output, output_len);
  }

  llama_memory_clear(llama_get_memory(g_fast_ctx), true);
  llama_sampler_reset(g_fast_sampler);

  // Build ChatML — same structure as main model.
  unsigned yr, mo, dy, hh, mm, ss;
  rtc_decompose(rtc_read_epoch(), &yr, &mo, &dy, &hh, &mm, &ss);

  static char fchat_buf[2048];
  int p1 = snprintf(fchat_buf, sizeof(fchat_buf),
      "<|im_start|>system\n"
      "You are Aeglos, a concise AI assistant on bare-metal hardware. "
      "Current date: %04u-%02u-%02u %02u:%02u:%02u UTC. "
      "Be brief and direct."
      "<|im_end|>\n"
      "<|im_start|>user\n",
      yr, mo, dy, hh, mm, ss);
  if (p1 <= 0 || p1 >= (int)sizeof(fchat_buf) - 1) p1 = 0;

  int space = (int)sizeof(fchat_buf) - p1 - 1;
  int ulen  = (int)prompt_len < space ? (int)prompt_len : space;
  if (ulen > 0) memcpy(fchat_buf + p1, prompt, ulen);

  static const char fsuffix[] = "<|im_end|>\n<|im_start|>assistant\n";
  int slen = sizeof(fsuffix) - 1;
  int flen = p1 + ulen;
  if (flen + slen < (int)sizeof(fchat_buf) - 1) {
    memcpy(fchat_buf + flen, fsuffix, slen);
    flen += slen;
  }
  fchat_buf[flen] = '\0';

  llama_token ftokens[1024];
  int32_t n_tokens = llama_tokenize(
      g_fast_vocab, fchat_buf, flen, ftokens, 512, false, true);
  if (n_tokens < 0) {
    snprintf(output, output_len, "Error: fast tokenize failed (%d)", n_tokens);
    return strlen(output);
  }

  struct llama_batch batch = llama_batch_get_one(ftokens, n_tokens);
  if (llama_decode(g_fast_ctx, batch) != 0) {
    snprintf(output, output_len, "Error: fast decode failed");
    return strlen(output);
  }

  size_t out_pos = 0;
  const int max_gen = 256; // Fast model: shorter responses

  for (int i = 0; i < max_gen; i++) {
    llama_token tok = llama_sampler_sample(g_fast_sampler, g_fast_ctx, -1);
    if (llama_vocab_is_eog(g_fast_vocab, tok)) break;

    char piece[64];
    int32_t nc = llama_token_to_piece(g_fast_vocab, tok, piece, sizeof(piece), 0, true);
    if (nc > 0) {
      if (out_pos + (size_t)nc >= output_len - 1) break;
      memcpy(output + out_pos, piece, nc);
      out_pos += nc;
    }

    struct llama_batch next = llama_batch_get_one(&tok, 1);
    if (llama_decode(g_fast_ctx, next) != 0) break;
  }

  output[out_pos] = '\0';
  printf("[Numenor] Fast generated %d chars.\n", (int)out_pos);
  return out_pos;
}

// ── Streaming inference ───────────────────────────────────────────────────────
// Like llm_infer but calls callback(piece, len, userdata) for each token piece
// instead of accumulating into an output buffer.  Returns total bytes delivered.
size_t llm_infer_streaming(const char *prompt, size_t prompt_len,
                           llm_token_cb callback, void *userdata) {
  if (!g_model || !g_ctx || !g_sampler || !callback) return 0;

  llama_memory_clear(llama_get_memory(g_ctx), true);
  llama_sampler_reset(g_sampler);

  unsigned yr, mo, dy, hh, mm, ss;
  rtc_decompose(rtc_read_epoch(), &yr, &mo, &dy, &hh, &mm, &ss);

  static char chat_buf[6144];
  int pos = 0;
  int p1 = snprintf(chat_buf, sizeof(chat_buf),
      "<|im_start|>system\n"
      "You are Aeglos, an AI assistant running directly on bare-metal hardware "
      "inside the Aeglos OS microkernel — no Linux, no userspace beneath you. "
      "Current date and time: %04u-%02u-%02u %02u:%02u:%02u UTC. "
      "Answer questions directly and helpfully. Be concise and accurate.\n\n"
      "You have access to OS tools. To call a tool, output [[NAME:arg]] on its own "
      "line and stop — the system will execute it and show you the result in the next turn.\n"
      "Available tools:\n"
      "  [[FETCH:https://url]]        HTTP/S GET — returns response body\n"
      "  [[POST:https://url body]]    HTTPS POST with body\n"
      "  [[DNS:hostname]]             Resolve hostname to IP\n"
      "  [[PING:hostname]]            ICMP ping — returns RTT ms or timeout\n"
      "  [[LS:path]]                  List directory (default /)\n"
      "  [[CAT:path]]                 Read a file\n"
      "  [[SAVE:path content]]        Write content to a file\n"
      "  [[MEM_STORE:text]]           Store text in semantic memory\n"
      "  [[MEM_QUERY:query]]          Semantic similarity search\n"
      "  [[MEM_SEARCH:tag]]           Tag-based memory search\n"
      "  [[STATS]]                    System stats (RAM, CPU, tasks)\n"
      "Only invoke a tool when the user needs real-time data or file operations. "
      "Never fabricate tool results."
      "<|im_end|>\n",
      yr, mo, dy, hh, mm, ss);
  if (p1 > 0 && p1 < (int)sizeof(chat_buf)) pos = p1;

  if (g_history_len > 0) {
    int hcopy = g_history_len < (int)sizeof(chat_buf) - pos - 1
              ? g_history_len : (int)sizeof(chat_buf) - pos - 1;
    if (hcopy > 0) { memcpy(chat_buf + pos, g_history, hcopy); pos += hcopy; }
  }

  static const char user_hdr[] = "<|im_start|>user\n";
  int hdrlen = sizeof(user_hdr) - 1;
  if (pos + hdrlen < (int)sizeof(chat_buf)) {
    memcpy(chat_buf + pos, user_hdr, hdrlen); pos += hdrlen;
  }
  int ulen = (int)prompt_len < (int)sizeof(chat_buf) - pos - 1
           ? (int)prompt_len : (int)sizeof(chat_buf) - pos - 1;
  if (ulen > 0) { memcpy(chat_buf + pos, prompt, ulen); pos += ulen; }

  static const char suffix[] = "<|im_end|>\n<|im_start|>assistant\n";
  int slen = sizeof(suffix) - 1;
  int chat_len = pos;
  if (chat_len + slen < (int)sizeof(chat_buf) - 1) {
    memcpy(chat_buf + chat_len, suffix, slen);
    chat_len += slen;
  }
  chat_buf[chat_len] = '\0';

  llama_token tokens[2048];
  int32_t n_tokens = llama_tokenize(g_vocab, chat_buf, chat_len, tokens, 512,
                                    false, true);
  if (n_tokens < 0) return 0;

  struct llama_batch batch = llama_batch_get_one(tokens, n_tokens);
  if (llama_decode(g_ctx, batch) != 0) return 0;

  // Accumulate output for history storage
  static char hist_out[1024];
  int hist_pos = 0;

  size_t total = 0;
  const int max_gen = 512;
  for (int i = 0; i < max_gen; i++) {
    llama_token new_token = llama_sampler_sample(g_sampler, g_ctx, -1);
    if (llama_vocab_is_eog(g_vocab, new_token)) break;

    char piece[64];
    int32_t n_chars = llama_token_to_piece(g_vocab, new_token, piece,
                                           sizeof(piece), 0, true);
    if (n_chars > 0) {
      callback(piece, n_chars, userdata);
      total += (size_t)n_chars;
      int copy = n_chars < (int)sizeof(hist_out) - hist_pos - 1
               ? n_chars : (int)sizeof(hist_out) - hist_pos - 1;
      if (copy > 0) { memcpy(hist_out + hist_pos, piece, copy); hist_pos += copy; }
    }

    struct llama_batch next = llama_batch_get_one(&new_token, 1);
    if (llama_decode(g_ctx, next) != 0) break;
  }

  // Store exchange in conversation history
  if (hist_pos > 0)
    history_append(prompt, (int)prompt_len, hist_out, hist_pos);

  return total;
}

// Streaming variant of the fast model.
size_t llm_fast_infer_streaming(const char *prompt, size_t prompt_len,
                                llm_token_cb callback, void *userdata) {
  if (!g_fast_model || !g_fast_ctx || !g_fast_sampler) {
    return llm_infer_streaming(prompt, prompt_len, callback, userdata);
  }
  if (!callback) return 0;

  llama_memory_clear(llama_get_memory(g_fast_ctx), true);
  llama_sampler_reset(g_fast_sampler);

  unsigned yr, mo, dy, hh, mm, ss;
  rtc_decompose(rtc_read_epoch(), &yr, &mo, &dy, &hh, &mm, &ss);

  static char fchat_buf[2048];
  int p1 = snprintf(fchat_buf, sizeof(fchat_buf),
      "<|im_start|>system\n"
      "You are Aeglos, a concise AI assistant on bare-metal hardware. "
      "Current date: %04u-%02u-%02u %02u:%02u:%02u UTC. "
      "Be brief and direct."
      "<|im_end|>\n"
      "<|im_start|>user\n",
      yr, mo, dy, hh, mm, ss);
  if (p1 <= 0 || p1 >= (int)sizeof(fchat_buf) - 1) p1 = 0;

  int space = (int)sizeof(fchat_buf) - p1 - 1;
  int ulen  = (int)prompt_len < space ? (int)prompt_len : space;
  if (ulen > 0) memcpy(fchat_buf + p1, prompt, ulen);

  static const char fsuffix[] = "<|im_end|>\n<|im_start|>assistant\n";
  int slen = sizeof(fsuffix) - 1;
  int flen = p1 + ulen;
  if (flen + slen < (int)sizeof(fchat_buf) - 1) {
    memcpy(fchat_buf + flen, fsuffix, slen);
    flen += slen;
  }
  fchat_buf[flen] = '\0';

  llama_token ftokens[1024];
  int32_t n_tokens = llama_tokenize(g_fast_vocab, fchat_buf, flen,
                                    ftokens, 512, false, true);
  if (n_tokens < 0) return 0;

  struct llama_batch batch = llama_batch_get_one(ftokens, n_tokens);
  if (llama_decode(g_fast_ctx, batch) != 0) return 0;

  size_t total = 0;
  const int max_gen = 256;
  for (int i = 0; i < max_gen; i++) {
    llama_token tok = llama_sampler_sample(g_fast_sampler, g_fast_ctx, -1);
    if (llama_vocab_is_eog(g_fast_vocab, tok)) break;

    char piece[64];
    int32_t nc = llama_token_to_piece(g_fast_vocab, tok, piece, sizeof(piece), 0, true);
    if (nc > 0) {
      callback(piece, nc, userdata);
      total += (size_t)nc;
    }

    struct llama_batch next = llama_batch_get_one(&tok, 1);
    if (llama_decode(g_fast_ctx, next) != 0) break;
  }
  return total;
}

// ── Embedding ─────────────────────────────────────────────────────────────────
// Generates a mean-pooled, L2-normalised embedding vector for `text`.
// Copies min(out_dim, model_hidden_dim) floats into `out`.
// Returns the number of floats written, or 0 on failure.
int llm_embedding(const char *text, int text_len, float *out, int out_dim) {
  if (!g_emb_ctx || !g_vocab || out_dim <= 0) {
    return 0;
  }

  // ── Tokenize (plain text, no ChatML) ──
  llama_token tokens[512];
  int32_t n_tokens = llama_tokenize(
      g_vocab, text, text_len, tokens, 512,
      true,  // add_special (BOS so pooling has a meaningful first token)
      false  // parse_special
  );
  if (n_tokens <= 0) {
    dbg("[Numenor][emb] Tokenize failed\r\n");
    return 0;
  }

  // ── Clear KV cache ──
  llama_memory_clear(llama_get_memory(g_emb_ctx), true);

  // ── Decode through embedding context ──
  struct llama_batch batch = llama_batch_get_one(tokens, n_tokens);
  if (llama_decode(g_emb_ctx, batch) != 0) {
    dbg("[Numenor][emb] Decode failed\r\n");
    return 0;
  }

  // ── Get mean-pooled embedding (sequence 0) ──
  const float *emb = llama_get_embeddings_seq(g_emb_ctx, 0);
  if (!emb) {
    // Fallback: last token embeddings
    emb = llama_get_embeddings_ith(g_emb_ctx, n_tokens - 1);
  }
  if (!emb) {
    dbg("[Numenor][emb] No embeddings returned\r\n");
    return 0;
  }

  // ── Determine how many dims we can copy ──
  // llama_model_n_embd() returns the model's hidden/embedding dimension.
  int model_dim = llama_model_n_embd(g_model);
  int copy_dim  = out_dim < model_dim ? out_dim : model_dim;

  // ── L2 normalise over copy_dim ──
  float norm_sq = 0.0f;
  for (int i = 0; i < copy_dim; i++) {
    norm_sq += emb[i] * emb[i];
  }
  float inv_norm = (norm_sq > 1e-12f) ? (1.0f / sqrtf(norm_sq)) : 0.0f;

  for (int i = 0; i < copy_dim; i++) {
    out[i] = emb[i] * inv_norm;
  }
  // Zero-fill remaining slots if out_dim > model_dim
  for (int i = copy_dim; i < out_dim; i++) {
    out[i] = 0.0f;
  }

  printf("[Numenor][emb] Wrote %d floats (model_dim=%d)\n", copy_dim, model_dim);
  return copy_dim;
}

// Returns the embedding dimension of the currently-loaded model.
// Returns 0 if no model is loaded or the embedding context is absent.
int llm_get_embedding_dim(void) {
  if (!g_model || !g_emb_ctx) return 0;
  return llama_model_n_embd(g_model);
}
