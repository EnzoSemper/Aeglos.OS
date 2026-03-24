/// Message format for IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Message {
    pub sender: usize,      // TID of sender
    pub data: [u8; 32],      // Fixed payload
}

/// A fixed-size mailbox (circular buffer).
/// Capacity: 8 messages.
const MAILBOX_CAPACITY: usize = 8;

pub struct Mailbox {
    buffer: [Option<Message>; MAILBOX_CAPACITY],
    head: usize, // Read index
    tail: usize, // Write index
    count: usize,
}

impl Mailbox {
    pub const fn new() -> Self {
        Self {
            buffer: [None; MAILBOX_CAPACITY],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    /// Push a message. Returns Err if full.
    pub fn push(&mut self, msg: Message) -> Result<(), ()> {
        if self.count >= MAILBOX_CAPACITY {
            return Err(());
        }
        self.buffer[self.tail] = Some(msg);
        self.tail = (self.tail + 1) % MAILBOX_CAPACITY;
        self.count += 1;
        Ok(())
    }

    /// Pop a message. Returns None if empty.
    pub fn pop(&mut self) -> Option<Message> {
        if self.count == 0 {
            return None;
        }
        let msg = self.buffer[self.head].take();
        self.head = (self.head + 1) % MAILBOX_CAPACITY;
        self.count -= 1;
        msg
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn is_full(&self) -> bool {
        self.count >= MAILBOX_CAPACITY
    }
}
