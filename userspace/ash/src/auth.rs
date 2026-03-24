//! Aeglos OS user authentication.
//!
//! Users are defined at compile time. In production, this would be
//! extended to read from a FAT32 /users file, but static users are
//! sufficient for the current single-board deployment target.

pub const MAX_USERS: usize = 4;

pub struct User {
    pub username: &'static str,
    /// SHA-256 of the PIN/password (hex string, lower case).
    /// Default: "aeglos" → sha256 = "c7e4fe0a6c73ba6e4c14b7fe2af48b2..."
    /// For simplicity, store the raw PIN as a compile-time str.
    /// In production replace with a proper hash.
    pub pin: &'static str,
    /// Display name shown on the login screen.
    pub display_name: &'static str,
    /// Capability set for this user's processes.
    pub caps: u64,
}

/// CAP_ALL — full privileges
pub const CAP_ALL: u64 = !0u64;
/// CAP_USER_DEFAULT — standard user privileges
pub const CAP_USER_DEFAULT: u64 = (1 << 0) | (1 << 1) | (1 << 3) | (1 << 4) | (1 << 5);

pub static USERS: [User; 2] = [
    User { username: "root",   pin: "0000",   display_name: "Root",        caps: CAP_ALL },
    User { username: "aeglos", pin: "aeglos", display_name: "Aeglos User", caps: CAP_USER_DEFAULT },
];

/// Attempt to authenticate. Returns Some(user_index) on success.
pub fn authenticate(username: &str, pin: &str) -> Option<usize> {
    for (i, u) in USERS.iter().enumerate() {
        if u.username == username && u.pin == pin {
            return Some(i);
        }
    }
    None
}
