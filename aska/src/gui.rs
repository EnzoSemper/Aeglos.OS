/// Fallback tiling window manager for Aska.
///
/// Activated when a framebuffer device is available (e.g. QEMU ramfb).
/// In serial-only mode these types are unused; they define the layout
/// contract for a future graphical front-end.

/// A rectangular region on the display, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// Split horizontally into (left, right) with `pct`% going left.
    pub fn split_h(self, pct: u32) -> (Rect, Rect) {
        let lw = self.w * pct / 100;
        let rw = self.w - lw;
        (
            Rect::new(self.x,      self.y, lw, self.h),
            Rect::new(self.x + lw, self.y, rw, self.h),
        )
    }

    /// Split vertically into (top, bottom) with `pct`% going to the top.
    pub fn split_v(self, pct: u32) -> (Rect, Rect) {
        let th = self.h * pct / 100;
        let bh = self.h - th;
        (
            Rect::new(self.x, self.y,      self.w, th),
            Rect::new(self.x, self.y + th, self.w, bh),
        )
    }
}

/// Role of a tile — determines how the shell renders its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileRole {
    /// Primary terminal pane (text input/output).
    Terminal,
    /// AI response / chat history view.
    AiPane,
    /// Semantic memory browser.
    MemPane,
    /// File browser.
    FilePane,
}

/// A single tile in the tiling layout.
#[derive(Debug, Clone, Copy)]
pub struct Tile {
    pub id:      u8,
    pub role:    TileRole,
    pub bounds:  Rect,
    pub focused: bool,
}

/// Simple tiling layout — up to 8 tiles.
pub struct TilingLayout {
    pub tiles:    [Option<Tile>; 8],
    pub focus_id: u8,
}

impl TilingLayout {
    pub const fn new() -> Self {
        Self { tiles: [None; 8], focus_id: 0 }
    }

    /// Add a tile with the given role and bounds.
    /// Returns the tile's id, or `None` if the layout is full.
    pub fn add_tile(&mut self, role: TileRole, bounds: Rect) -> Option<u8> {
        for (i, slot) in self.tiles.iter_mut().enumerate() {
            if slot.is_none() {
                let id = i as u8;
                *slot = Some(Tile { id, role, bounds, focused: i == 0 });
                return Some(id);
            }
        }
        None
    }

    /// Focus the tile with the given id.
    pub fn focus(&mut self, id: u8) {
        self.focus_id = id;
        for slot in &mut self.tiles {
            if let Some(t) = slot {
                t.focused = t.id == id;
            }
        }
    }

    /// Return a reference to the currently focused tile.
    pub fn focused_tile(&self) -> Option<&Tile> {
        self.tiles
            .iter()
            .filter_map(|s| s.as_ref())
            .find(|t| t.id == self.focus_id)
    }

    /// Standard two-pane layout: terminal left (60%), AI pane right (40%).
    pub fn default_two_pane(screen: Rect) -> Self {
        let mut layout = Self::new();
        let (left, right) = screen.split_h(60);
        layout.add_tile(TileRole::Terminal, left);
        layout.add_tile(TileRole::AiPane,   right);
        layout
    }

    /// Three-pane layout: terminal top-left, AI top-right, mem bottom.
    pub fn default_three_pane(screen: Rect) -> Self {
        let mut layout = Self::new();
        let (top, bottom) = screen.split_v(70);
        let (tl, tr)      = top.split_h(60);
        layout.add_tile(TileRole::Terminal, tl);
        layout.add_tile(TileRole::AiPane,   tr);
        layout.add_tile(TileRole::MemPane,  bottom);
        layout
    }
}
