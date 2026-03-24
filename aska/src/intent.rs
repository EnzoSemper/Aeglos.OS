/// Intent parser — decomposes natural language input into typed intents.
///
/// Every built-in command is expressed as a typed `Intent` variant so that
/// the dispatch layer can be a single `match` expression with no ad-hoc
/// string scanning.  Unrecognised input falls through to `AiQuery`.

/// A parsed user intent.
#[derive(Debug, PartialEq)]
pub enum Intent<'a> {
    // ── Shell lifecycle ──────────────────────────────────────────────────────
    Help,
    Exit,

    // ── Filesystem ──────────────────────────────────────────────────────────
    /// List directory contents. Arg: target path ("" = root).
    ListDir(&'a str),
    /// Print a file's contents to the terminal.
    CatFile(&'a str),
    /// Launch an ELF binary. (path, wait_for_exit)
    Exec(&'a str, bool),
    /// Write text content to a file. (path, content)
    Save(&'a str, &'a str),

    // ── Semantic memory ──────────────────────────────────────────────────────
    Mem(MemOp<'a>),

    // ── Network ─────────────────────────────────────────────────────────────
    /// Show network information (IP address).
    NetInfo,
    /// ICMP ping. Arg: hostname or dotted-decimal IP.
    Ping(&'a str),
    /// DNS A-record lookup. Arg: hostname.
    Dns(&'a str),
    /// HTTP GET and print response body. Arg: URL.
    Fetch(&'a str),
    /// HTTP GET then save response to file. (url, filepath)
    Download(&'a str, &'a str),
    /// HTTPS POST. (url, body)
    Post(&'a str, &'a str),
    /// Raw TCP connect, send optional message, print response. (host, port, msg)
    Nc(&'a str, u16, &'a str),
    /// Bind a local port and accept one incoming TCP connection.
    TcpListen(u16),

    // ── System ──────────────────────────────────────────────────────────────
    /// Clear conversation history (start a new AI session).
    ResetHistory,
    /// Run the Aeglos installer.
    Install,
    /// Load and run a WASM binary from the filesystem. Arg: path (e.g. "/app.wasm").
    WasmRun(&'a str),
    /// Capability management (grant, revoke, query, list).
    Cap(CapOp<'a>),

    // ── Environment variables ─────────────────────────────────────────────────
    /// List all env vars.
    EnvList,
    /// Set an env var. (key, value)
    EnvSet(&'a str, &'a str),
    /// Unset an env var. Arg: key.
    EnvUnset(&'a str),

    // ── TTS ──────────────────────────────────────────────────────────────────
    /// Speak text via HDA TTS. Arg: text to synthesize.
    Speak(&'a str),

    // ── Background jobs ──────────────────────────────────────────────────────
    /// Launch a command in background (trailing &). Arg: command line.
    BgExec(&'a str),
    /// List background jobs.
    Jobs,
    /// Wait for a background job by TID to finish.
    WaitJob(usize),

    // ── Pipe consumers (read from stdin / pipe buffer) ────────────────────────
    /// Filter stdin lines containing `pattern`.
    Grep(&'a str),
    /// Print first N lines of stdin (default 10).
    Head(usize),
    /// Count lines, words, bytes in stdin.
    Wc,

    // ── AI fallthrough ───────────────────────────────────────────────────────
    /// Forward to Numenor for natural-language inference.
    AiQuery(&'a str),
    /// Empty / whitespace-only input — no action needed.
    Empty,
}

/// Sub-operations for `Intent::Mem`.
#[derive(Debug, PartialEq)]
pub enum MemOp<'a> {
    Store(&'a str),
    Query(&'a str),
    Search(&'a str),
    Help,
}

/// Sub-operations for `Intent::Cap`.
#[derive(Debug, PartialEq)]
pub enum CapOp<'a> {
    /// List all running tasks with their capability bitmasks.
    List,
    /// Show capabilities for a specific TID.
    Show(&'a str),
    /// Grant a named capability to a task. (tid_str, cap_name)
    Grant(&'a str, &'a str),
    /// Revoke a named capability from a task. (tid_str, cap_name)
    Revoke(&'a str, &'a str),
    Help,
}

/// Parse a decimal port number from a string slice; returns 0 on failure.
fn parse_port(s: &str) -> u16 {
    let mut n = 0u32;
    for b in s.bytes() {
        if b < b'0' || b > b'9' { return 0; }
        n = n * 10 + (b - b'0') as u32;
        if n > 65535 { return 0; }
    }
    n as u16
}

/// Parse a raw input line into a typed `Intent`.
///
/// Recognised built-ins (case-sensitive first word):
///
/// Shell:       `help`, `?`, `exit`, `quit`, `q`
/// Filesystem:  `ls`, `dir`, `cat`, `type`, `exec`, `execw`, `save`
/// Network:     `net`, `ifconfig`, `ping`, `dns`, `nslookup`, `fetch`,
///              `curl`, `wget`, `download`
/// Memory:      `mem`, `memory`
/// System:      `install`
///
/// Everything else becomes `AiQuery`.
pub fn parse(input: &str) -> Intent<'_> {
    let input = input.trim();
    if input.is_empty() {
        return Intent::Empty;
    }

    let (cmd, rest) = split_first_word(input);
    let rest = rest.trim();

    match cmd {
        // ── Shell lifecycle ──────────────────────────────────────────────────
        "help" | "?"                        => Intent::Help,
        "exit" | "quit" | "q"              => Intent::Exit,

        // ── Filesystem ──────────────────────────────────────────────────────
        "ls" | "dir"                        => Intent::ListDir(rest),
        "cat" | "type"                      => Intent::CatFile(rest),
        "exec"                              => Intent::Exec(rest, false),
        "execw"                             => Intent::Exec(rest, true),
        "save" => {
            let (path, content) = split_first_word(rest);
            Intent::Save(path, content.trim())
        }

        // ── Semantic memory ──────────────────────────────────────────────────
        "mem" | "memory" => {
            let (subcmd, args) = split_first_word(rest);
            let args = args.trim();
            match subcmd {
                "store" | "put" | "save" | "remember" => Intent::Mem(MemOp::Store(args)),
                "query" | "find" | "get" | "recall"   => Intent::Mem(MemOp::Query(args)),
                "search" | "tag"                       => Intent::Mem(MemOp::Search(args)),
                _                                      => Intent::Mem(MemOp::Help),
            }
        }

        // ── Network ─────────────────────────────────────────────────────────
        "net" | "ifconfig"                  => Intent::NetInfo,
        "ping"                              => Intent::Ping(rest),
        "dns" | "nslookup"                  => Intent::Dns(rest),
        "fetch" | "curl" | "wget"           => Intent::Fetch(rest),
        "download" => {
            let (url, filepath) = split_first_word(rest);
            Intent::Download(url, filepath.trim())
        }
        "post" => {
            let (url, body) = split_first_word(rest);
            Intent::Post(url, body.trim())
        }
        "nc" | "telnet" | "connect" => {
            // nc <host> <port> [message...]
            let (host, rest2) = split_first_word(rest);
            let (port_str, msg) = split_first_word(rest2.trim());
            let port = parse_port(port_str);
            Intent::Nc(host, port, msg.trim())
        }
        "listen" | "tcp_listen" => {
            // listen <port>
            let port = parse_port(rest.trim());
            Intent::TcpListen(port)
        }

        // ── System ──────────────────────────────────────────────────────────
        "reset" | "new" | "clear" if rest.trim().is_empty() || rest.trim() == "history"
                                            => Intent::ResetHistory,
        "install"                           => Intent::Install,
        "wasm" | "wasmrun"                  => Intent::WasmRun(rest),
        "cap" | "caps" | "capctl" => {
            let (subcmd, args2) = split_first_word(rest);
            let args2 = args2.trim();
            match subcmd {
                "list" | "ls" | ""          => Intent::Cap(CapOp::List),
                "show" | "query" | "get"    => Intent::Cap(CapOp::Show(args2)),
                "grant" | "add" | "give" => {
                    let (tid_str, cap_str) = split_first_word(args2);
                    Intent::Cap(CapOp::Grant(tid_str, cap_str.trim()))
                }
                "revoke" | "remove" | "deny" | "take" => {
                    let (tid_str, cap_str) = split_first_word(args2);
                    Intent::Cap(CapOp::Revoke(tid_str, cap_str.trim()))
                }
                _                           => Intent::Cap(CapOp::Help),
            }
        }

        // ── Environment variables ─────────────────────────────────────────────
        "env"    => Intent::EnvList,
        "export" | "set" => {
            if rest.trim().is_empty() {
                Intent::EnvList
            } else {
                // "export KEY=VALUE" or "export KEY VALUE"
                if let Some(eq_pos) = rest.find('=') {
                    let key = rest[..eq_pos].trim();
                    let val = rest[eq_pos + 1..].trim();
                    Intent::EnvSet(key, val)
                } else {
                    let (key, val) = split_first_word(rest.trim());
                    Intent::EnvSet(key, val.trim())
                }
            }
        }
        "unset" => Intent::EnvUnset(rest.trim()),

        // ── TTS ─────────────────────────────────────────────────────────────
        "speak" | "say" | "tts" => Intent::Speak(rest),

        // ── Background jobs ──────────────────────────────────────────────────
        "jobs"                   => Intent::Jobs,
        "wait" if !rest.is_empty() => {
            let mut n = 0usize;
            for b in rest.trim().bytes() {
                if b < b'0' || b > b'9' { break; }
                n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
            }
            Intent::WaitJob(n)
        }

        // ── Pipe consumers ───────────────────────────────────────────────────
        "grep" | "fgrep" => Intent::Grep(rest),
        "head" => {
            let (tok, _) = split_first_word(rest);
            let tok = tok.trim_start_matches("-n");
            let mut n = 0usize;
            for b in tok.trim().bytes() {
                if b < b'0' || b > b'9' { break; }
                n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
            }
            Intent::Head(if n == 0 { 10 } else { n })
        }
        "wc" => Intent::Wc,

        // ── AI fallthrough ───────────────────────────────────────────────────
        _                                   => Intent::AiQuery(input),
    }
}

/// Split a string at the first whitespace boundary.
/// Returns `(first_word, remainder)` — remainder may be empty.
pub fn split_first_word(s: &str) -> (&str, &str) {
    match s.find(|c: char| c.is_ascii_whitespace()) {
        Some(i) => (&s[..i], &s[i..]),
        None    => (s, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        assert_eq!(parse(""), Intent::Empty);
        assert_eq!(parse("   "), Intent::Empty);
    }

    #[test]
    fn test_help() {
        assert_eq!(parse("help"), Intent::Help);
        assert_eq!(parse("?"), Intent::Help);
    }

    #[test]
    fn test_ls() {
        assert_eq!(parse("ls"), Intent::ListDir(""));
        assert_eq!(parse("ls /boot"), Intent::ListDir("/boot"));
        assert_eq!(parse("dir /"), Intent::ListDir("/"));
    }

    #[test]
    fn test_exec() {
        assert_eq!(parse("exec /ash"), Intent::Exec("/ash", false));
        assert_eq!(parse("execw /ash"), Intent::Exec("/ash", true));
    }

    #[test]
    fn test_save() {
        assert_eq!(parse("save /notes.txt hello world"), Intent::Save("/notes.txt", "hello world"));
        assert_eq!(parse("save /empty"), Intent::Save("/empty", ""));
    }

    #[test]
    fn test_mem_store() {
        assert_eq!(parse("mem store hello world"), Intent::Mem(MemOp::Store("hello world")));
        assert_eq!(parse("memory remember the answer is 42"), Intent::Mem(MemOp::Store("the answer is 42")));
    }

    #[test]
    fn test_mem_query() {
        assert_eq!(parse("mem query kernel"), Intent::Mem(MemOp::Query("kernel")));
        assert_eq!(parse("mem find boot"), Intent::Mem(MemOp::Query("boot")));
    }

    #[test]
    fn test_network_intents() {
        assert_eq!(parse("net"), Intent::NetInfo);
        assert_eq!(parse("ifconfig"), Intent::NetInfo);
        assert_eq!(parse("ping 8.8.8.8"), Intent::Ping("8.8.8.8"));
        assert_eq!(parse("ping google.com"), Intent::Ping("google.com"));
        assert_eq!(parse("dns example.com"), Intent::Dns("example.com"));
        assert_eq!(parse("nslookup example.com"), Intent::Dns("example.com"));
        assert_eq!(parse("fetch http://10.0.2.2/"), Intent::Fetch("http://10.0.2.2/"));
        assert_eq!(parse("curl http://10.0.2.2/"), Intent::Fetch("http://10.0.2.2/"));
        assert_eq!(parse("wget http://10.0.2.2/file"), Intent::Fetch("http://10.0.2.2/file"));
    }

    #[test]
    fn test_download() {
        assert_eq!(
            parse("download http://10.0.2.2/f.bin /f.bin"),
            Intent::Download("http://10.0.2.2/f.bin", "/f.bin"),
        );
    }

    #[test]
    fn test_install() {
        assert_eq!(parse("install"), Intent::Install);
    }

    #[test]
    fn test_wasm() {
        assert_eq!(parse("wasm /app.wasm"), Intent::WasmRun("/app.wasm"));
        assert_eq!(parse("wasmrun /hello.wasm"), Intent::WasmRun("/hello.wasm"));
        assert_eq!(parse("wasm"), Intent::WasmRun(""));
    }

    #[test]
    fn test_ai_fallthrough() {
        assert_eq!(parse("what is the weather?"), Intent::AiQuery("what is the weather?"));
        assert_eq!(parse("UNKNOWN_CMD foo"), Intent::AiQuery("UNKNOWN_CMD foo"));
    }
}
