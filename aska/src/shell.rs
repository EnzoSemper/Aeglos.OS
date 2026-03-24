/// Shell session state — tracks command history and working directory.

/// Maximum number of history entries retained (circular buffer).
const HISTORY_CAP: usize = 16;
/// Maximum byte length of a single history entry.
const HISTORY_LEN: usize = 128;
/// Maximum byte length of a stored working directory path.
const CWD_MAX: usize = 128;

/// Persistent state for one Aska shell session.
pub struct ShellState {
    /// Circular ring of past commands.
    history:       [[u8; HISTORY_LEN]; HISTORY_CAP],
    history_sizes: [usize; HISTORY_CAP],
    /// Next write index (wraps modulo HISTORY_CAP).
    head: usize,
    /// Total commands entered this session (saturates at usize::MAX).
    pub cmd_count: usize,
    /// Current working directory.
    cwd:     [u8; CWD_MAX],
    cwd_len: usize,
}

impl ShellState {
    /// Construct a new session with CWD = `/`.
    pub const fn new() -> Self {
        let mut cwd = [0u8; CWD_MAX];
        cwd[0] = b'/';
        Self {
            history:       [[0u8; HISTORY_LEN]; HISTORY_CAP],
            history_sizes: [0usize; HISTORY_CAP],
            head:          0,
            cmd_count:     0,
            cwd,
            cwd_len:       1,
        }
    }

    /// Record a command in the history ring. Truncates to `HISTORY_LEN`.
    pub fn push_history(&mut self, cmd: &[u8]) {
        let slot = self.head % HISTORY_CAP;
        let len = cmd.len().min(HISTORY_LEN);
        self.history[slot][..len].copy_from_slice(&cmd[..len]);
        self.history_sizes[slot] = len;
        self.head = self.head.wrapping_add(1);
        self.cmd_count = self.cmd_count.saturating_add(1);
    }

    /// Return a history entry by recency index (0 = most recent).
    /// Returns `None` if `idx` is out of range.
    pub fn history_entry(&self, idx: usize) -> Option<&[u8]> {
        let filled = self.cmd_count.min(HISTORY_CAP);
        if idx >= filled {
            return None;
        }
        // Most-recent entry is at head-1, oldest at head-filled.
        let slot = self.head.wrapping_sub(1 + idx) % HISTORY_CAP;
        Some(&self.history[slot][..self.history_sizes[slot]])
    }

    /// Return the current working directory as a `&str`.
    pub fn cwd(&self) -> &str {
        core::str::from_utf8(&self.cwd[..self.cwd_len]).unwrap_or("/")
    }

    /// Update the current working directory (truncated to `CWD_MAX - 1`).
    pub fn set_cwd(&mut self, path: &str) {
        let bytes = path.as_bytes();
        let len = bytes.len().min(CWD_MAX - 1);
        self.cwd[..len].copy_from_slice(&bytes[..len]);
        self.cwd_len = len;
    }
}
