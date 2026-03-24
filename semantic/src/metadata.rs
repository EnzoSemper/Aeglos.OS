
/// Fixed-size metadata structure for on-disk storage.
/// Total size should align well with blocks or be small enough to pack.
/// Current design: 256 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Metadata {
    pub created: u64,
    pub accessed: u64,
    pub content_type: u32, // 0=Binary, 1=Text, 2=Image, etc.
    pub parent: [u8; 32],  // Hash of parent/previous version (0 if none)
    pub flags: u32,
    pub tags: [u8; 128],   // Simple null-terminated string or list of fixed-width tags?
                           // For now: raw bytes for tags (e.g. "tag1,tag2\0")
    pub _padding: [u8; 72], // Pad to 256 bytes
}

impl Metadata {
    pub fn new(content_type: u32) -> Self {
        Self {
            created: 0, // No real time yet, maybe passed in?
            accessed: 0,
            content_type,
            parent: [0; 32],
            flags: 0,
            tags: [0; 128],
            _padding: [0; 72],
        }
    }
}
