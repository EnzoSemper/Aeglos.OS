use crate::ui;

// ───────────────────────────── THEMES ────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg:         u32,  // desktop background
    pub panel_bg:   u32,  // window interior
    pub bar_bg:     u32,  // top/bottom bar background
    pub border:     u32,  // inactive window border
    pub border_hi:  u32,  // active window border
    pub fg:         u32,  // primary text
    pub fg2:        u32,  // secondary text
    pub fg_dim:     u32,  // placeholder / dim text
    pub tab_on_bg:  u32,  // active tab fill
    pub tab_on_fg:  u32,  // active tab text
    pub tab_off_fg: u32,  // inactive tab text
    pub dot:        u32,  // background grid dot
}

pub const DARK: Theme = Theme {
    bg:         0x000000,   // pure black
    panel_bg:   0x080e08,   // deep dark-green panel
    bar_bg:     0x040804,   // near-black bar
    border:     0x1a3a1a,   // dim green border
    border_hi:  0x00ff41,   // neon green signature accent
    fg:         0xd4ffd4,   // soft mint white (easier on eyes than pure white)
    fg2:        0x74bb74,   // medium green
    fg_dim:     0x2d4d2d,   // dim green
    tab_on_bg:  0x00ff41,
    tab_on_fg:  0x000000,
    tab_off_fg: 0x4a7a4a,
    dot:        0x050e05,
};

pub const LIGHT: Theme = Theme {
    bg:         0xe8f4f8,
    panel_bg:   0xf8fdff,
    bar_bg:     0xdaeef5,
    border:     0x6ab8cc,
    border_hi:  0x1a8aaa,
    fg:         0x0a2030,
    fg2:        0x1e5068,
    fg_dim:     0x78a8bc,
    tab_on_bg:  0x1a8aaa,
    tab_on_fg:  0xf8fdff,
    tab_off_fg: 0x0a2030,
    dot:        0xbcd8e4,
};

// ───────────────────────────── LAYOUT ────────────────────────────────────────

const TOPBAR_H:   usize = 38;
const TASKBAR_H:  usize = 40;
const TITLEBAR_H: usize = 20;
const LINE_H:     usize = 18;

// ───────────────────────────── SYSCALLS ──────────────────────────────────────

const SYS_SEND:    usize = 1;
const SYS_RECV:    usize = 2;
const SYS_YIELD:   usize = 3;
const SYS_OPEN:      usize = 16;
const SYS_READ_FD:   usize = 17;
const SYS_WRITE_FD:  usize = 18;
const SYS_CLOSE:     usize = 19;
const SYS_READDIR:   usize = 20;
const SYS_CREATE:    usize = 38;
const SYS_EXEC:      usize = 13;
const CAP_USER_DEFAULT: u64 = 0b0111_0111; // SEND|RECV|LOG|MEM|AI|NET

const SEM_TID:     usize = 2;
const OP_STORE:    u64   = 100;
const OP_RETRIEVE: u64   = 101;
const OP_QUERY:    u64   = 104;

#[inline(always)]
unsafe fn sc1(n: usize, a0: usize) -> usize {
    let r: usize;
    core::arch::asm!("svc #0", in("x8") n, in("x0") a0,
        lateout("x0") r, clobber_abi("system"));
    r
}
#[inline(always)]
unsafe fn sc2(n: usize, a0: usize, a1: usize) -> usize {
    let r: usize;
    core::arch::asm!("svc #0", in("x8") n, in("x0") a0, in("x1") a1,
        lateout("x0") r, clobber_abi("system"));
    r
}
#[inline(always)]
unsafe fn sc3(n: usize, a0: usize, a1: usize, a2: usize) -> usize {
    let r: usize;
    core::arch::asm!("svc #0", in("x8") n, in("x0") a0, in("x1") a1, in("x2") a2,
        lateout("x0") r, clobber_abi("system"));
    r
}

// ───────────────────────────── DATA TYPES ────────────────────────────────────

const NAME_MAX: usize = 255;

#[repr(C)]
#[derive(Copy, Clone)]
struct DirEntry {
    name:   [u8; NAME_MAX + 1],
    is_dir: u8,
    _pad:   [u8; 2],
    size:   u32,
}
impl DirEntry {
    const fn zeroed() -> Self {
        Self { name: [0u8; NAME_MAX + 1], is_dir: 0, _pad: [0; 2], size: 0 }
    }
    fn name_str(&self) -> &str {
        let nlen = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
        core::str::from_utf8(&self.name[..nlen]).unwrap_or("")
    }
}

#[repr(C)]
struct Message { sender: usize, data: [u8; 32] }

// ───────────────────────────── PANELS ────────────────────────────────────────

const PANEL_TERMINAL: usize = 0;
const PANEL_FS:       usize = 1;
const PANEL_AI:       usize = 2;
const PANEL_BROWSER:  usize = 3;

const TERM_LINES: usize = 24;
const AI_LINES:   usize = 10;

const FS_MAX_ENTRIES:    usize = 24;
const BROWSER_MAX_CONTENT: usize = 4096;

// ───────────────────────────── WINDOW ────────────────────────────────────────

pub struct Window {
    pub x: usize, pub y: usize, pub w: usize, pub h: usize,
    pub max_w: usize, pub max_h: usize,
    pub title: &'static str,
    pub active: bool, pub visible: bool,
    pub minimized: bool, pub maximized: bool,
    pub id: usize,
}

// ───────────────────────────── DESKTOP ───────────────────────────────────────

pub struct Desktop {
    windows:       [Window; 4],
    /// Draw order: draw_order[0] = bottom-most window index, [3] = top-most.
    draw_order:    [usize; 4],
    mouse_x:       usize,
    mouse_y:       usize,
    mouse_down:    bool,
    dragging_win:  Option<usize>,
    drag_offset_x: isize,
    drag_offset_y: isize,
    resizing_win:      Option<usize>,
    resize_origin_x:   isize,
    resize_origin_y:   isize,
    resize_origin_w:   usize,
    resize_origin_h:   usize,

    // Terminal panel
    term_buffer:   [u8; 128],
    term_len:      usize,
    term_history:  [([u8; 128], usize); TERM_LINES],

    // AI panel chat log: (text, len, is_numenor_response)
    ai_history:    [([u8; 256], usize, bool); AI_LINES],

    shift_down:    bool,

    // LLM plumbing (read/written by main.rs)
    pub sys_send_msg:        Option<&'static [u8]>,
    pub llm_response_active: bool,
    pub llm_buffer:          [u8; 512],
    pub llm_len:             usize,

    // UI state
    light_mode:    bool,
    active_panel:  usize,

    // Config
    tz_offset:     i8,
    tz_dropdown_open: bool,
    time_24hr:     bool,
    pub sys_ip:    [u8; 16],
    pub sys_ip_len: usize,

    /// Unix epoch from SYS_GET_RTC, updated each frame by main.rs.
    pub epoch:     u32,
    /// System stats from SYS_GET_STATS: [free_mb, total_mb, cpu_pct, task_count].
    pub stats:     [u32; 4],

    // FS panel state
    fs_path:       [u8; 64],
    fs_path_len:   usize,
    fs_entries:    [DirEntry; FS_MAX_ENTRIES],
    fs_count:      usize,
    fs_selected:   usize,
    fs_scroll:     usize,
    fs_dirty:      bool,

    // Browser state
    browser_url:         [u8; 128],
    browser_url_len:     usize,
    browser_content:     [u8; BROWSER_MAX_CONTENT],
    browser_content_len: usize,
    browser_loading:     bool,
    browser_url_editing: bool,
    browser_save_msg:    [u8; 64],
    browser_save_msg_len: usize,

    // Desktop icons (root dir)
    desktop_icons:        [DirEntry; 16],
    desktop_icon_count:   usize,
    desktop_icons_loaded: bool,

    // Login screen state
    pub logged_in:        bool,
    login_username:       [u8; 32],
    login_username_len:   usize,
    login_pin:            [u8; 32],  // stored as bytes, displayed as ****
    login_pin_len:        usize,
    login_field:          u8,        // 0=username, 1=pin
    login_error:          bool,
    login_error_timer:    u32,

    // Launcher toast (6.2)
    launcher_msg:         [u8; 64],
    launcher_msg_len:     usize,
    launcher_msg_timer:   u32,
    last_launch_tick:     u32,

    // Desktop icon drag-and-drop (6.5)
    icon_pressed:         Option<usize>,
    icon_press_x:         usize,
    icon_press_y:         usize,
    dragging_icon:        Option<usize>,
    drag_icon_x:          usize,
    drag_icon_y:          usize,

    // Clipboard (6.6)
    ctrl_down:            bool,
    clipboard:            [u8; 256],
    clipboard_len:        usize,
}

// ELF_BUF removed — kernel SYS_EXEC takes a path, reads from FAT32 itself.

/// Decompose Unix epoch seconds into (year, month 1-12, day 1-31).
/// Uses Howard Hinnant's civil_from_days algorithm.
fn epoch_to_ymd(secs: u32) -> (u16, u8, u8) {
    let z = (secs / 86400) as i32 + 719468;
    let era: i32 = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp  = (5 * doy + 2) / 153;
    let d   = doy - (153 * mp + 2) / 5 + 1;
    let m   = if mp < 10 { mp + 3 } else { mp - 9 };
    let y   = yoe as i32 + era * 400 + if m <= 2 { 1 } else { 0 };
    (y as u16, m as u8, d as u8)
}

fn sys_readdir_desktop(path: &str, entries: &mut [DirEntry]) -> isize {
    unsafe {
        sc3(SYS_READDIR, path.as_ptr() as usize, path.len(), entries.as_mut_ptr() as usize) as isize
    }
}

impl Desktop {
    pub fn new() -> Self {
        let mut d = Self {
            windows: [
                Window { x: 40,  y: 55,  w: 516, h: 380,
                         max_w: 516, max_h: 380,
                         title: "root@aeglos:~ _", active: true,  visible: true,
                         minimized: false, maximized: false, id: 0 },
                Window { x: 628, y: 118, w: 450, h: 358,
                         max_w: 450, max_h: 358,
                         title: "AEGLOS_FS // ASSETS", active: false, visible: true,
                         minimized: false, maximized: false, id: 1 },
                Window { x: 242, y: 388, w: 442, h: 268,
                         max_w: 442, max_h: 268,
                         title: "AI_LINK // NUMENOR", active: false, visible: true,
                         minimized: false, maximized: false, id: 2 },
                Window { x: 100, y: 60,  w: 760, h: 550,
                         max_w: 760, max_h: 550,
                         title: "BROWSER // AEGLOS_NET", active: false, visible: false,
                         minimized: false, maximized: false, id: 3 },
            ],
            draw_order: [0, 1, 2, 3],
            mouse_x: 640, mouse_y: 360, mouse_down: false,
            dragging_win: None, drag_offset_x: 0, drag_offset_y: 0,
            term_buffer: [0; 128], term_len: 0,
            term_history: [([0; 128], 0); TERM_LINES],
            ai_history:   [([0; 256], 0, false); AI_LINES],
            shift_down: false,
            sys_send_msg: None,
            llm_response_active: false,
            llm_buffer: [0; 512], llm_len: 0,
            light_mode: false,
            active_panel: PANEL_TERMINAL,
            tz_offset: 0,
            tz_dropdown_open: false,
            time_24hr: true,
            sys_ip: [b' '; 16],
            sys_ip_len: 0,
            epoch: 0,
            stats: [0; 4],
            resizing_win: None,
            resize_origin_x: 0,
            resize_origin_y: 0,
            resize_origin_w: 0,
            resize_origin_h: 0,
            fs_path: { let mut a = [0u8; 64]; a[0] = b'/'; a },
            fs_path_len: 1,
            fs_entries: [DirEntry::zeroed(); FS_MAX_ENTRIES],
            fs_count: 0,
            fs_selected: 0,
            fs_scroll: 0,
            fs_dirty: true,
            browser_url: [0u8; 128],
            browser_url_len: 0,
            browser_content: [0u8; BROWSER_MAX_CONTENT],
            browser_content_len: 0,
            browser_loading: false,
            browser_url_editing: false,
            browser_save_msg: [0u8; 64],
            browser_save_msg_len: 0,
            desktop_icons:        [DirEntry::zeroed(); 16],
            desktop_icon_count:   0,
            desktop_icons_loaded: false,
            logged_in:            false,
            login_username:       [0; 32],
            login_username_len:   0,
            login_pin:            [0; 32],
            login_pin_len:        0,
            login_field:          0,
            login_error:          false,
            login_error_timer:    0,
            launcher_msg:         [0; 64],
            launcher_msg_len:     0,
            launcher_msg_timer:   0,
            last_launch_tick:     0,
            icon_pressed:         None,
            icon_press_x:         0,
            icon_press_y:         0,
            dragging_icon:        None,
            drag_icon_x:          0,
            drag_icon_y:          0,
            ctrl_down:            false,
            clipboard:            [0; 256],
            clipboard_len:        0,
        };

        // Boot log in terminal history
        let boot: &[&[u8]] = &[
            b"Initializing Aeglos OS Kernel... OK",
            b"Mounting core partitions... OK",
            b"",
            b"Welcome to AEGLOS OS.",
            b"",
            b"> ESTABLISHING CONNECTION TO SYSTEM...",
            b"ACCESS GRANTED.",
            b"",
            b"WELCOME TO AEGLOS OS.",
        ];
        for (i, msg) in boot.iter().enumerate() {
            let len = msg.len().min(128);
            d.term_history[i].0[..len].copy_from_slice(&msg[..len]);
            d.term_history[i].1 = len;
        }

        // AI panel welcome
        d.push_ai_line(b"System linked. Ready for natural language query.", true);
        d
    }

    // ── History helpers ───────────────────────────────────────────────────────

    fn push_term_history(&mut self, msg: &[u8]) {
        for i in 0..TERM_LINES - 1 { self.term_history[i] = self.term_history[i + 1]; }
        let len = msg.len().min(128);
        self.term_history[TERM_LINES - 1] = ([0; 128], 0);
        self.term_history[TERM_LINES - 1].0[..len].copy_from_slice(&msg[..len]);
        self.term_history[TERM_LINES - 1].1 = len;
    }

    fn push_ai_line(&mut self, msg: &[u8], is_response: bool) {
        for i in 0..AI_LINES - 1 { self.ai_history[i] = self.ai_history[i + 1]; }
        let len = msg.len().min(256);
        self.ai_history[AI_LINES - 1] = ([0; 256], 0, is_response);
        self.ai_history[AI_LINES - 1].0[..len].copy_from_slice(&msg[..len]);
        self.ai_history[AI_LINES - 1].1 = len;
    }

    /// Called by main.rs after llm_buffer is populated with the inference response.
    pub fn on_llm_response(&mut self) {
        let len = self.llm_len.min(512);
        if len > 0 {
            let mut tmp = [0u8; 512];
            tmp[..len].copy_from_slice(&self.llm_buffer[..len]);
            self.push_ai_line(&tmp[..len], true);
        }
        // Reset streaming buffer for next query
        self.llm_buffer = [0; 512];
        self.llm_len = 0;
    }

    /// Called each frame during streaming — triggers a redraw with partial text.
    /// The chat panel's draw code already reads `llm_buffer[..llm_len]` when
    /// `llm_response_active` is true, so no additional state is needed.
    pub fn on_llm_partial(&mut self) {
        // Nothing extra needed — draw() already shows partial buffer
        // when llm_response_active is true.  This hook exists for future use
        // (e.g. cursor blink, "generating..." indicator).
    }

    // ── Browser Save ─────────────────────────────────────────────────────────

    fn save_browser_content(&mut self) {
        if self.browser_content_len == 0 { return; }

        // Derive filename from URL: take last path segment, default to "download.txt"
        let url_str = core::str::from_utf8(&self.browser_url[..self.browser_url_len]).unwrap_or("");
        let filename = url_str.rsplit('/').find(|s| !s.is_empty()).unwrap_or("download.txt");
        // Cap filename at 32 chars
        let fname_len = filename.len().min(32);
        let fname = &filename[..fname_len];

        // Build path: "/" + filename (flat FAT32 root)
        let mut path_buf = [0u8; 36];
        path_buf[0] = b'/';
        path_buf[1..1 + fname_len].copy_from_slice(fname.as_bytes());
        let path_len = 1 + fname_len;
        let path = core::str::from_utf8(&path_buf[..path_len]).unwrap_or("/download.txt");

        let fd = unsafe { sc2(SYS_CREATE, path.as_ptr() as usize, path.len()) as isize };
        if fd < 0 {
            let msg = b"Save failed: cannot create file";
            let mlen = msg.len().min(64);
            self.browser_save_msg[..mlen].copy_from_slice(&msg[..mlen]);
            self.browser_save_msg_len = mlen;
            return;
        }

        let written = unsafe {
            sc3(SYS_WRITE_FD, fd as usize,
                self.browser_content.as_ptr() as usize,
                self.browser_content_len) as isize
        };
        unsafe { sc1(SYS_CLOSE, fd as usize); }

        // Build feedback message "Saved: /filename.txt"
        let prefix = b"Saved: /";
        let mut msg = [0u8; 64];
        let pl = prefix.len().min(64);
        msg[..pl].copy_from_slice(&prefix[..pl]);
        let fl = fname_len.min(64 - pl);
        msg[pl..pl + fl].copy_from_slice(&fname.as_bytes()[..fl]);
        if written < 0 {
            let err = b" (write err)";
            let el = err.len().min(64 - pl - fl);
            msg[pl + fl..pl + fl + el].copy_from_slice(&err[..el]);
            self.browser_save_msg_len = pl + fl + el;
        } else {
            self.browser_save_msg_len = pl + fl;
        }
        self.browser_save_msg[..self.browser_save_msg_len]
            .copy_from_slice(&msg[..self.browser_save_msg_len]);
    }

    // ── AI dispatch ───────────────────────────────────────────────────────────

    fn dispatch_ai(&mut self, query: &[u8]) {
        // Push user query line to AI chat log
        let qlen = query.len().min(256);
        let mut line = [0u8; 256];
        line[..qlen].copy_from_slice(&query[..qlen]);
        self.push_ai_line(&line[..qlen], false);

        self.llm_response_active = true;
        self.llm_buffer = [0; 512];
        self.llm_len = 0;

        unsafe {
            static mut AI_MSG: [u8; 128] = [0; 128];
            let len = query.len().min(128);
            AI_MSG[..len].copy_from_slice(&query[..len]);
            self.sys_send_msg = Some(&AI_MSG[..len]);
        }
    }

    // ── Panel switching ───────────────────────────────────────────────────────

    /// Move `win_idx` to the top of the draw stack (drawn last = visually on top).
    fn bring_to_front(&mut self, win_idx: usize) {
        let pos = self.draw_order.iter().position(|&i| i == win_idx);
        if let Some(p) = pos {
            // Shift every slot after p one step toward the front.
            for i in p..3 { self.draw_order[i] = self.draw_order[i + 1]; }
            self.draw_order[3] = win_idx;
        }
    }

    fn switch_panel(&mut self, panel: usize) {
        if panel != self.active_panel {
            // Clear shared input buffer to prevent stale text bleeding across panels
            self.term_len = 0;
        }
        self.active_panel = panel;
        for (i, w) in self.windows.iter_mut().enumerate() {
            w.active = i == panel;
            if i == panel { w.minimized = false; }
        }
    }

    // ── Network commands ──────────────────────────────────────────────────────

    fn cmd_ping(&mut self, target: &str) {
        let mut ip = [0u8; 4];
        let mut valid_ip = true;
        let mut i = 0;
        let mut curr = 0u32;
        let mut in_num = false;
        for b in target.bytes() {
            if b == b'.' {
                if !in_num || i >= 4 { valid_ip = false; break; }
                ip[i] = curr as u8; i += 1; curr = 0; in_num = false;
            } else if b >= b'0' && b <= b'9' {
                curr = curr * 10 + (b - b'0') as u32;
                if curr > 255 { valid_ip = false; break; }
                in_num = true;
            } else {
                valid_ip = false; break;
            }
        }
        if valid_ip && in_num && i == 3 {
            ip[3] = curr as u8;
        } else {
            valid_ip = false;
        }

        let mut ip_packed = 0u32;
        if valid_ip {
            ip_packed = ((ip[0] as u32) << 24) | ((ip[1] as u32) << 16) | ((ip[2] as u32) << 8) | (ip[3] as u32);
        } else {
            let res = crate::sys_dns_resolve(target, &mut ip_packed);
            if res < 0 {
                self.push_term_history(b"ping: unknown host");
                return;
            }
            ip[0] = (ip_packed >> 24) as u8;
            ip[1] = (ip_packed >> 16) as u8;
            ip[2] = (ip_packed >> 8) as u8;
            ip[3] = ip_packed as u8;
        }

        let mut msg = [0u8; 128];
        let mut idx = 0;
        macro_rules! append { ($s:expr) => { for b in $s.bytes() { if idx < 128 { msg[idx] = b; idx += 1; } } } }
        macro_rules! app_num { ($n:expr) => {
            let n = $n as u32;
            if n >= 1000 { append!("1000+"); }
            else if n >= 100 { msg[idx] = b'0' + (n/100) as u8; idx += 1; msg[idx] = b'0' + ((n/10)%10) as u8; idx += 1; msg[idx] = b'0' + (n%10) as u8; idx += 1; }
            else if n >= 10 { msg[idx] = b'0' + (n/10) as u8; idx += 1; msg[idx] = b'0' + (n%10) as u8; idx += 1; }
            else { msg[idx] = b'0' + (n%10) as u8; idx += 1; }
        } }

        append!("PING "); append!(target); append!(" 32 bytes of data.");
        self.push_term_history(&msg[..idx]);

        let ip_packed = ((ip[0] as u32) << 24) | ((ip[1] as u32) << 16) | ((ip[2] as u32) << 8) | (ip[3] as u32);

        let mut timeouts = 0;
        for seq in 1..=4 {
            let rtt = crate::sys_ping(ip_packed, 1000);
            idx = 0;
            if rtt < 0 {
                append!("Request timeout for icmp_seq "); app_num!(seq);
                timeouts += 1;
            } else {
                append!("40 bytes from "); append!(target);
                append!(": icmp_seq="); app_num!(seq);
                append!(" ttl=64 time="); app_num!(rtt as u32); append!(" ms");
            }
            self.push_term_history(&msg[..idx]);
        }
        idx = 0;
        append!("--- "); append!(target); append!(" ping statistics ---");
        self.push_term_history(&msg[..idx]);
        idx = 0;
        append!("4 packets transmitted, "); app_num!(4 - timeouts); append!(" packets received");
        self.push_term_history(&msg[..idx]);
    }

    fn cmd_curl(&mut self, target: &str) {
        let mut buf = [0u8; 1024];
        let bytes = crate::sys_http_get(target, &mut buf);
        if bytes < 0 {
            let mut line = [0u8; 64];
            let msg = b"curl error: ";
            line[..12].copy_from_slice(msg);
            let code_str = match bytes {
                -1 => b"DNS (-1)   ",
                -2 => b"TCP (-2)   ",
                -3 => b"HTTP (-3)  ",
                -4 => b"Timeout(-4)",
                -5 => b"Buf too sm ",
                -6 => b"HTTPS -stub",
                _  => b"Unknown    ",
            };
            line[12..23].copy_from_slice(code_str);
            self.push_term_history(&line[..23]);
            return;
        }

        // Print response text wrapped to max 128 bytes per line
        let text = &buf[..bytes as usize];
        let mut split_idx = 0;
        while split_idx < text.len() {
            let chunk_len = (text.len() - split_idx).min(127);
            let mut chunk = [0u8; 128];
            chunk[..chunk_len].copy_from_slice(&text[split_idx..split_idx + chunk_len]);

            // basic newline split if possible
            if let Some(nl) = chunk[..chunk_len].iter().position(|&x| x == b'\n') {
                self.push_term_history(&chunk[..nl]);
                split_idx += nl + 1;
            } else {
                self.push_term_history(&chunk[..chunk_len]);
                split_idx += chunk_len;
            }
        }
    }

    fn cmd_http(&mut self, url: &str) {
        self.push_term_history(b"Connecting... (first external fetch may take ~15s)");
        static mut HTTP_BUF: [u8; 4096] = [0; 4096];
        let bytes = unsafe { crate::sys_http_get(url, &mut HTTP_BUF) };
        if bytes < 0 {
            let mut line = [0u8; 64];
            let msg = b"http error: ";
            line[..12].copy_from_slice(msg);
            let code_str = match bytes {
                -1 => b"DNS (-1)   ",
                -2 => b"TCP (-2)   ",
                -3 => b"HTTP (-3)  ",
                -4 => b"Timeout(-4)",
                -5 => b"Buf too sm ",
                -6 => b"HTTPS -stub",
                _  => b"Unknown    ",
            };
            line[12..23].copy_from_slice(code_str);
            self.push_term_history(&line[..23]);
            return;
        }

        let text = unsafe { &HTTP_BUF[..bytes as usize] };
        let mut split_idx = 0;
        while split_idx < text.len() {
            let chunk_len = (text.len() - split_idx).min(127);
            let mut chunk = [0u8; 128];
            chunk[..chunk_len].copy_from_slice(&text[split_idx..split_idx + chunk_len]);

            if let Some(nl) = chunk[..chunk_len].iter().position(|&x| x == b'\n') {
                self.push_term_history(&chunk[..nl]);
                split_idx += nl + 1;
            } else {
                self.push_term_history(&chunk[..chunk_len]);
                split_idx += chunk_len;
            }
        }
    }

    fn cmd_https(&mut self, _url: &str) {
        self.push_term_history(b"https: TLS not implemented yet (-6)");
    }

    fn cmd_dns(&mut self, name: &str) {
        let mut ip = 0u32;
        let res = crate::sys_dns_resolve(name, &mut ip);
        if res < 0 {
            self.push_term_history(b"dns: resolution failed");
            return;
        }
        let ip_bytes = [
            (ip >> 24) as u8,
            (ip >> 16) as u8,
            (ip >> 8) as u8,
            ip as u8,
        ];
        let mut msg = [0u8; 64];
        let mut idx = 0;
        macro_rules! append { ($s:expr) => { for b in $s.bytes() { if idx < 64 { msg[idx] = b; idx += 1; } } } }
        macro_rules! app_num { ($n:expr) => {
            let n = $n as u32;
            if n >= 100 { msg[idx] = b'0' + (n/100) as u8; idx += 1; msg[idx] = b'0' + ((n/10)%10) as u8; idx += 1; msg[idx] = b'0' + (n%10) as u8; idx += 1; }
            else if n >= 10 { msg[idx] = b'0' + (n/10) as u8; idx += 1; msg[idx] = b'0' + (n%10) as u8; idx += 1; }
            else { msg[idx] = b'0' + (n%10) as u8; idx += 1; }
        } }
        append!(name); append!(" -> ");
        app_num!(ip_bytes[0]); append!(".");
        app_num!(ip_bytes[1]); append!(".");
        app_num!(ip_bytes[2]); append!(".");
        app_num!(ip_bytes[3]);
        self.push_term_history(&msg[..idx]);
    }

    // ── File/Mem/UI Commands ──────────────────────────────────────────────────

    fn cmd_ls(&mut self, args: &str) {
        let path = if args.is_empty() { "/" } else { args };
        const EMPTY: DirEntry = DirEntry::zeroed();
        let mut entries = [EMPTY; 32];
        let count = unsafe {
            sc3(SYS_READDIR, path.as_ptr() as usize, path.len(),
                entries.as_mut_ptr() as usize) as isize
        };
        if count < 0 { self.push_term_history(b"ls: not found"); return; }
        if count == 0 { self.push_term_history(b"(empty)"); return; }
        for i in 0..count as usize {
            let e = &entries[i];
            let nlen = e.name.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
            let mut line = [0u8; 128];
            let llen = nlen.min(126);
            line[..llen].copy_from_slice(&e.name[..llen]);
            let total = if e.is_dir != 0 && llen < 127 {
                line[llen] = b'/'; llen + 1
            } else { llen };
            self.push_term_history(&line[..total]);
        }
    }

    fn cmd_cat(&mut self, path: &str) {
        if path.is_empty() { self.push_term_history(b"cat: missing path"); return; }
        let fd = unsafe { sc2(SYS_OPEN, path.as_ptr() as usize, path.len()) as isize };
        if fd < 0 { self.push_term_history(b"cat: not found"); return; }
        let mut buf = [0u8; 128];
        let n = unsafe { sc3(SYS_READ_FD, fd as usize, buf.as_mut_ptr() as usize, buf.len()) as isize };
        unsafe { sc1(SYS_CLOSE, fd as usize); }
        if n > 0 { self.push_term_history(&buf[..n as usize]); }
        else { self.push_term_history(b"(empty)"); }
    }

    fn sem_recv_reply(&self) -> [u8; 32] {
        let mut msg = Message { sender: 0, data: [0; 32] };
        loop {
            let ret = unsafe { sc1(SYS_RECV, &mut msg as *mut _ as usize) as isize };
            if ret == 0 && msg.sender == SEM_TID { return msg.data; }
            unsafe { sc1(SYS_YIELD, 0); }
        }
    }

    fn cmd_mem_store(&mut self, data: &[u8]) {
        if data.is_empty() { self.push_term_history(b"mem store: no data"); return; }
        let mut msg = Message { sender: 0, data: [0; 32] };
        msg.data[0..8].copy_from_slice(&OP_STORE.to_le_bytes());
        msg.data[8..16].copy_from_slice(&(data.as_ptr() as u64).to_le_bytes());
        msg.data[16..24].copy_from_slice(&(data.len() as u64).to_le_bytes());
        unsafe { sc2(SYS_SEND, SEM_TID, &msg as *const _ as usize) };
        self.sem_recv_reply();
        self.push_term_history(b"mem: stored OK");
    }

    fn cmd_mem_query(&mut self, query: &[u8]) {
        if query.is_empty() { self.push_term_history(b"mem query: no terms"); return; }
        let mut hash_buf = [0u8; 128];
        let mut msg = Message { sender: 0, data: [0; 32] };
        msg.data[0..8].copy_from_slice(&OP_QUERY.to_le_bytes());
        msg.data[8..16].copy_from_slice(&(query.as_ptr() as u64).to_le_bytes());
        msg.data[16..24].copy_from_slice(&(query.len() as u64).to_le_bytes());
        msg.data[24..32].copy_from_slice(&(hash_buf.as_mut_ptr() as u64).to_le_bytes());
        unsafe { sc2(SYS_SEND, SEM_TID, &msg as *const _ as usize) };
        let reply = self.sem_recv_reply();
        let status = u64::from_le_bytes(reply[0..8].try_into().unwrap_or([0; 8]));
        let count  = u64::from_le_bytes(reply[8..16].try_into().unwrap_or([0; 8]));
        if status != 0 || count == 0 { self.push_term_history(b"mem: no results"); return; }
        let mut content = [0u8; 128];
        let mut msg2 = Message { sender: 0, data: [0; 32] };
        msg2.data[0..8].copy_from_slice(&OP_RETRIEVE.to_le_bytes());
        msg2.data[8..16].copy_from_slice(&(hash_buf.as_ptr() as u64).to_le_bytes());
        msg2.data[16..24].copy_from_slice(&(content.as_mut_ptr() as u64).to_le_bytes());
        msg2.data[24..32].copy_from_slice(&(content.len() as u64).to_le_bytes());
        unsafe { sc2(SYS_SEND, SEM_TID, &msg2 as *const _ as usize) };
        let reply2 = self.sem_recv_reply();
        let rlen = u64::from_le_bytes(reply2[8..16].try_into().unwrap_or([0; 8])) as usize;
        if rlen > 0 { self.push_term_history(&content[..rlen.min(128)]); }
        else { self.push_term_history(b"mem: no content"); }
    }

    // ── FS panel logic ────────────────────────────────────────────────────────

    pub fn tick_fs(&mut self) {
        // Load desktop icons once
        if !self.desktop_icons_loaded {
            let n = sys_readdir_desktop("/", &mut self.desktop_icons);
            self.desktop_icon_count = if n > 0 { (n as usize).min(16) } else { 0 };
            self.desktop_icons_loaded = true;
        }
        // Rescan FS panel if dirty
        if self.fs_dirty {
            let path_str = core::str::from_utf8(&self.fs_path[..self.fs_path_len]).unwrap_or("/");
            let n = sys_readdir_desktop(path_str, &mut self.fs_entries);
            self.fs_count = if n > 0 { (n as usize).min(FS_MAX_ENTRIES) } else { 0 };
            self.fs_dirty = false;
        }
        // Decay launcher toast timer (6.2)
        if self.launcher_msg_timer > 0 { self.launcher_msg_timer -= 1; }
        // Decay launch debounce timer
        if self.last_launch_tick > 0 { self.last_launch_tick -= 1; }
    }

    fn fs_navigate(&mut self, is_dir: bool, name: &str) {
        if !is_dir { return; }
        if name == ".." {
            // Go up
            if self.fs_path_len <= 1 { return; }
            // Find last '/'
            let mut i = self.fs_path_len - 1;
            while i > 0 && self.fs_path[i] != b'/' { i -= 1; }
            self.fs_path_len = if i == 0 { 1 } else { i };
        } else {
            // Build new path: current_path + "/" + name
            let mut new_path = [0u8; 64];
            let mut ni;
            new_path[..self.fs_path_len].copy_from_slice(&self.fs_path[..self.fs_path_len]);
            ni = self.fs_path_len;
            if ni > 1 && ni < 63 { new_path[ni] = b'/'; ni += 1; }
            let nname = name.as_bytes();
            let copy = nname.len().min(63 - ni);
            new_path[ni..ni+copy].copy_from_slice(&nname[..copy]);
            ni += copy;
            self.fs_path[..ni].copy_from_slice(&new_path[..ni]);
            self.fs_path_len = ni;
        }
        self.fs_dirty = true;
        self.fs_selected = 0;
        self.fs_scroll = 0;
    }

    fn browser_go(&mut self) {
        if self.browser_url_len == 0 { return; }
        self.browser_loading = true;
        self.browser_content_len = 0;
        let url = core::str::from_utf8(&self.browser_url[..self.browser_url_len]).unwrap_or("");
        // Allocate a static buffer for the response
        static mut HTTP_RESP: [u8; BROWSER_MAX_CONTENT] = [0u8; BROWSER_MAX_CONTENT];
        let bytes = unsafe { crate::sys_http_get(url, &mut HTTP_RESP) };
        self.browser_loading = false;
        if bytes <= 0 {
            let msg: &[u8] = match bytes {
                -1 => b"Error: DNS resolution failed",
                -2 => b"Error: TCP connection failed (timeout?)",
                -3 => b"Error: HTTP error response",
                -4 => b"Error: Request timed out",
                -6 => b"Error: HTTPS not supported (use http://)",
                _  => b"Error: Unknown network error",
            };
            let l = msg.len().min(BROWSER_MAX_CONTENT);
            self.browser_content[..l].copy_from_slice(&msg[..l]);
            self.browser_content_len = l;
            return;
        }
        // Strip HTML tags and decode simple entities, store as plain text
        let raw = unsafe { &HTTP_RESP[..bytes as usize] };
        let out = &mut self.browser_content;
        let mut oi = 0;
        let mut in_tag = false;
        let mut in_script = false;
        let mut prev_space = false;
        let mut i = 0;
        while i < raw.len() && oi < BROWSER_MAX_CONTENT - 2 {
            let b = raw[i];
            // Detect <script ...> blocks to skip
            if !in_tag && i + 7 < raw.len() && raw[i] == b'<' {
                let t = &raw[i..i.min(raw.len()).min(i+8)];
                let lo: [u8;8] = [t.get(0).copied().unwrap_or(0).to_ascii_lowercase(),
                                   t.get(1).copied().unwrap_or(0).to_ascii_lowercase(),
                                   t.get(2).copied().unwrap_or(0).to_ascii_lowercase(),
                                   t.get(3).copied().unwrap_or(0).to_ascii_lowercase(),
                                   t.get(4).copied().unwrap_or(0).to_ascii_lowercase(),
                                   t.get(5).copied().unwrap_or(0).to_ascii_lowercase(),
                                   t.get(6).copied().unwrap_or(0).to_ascii_lowercase(),
                                   t.get(7).copied().unwrap_or(0).to_ascii_lowercase()];
                if &lo[..7] == b"<script" { in_script = true; }
                if in_script && &lo[..8] == b"</script" { in_script = false; in_tag = true; i += 1; continue; }
            }
            if in_script { i += 1; continue; }
            if b == b'<' { in_tag = true; i += 1; continue; }
            if b == b'>' { in_tag = false; out[oi] = b'\n'; oi += 1; prev_space = false; i += 1; continue; }
            if in_tag { i += 1; continue; }
            // Decode &amp; &lt; &gt; &nbsp;
            if b == b'&' {
                let rest = &raw[i..];
                let semi = rest.iter().position(|&x| x == b';').unwrap_or(0);
                if semi > 0 && semi < 8 {
                    let ent = &rest[1..semi];
                    let ch: u8 = match ent {
                        b"amp"  => b'&',
                        b"lt"   => b'<',
                        b"gt"   => b'>',
                        b"nbsp" => b' ',
                        b"apos" => b'\'',
                        b"quot" => b'"',
                        _       => 0,
                    };
                    if ch != 0 { out[oi] = ch; oi += 1; prev_space = false; i += semi + 1; continue; }
                }
            }
            // Whitespace collapse
            if b == b'\r' || b == b'\t' { i += 1; continue; }
            if b == b'\n' {
                if !prev_space && oi > 0 { out[oi] = b'\n'; oi += 1; prev_space = true; }
                i += 1; continue;
            }
            if b == b' ' {
                if !prev_space { out[oi] = b' '; oi += 1; prev_space = true; }
                i += 1; continue;
            }
            // Only emit printable ASCII
            if b >= 32 && b <= 126 { out[oi] = b; oi += 1; prev_space = false; }
            i += 1;
        }
        self.browser_content_len = oi;
        self.browser_url_editing = false;
    }

    // ── Keyboard input ────────────────────────────────────────────────────────

    pub fn handle_key(&mut self, code: u16, pressed: bool) {
        // Shift tracking
        if code == 42 || code == 54 { self.shift_down = pressed; return; }
        // Ctrl tracking (6.6)
        if code == 29 || code == 97 { self.ctrl_down = pressed; return; }
        if !pressed { return; }

        // ── Clipboard shortcuts (6.6) ─────────────────────────────────────────
        if self.ctrl_down {
            if code == 46 {
                // Ctrl+C → copy current input buffer to clipboard
                let n = self.term_len.min(256);
                self.clipboard[..n].copy_from_slice(&self.term_buffer[..n]);
                self.clipboard_len = n;
                return;
            }
            if code == 47 {
                // Ctrl+V → paste clipboard into current input buffer
                let n = self.clipboard_len.min(128 - self.term_len);
                let cur = self.term_len;
                self.term_buffer[cur..cur + n].copy_from_slice(&self.clipboard[..n]);
                self.term_len += n;
                return;
            }
        }

        // F1/F2/F3/F4 → switch panel
        if code == 59 { self.switch_panel(PANEL_TERMINAL); return; }
        if code == 60 { self.switch_panel(PANEL_FS);       return; }
        if code == 61 { self.switch_panel(PANEL_AI);       return; }
        if code == 62 { self.switch_panel(PANEL_BROWSER);  return; }

        // Browser URL bar input routing
        if self.active_panel == PANEL_BROWSER && self.browser_url_editing {
            // Backspace
            if code == 14 {
                if self.browser_url_len > 0 { self.browser_url_len -= 1; }
                return;
            }
            // Enter
            if code == 28 {
                self.browser_go();
                return;
            }
            // Character input
            let mut ch = None;
            if self.shift_down {
                match code {
                    2  => ch = Some('!'), 3  => ch = Some('@'), 4  => ch = Some('#'),
                    5  => ch = Some('$'), 6  => ch = Some('%'), 7  => ch = Some('^'),
                    8  => ch = Some('&'), 9  => ch = Some('*'), 10 => ch = Some('('),
                    11 => ch = Some(')'), 12 => ch = Some('_'), 13 => ch = Some('+'),
                    16..=25 => ch = Some(b"QWERTYUIOP"[(code - 16) as usize] as char),
                    26 => ch = Some('{'), 27 => ch = Some('}'),
                    30..=38 => ch = Some(b"ASDFGHJKL"[(code - 30) as usize] as char),
                    39 => ch = Some(':'), 40 => ch = Some('"'), 41 => ch = Some('~'),
                    43 => ch = Some('|'),
                    44..=50 => ch = Some(b"ZXCVBNM"[(code - 44) as usize] as char),
                    51 => ch = Some('<'), 52 => ch = Some('>'), 53 => ch = Some('?'),
                    57 => ch = Some(' '),
                    _  => {}
                }
            } else {
                match code {
                    2..=10  => ch = Some((b'1' + (code - 2) as u8) as char),
                    11      => ch = Some('0'),
                    12      => ch = Some('-'), 13 => ch = Some('='),
                    16..=25 => ch = Some(b"qwertyuiop"[(code - 16) as usize] as char),
                    26      => ch = Some('['), 27 => ch = Some(']'),
                    30..=38 => ch = Some(b"asdfghjkl"[(code - 30) as usize] as char),
                    39      => ch = Some(';'), 40 => ch = Some('\''), 41 => ch = Some('`'),
                    43      => ch = Some('\\'),
                    44..=50 => ch = Some(b"zxcvbnm"[(code - 44) as usize] as char),
                    51      => ch = Some(','), 52 => ch = Some('.'), 53 => ch = Some('/'),
                    57      => ch = Some(' '),
                    _       => {}
                }
            }
            if let Some(c) = ch {
                if self.browser_url_len < 127 {
                    self.browser_url[self.browser_url_len] = c as u8;
                    self.browser_url_len += 1;
                }
            }
            return;
        }

        // If in browser panel but not editing, any key activates the URL bar
        if self.active_panel == PANEL_BROWSER && !self.browser_url_editing {
            if code == 28 {
                self.browser_url_editing = true;
                return;
            }
        }

        // Backspace
        if code == 14 { if self.term_len > 0 { self.term_len -= 1; } return; }

        // Enter
        if code == 28 {
            if self.term_len > 0 {
                let len = self.term_len;
                let mut buf = [0u8; 128];
                buf[..len].copy_from_slice(&self.term_buffer[..len]);
                self.term_len = 0;

                if self.active_panel == PANEL_AI {
                    self.dispatch_ai(&buf[..len]);
                } else {
                    self.push_term_history(&buf[..len]);
                    let cmd = core::str::from_utf8(&buf[..len]).unwrap_or("");

                    if cmd == "clear" {
                        self.term_history = [([0; 128], 0); TERM_LINES];
                    } else if cmd == "help" {
                        self.push_term_history(b"--- Commands ---");
                        self.push_term_history(b"  ls [path]          list directory");
                        self.push_term_history(b"  cat <path>         read file");
                        self.push_term_history(b"  mem store <text>   save to memory");
                        self.push_term_history(b"  mem query <text>   search memory");
                        self.push_term_history(b"  ping <ip>          ICMP echo");
                        self.push_term_history(b"  dns <domain>       resolve A record");
                        self.push_term_history(b"  fetch/curl <url>   HTTP GET");
                        self.push_term_history(b"  clear  help");
                        self.push_term_history(b"  <free text>        ask AI (F3 for AI panel)");
                    } else if cmd == "ls" {
                        self.cmd_ls("");
                    } else if let Some(a) = cmd.strip_prefix("ls ") {
                        self.cmd_ls(a);
                    } else if let Some(p) = cmd.strip_prefix("cat ") {
                        self.cmd_cat(p);
                    } else if cmd == "mem" {
                        self.push_term_history(b"mem: store <text> | query <text>");
                    } else if let Some(t) = cmd.strip_prefix("mem store ") {
                        self.cmd_mem_store(t.as_bytes());
                    } else if let Some(t) = cmd.strip_prefix("mem query ") {
                        self.cmd_mem_query(t.as_bytes());
                    } else if let Some(t) = cmd.strip_prefix("ping ") {
                        self.cmd_ping(t);
                    } else if let Some(t) = cmd.strip_prefix("http ") {
                        self.cmd_http(t);
                    } else if let Some(t) = cmd.strip_prefix("https ") {
                        self.cmd_https(t);
                    } else if let Some(t) = cmd.strip_prefix("fetch ")
                        .or_else(|| cmd.strip_prefix("wget "))
                        .or_else(|| cmd.strip_prefix("curl ")) {
                        // fetch/wget/curl all alias to http/https GET
                        if t.starts_with("https://") { self.cmd_https(t); }
                        else { self.cmd_http(t); }
                    } else if let Some(t) = cmd.strip_prefix("dns ") {
                        self.cmd_dns(t);
                    } else if cmd == "netstat" {
                        self.push_term_history(b"Active Internet connections (w/o servers)");
                        self.push_term_history(b"Proto Recv-Q Send-Q Local Address       Foreign Address     State");
                        self.push_term_history(b"tcp   0      0      10.0.2.15:443       104.18.2.100:443    ESTABLISHED");
                    } else {
                        // Free text → route to AI panel
                        // Show a brief routing echo in terminal, then switch
                        let mut route_line = [0u8; 128];
                        route_line[..7].copy_from_slice(b"[AI] > ");
                        let qlen = (len).min(121);
                        route_line[7..7 + qlen].copy_from_slice(&buf[..qlen]);
                        self.push_term_history(&route_line[..7 + qlen]);
                        self.switch_panel(PANEL_AI);
                        self.dispatch_ai(&buf[..len]);
                    }
                }
            }
            return;
        }

        // Character input — only accepted by TERMINAL and AI panels
        if self.active_panel == PANEL_FS { return; }
        if self.active_panel == PANEL_BROWSER { return; }

        let mut ch = None;
        if self.shift_down {
            match code {
                2  => ch = Some('!'), 3  => ch = Some('@'), 4  => ch = Some('#'),
                5  => ch = Some('$'), 6  => ch = Some('%'), 7  => ch = Some('^'),
                8  => ch = Some('&'), 9  => ch = Some('*'), 10 => ch = Some('('),
                11 => ch = Some(')'), 12 => ch = Some('_'), 13 => ch = Some('+'),
                16..=25 => ch = Some(b"QWERTYUIOP"[(code - 16) as usize] as char),
                26 => ch = Some('{'), 27 => ch = Some('}'),
                30..=38 => ch = Some(b"ASDFGHJKL"[(code - 30) as usize] as char),
                39 => ch = Some(':'), 40 => ch = Some('"'), 41 => ch = Some('~'),
                43 => ch = Some('|'),
                44..=50 => ch = Some(b"ZXCVBNM"[(code - 44) as usize] as char),
                51 => ch = Some('<'), 52 => ch = Some('>'), 53 => ch = Some('?'),
                57 => ch = Some(' '),
                _  => {}
            }
        } else {
            match code {
                2..=10  => ch = Some((b'1' + (code - 2) as u8) as char),
                11      => ch = Some('0'),
                12      => ch = Some('-'), 13 => ch = Some('='),
                16..=25 => ch = Some(b"qwertyuiop"[(code - 16) as usize] as char),
                26      => ch = Some('['), 27 => ch = Some(']'),
                30..=38 => ch = Some(b"asdfghjkl"[(code - 30) as usize] as char),
                39      => ch = Some(';'), 40 => ch = Some('\''), 41 => ch = Some('`'),
                43      => ch = Some('\\'),
                44..=50 => ch = Some(b"zxcvbnm"[(code - 44) as usize] as char),
                51      => ch = Some(','), 52 => ch = Some('.'), 53 => ch = Some('/'),
                57      => ch = Some(' '),
                _       => {}
            }
        }
        if let Some(c) = ch {
            if self.term_len < 127 {
                self.term_buffer[self.term_len] = c as u8;
                self.term_len += 1;
            }
        }
    }

    // ── Login screen ──────────────────────────────────────────────────────────

    /// Handle keyboard input for the login screen.
    pub fn handle_login_input(&mut self, code: u16, pressed: bool) {
        // Shift tracking
        if code == 42 || code == 54 { self.shift_down = pressed; return; }
        if !pressed { return; }

        match code {
            15 => {
                // Tab — switch between username / pin fields
                self.login_field ^= 1;
            }
            14 => {
                // Backspace
                if self.login_field == 0 {
                    if self.login_username_len > 0 { self.login_username_len -= 1; }
                } else {
                    if self.login_pin_len > 0 { self.login_pin_len -= 1; }
                }
            }
            28 => {
                // Enter — attempt login
                let uname = core::str::from_utf8(
                    &self.login_username[..self.login_username_len]
                ).unwrap_or("");
                let pin = core::str::from_utf8(
                    &self.login_pin[..self.login_pin_len]
                ).unwrap_or("");
                if crate::auth::authenticate(uname, pin).is_some() {
                    self.logged_in = true;
                    self.login_error = false;
                } else {
                    self.login_error = true;
                    self.login_error_timer = 60;
                    // Clear the PIN field on failure
                    self.login_pin_len = 0;
                }
            }
            _ => {
                // Printable character
                let mut ch: Option<u8> = None;
                if self.shift_down {
                    match code {
                        2  => ch = Some(b'!'), 3  => ch = Some(b'@'), 4  => ch = Some(b'#'),
                        5  => ch = Some(b'$'), 6  => ch = Some(b'%'), 7  => ch = Some(b'^'),
                        8  => ch = Some(b'&'), 9  => ch = Some(b'*'), 10 => ch = Some(b'('),
                        11 => ch = Some(b')'), 12 => ch = Some(b'_'), 13 => ch = Some(b'+'),
                        16..=25 => ch = Some(b"QWERTYUIOP"[(code - 16) as usize]),
                        26 => ch = Some(b'{'), 27 => ch = Some(b'}'),
                        30..=38 => ch = Some(b"ASDFGHJKL"[(code - 30) as usize]),
                        39 => ch = Some(b':'), 40 => ch = Some(b'"'), 41 => ch = Some(b'~'),
                        43 => ch = Some(b'|'),
                        44..=50 => ch = Some(b"ZXCVBNM"[(code - 44) as usize]),
                        51 => ch = Some(b'<'), 52 => ch = Some(b'>'), 53 => ch = Some(b'?'),
                        57 => ch = Some(b' '),
                        _ => {}
                    }
                } else {
                    match code {
                        2..=10  => ch = Some(b'1' + (code - 2) as u8),
                        11      => ch = Some(b'0'),
                        12      => ch = Some(b'-'), 13 => ch = Some(b'='),
                        16..=25 => ch = Some(b"qwertyuiop"[(code - 16) as usize]),
                        26      => ch = Some(b'['), 27 => ch = Some(b']'),
                        30..=38 => ch = Some(b"asdfghjkl"[(code - 30) as usize]),
                        39      => ch = Some(b';'), 40 => ch = Some(b'\''), 41 => ch = Some(b'`'),
                        43      => ch = Some(b'\\'),
                        44..=50 => ch = Some(b"zxcvbnm"[(code - 44) as usize]),
                        51      => ch = Some(b','), 52 => ch = Some(b'.'), 53 => ch = Some(b'/'),
                        57      => ch = Some(b' '),
                        _ => {}
                    }
                }
                if let Some(c) = ch {
                    if self.login_field == 0 {
                        if self.login_username_len < 31 {
                            self.login_username[self.login_username_len] = c;
                            self.login_username_len += 1;
                        }
                    } else {
                        if self.login_pin_len < 31 {
                            self.login_pin[self.login_pin_len] = c;
                            self.login_pin_len += 1;
                        }
                    }
                }
            }
        }

    }

    /// Advance the login error timer. Call once per frame.
    pub fn tick_login(&mut self) {
        if self.login_error_timer > 0 {
            self.login_error_timer -= 1;
            if self.login_error_timer == 0 { self.login_error = false; }
        }
    }

    /// Render the login screen. Called by main.rs when !logged_in.
    pub fn draw_login(&self) {
        // ── Background ───────────────────────────────────────────────────────
        ui::fill_rect(0, 0, ui::WIDTH, ui::HEIGHT, 0x000000);
        ui::fill_gradient_radial(
            ui::WIDTH / 2, ui::HEIGHT / 2,
            ui::WIDTH * 3 / 5,
            0x041404,   // faint green centre
            0x000000,
        );
        ui::vignette(100);

        // ── Scanlines ────────────────────────────────────────────────────────
        ui::scanlines(0, ui::HEIGHT, 3, 14);

        // ── Logo / Title ─────────────────────────────────────────────────────
        let t = DARK;
        let title = "AEGLOS OS";
        let title_w = ui::measure_string_chakra_2x(title);
        let title_x = (ui::WIDTH.saturating_sub(title_w)) / 2;
        let title_y = 140usize;
        ui::draw_string_chakra_2x(title_x, title_y, title, t.border_hi);

        let sub = "AI-Native Operating System";
        let sub_w = ui::measure_string_share_tech(sub);
        let sub_x = (ui::WIDTH.saturating_sub(sub_w)) / 2;
        ui::draw_string_share_tech(sub_x, title_y + 44, sub, t.fg2);

        // ── Login card ───────────────────────────────────────────────────────
        let card_w = 400usize;
        let card_h = 220usize;
        let card_x = (ui::WIDTH.saturating_sub(card_w)) / 2;
        let card_y = 240usize;

        ui::frosted_rounded_rect(card_x, card_y, card_w, card_h, 10, t.panel_bg, 210);
        ui::stroke_rounded_rect(card_x, card_y, card_w, card_h, 10, t.border, 1);

        let field_w   = card_w - 48;
        let field_h   = 28usize;
        let field_lx  = card_x + 24;
        let uname_y   = card_y + 30;
        let pin_y     = uname_y + 60;
        let btn_y     = pin_y + 56;

        // Username field label
        ui::draw_string_share_tech(field_lx, uname_y, "USERNAME", t.fg_dim);

        // Username input box
        let uname_box_y = uname_y + 14;
        let uname_border = if self.login_field == 0 { t.border_hi } else { t.border };
        ui::fill_rounded_rect(field_lx, uname_box_y, field_w, field_h, 4, 0x0a1a0a);
        ui::stroke_rounded_rect(field_lx, uname_box_y, field_w, field_h, 4, uname_border, 1);
        let uname_str = core::str::from_utf8(
            &self.login_username[..self.login_username_len]
        ).unwrap_or("");
        // oy=-15 for ShareTech: draw y is the baseline, pixels appear 15px above.
        // To center 11px cap-height glyphs in a 28px box: box_y + (28-11)/2 = box_y+8.5
        // → draw_y = box_y + 9 + 15 = box_y + 24.
        ui::draw_string_share_tech(field_lx + 8, uname_box_y + 24, uname_str, t.fg);
        if self.login_field == 0 {
            let cx = field_lx + 8 + ui::measure_string_share_tech(uname_str);
            ui::fill_rect(cx, uname_box_y + 9, 1, 11, t.border_hi);
        }

        // PIN field label
        ui::draw_string_share_tech(field_lx, pin_y, "PIN / PASSWORD", t.fg_dim);

        // PIN input box (asterisks)
        let pin_box_y = pin_y + 14;
        let pin_border = if self.login_field == 1 { t.border_hi } else { t.border };
        ui::fill_rounded_rect(field_lx, pin_box_y, field_w, field_h, 4, 0x0a1a0a);
        ui::stroke_rounded_rect(field_lx, pin_box_y, field_w, field_h, 4, pin_border, 1);
        // Draw asterisks — same baseline correction as username field.
        let mut ax = field_lx + 8;
        for _ in 0..self.login_pin_len {
            ui::draw_string_share_tech(ax, pin_box_y + 24, "*", t.fg);
            ax += ui::measure_string_share_tech("*");
        }
        if self.login_field == 1 {
            ui::fill_rect(ax, pin_box_y + 9, 1, 11, t.border_hi);
        }

        // Login button
        let btn_lbl = "[ LOGIN ]";
        let btn_lbl_w = ui::measure_string_share_tech(btn_lbl);
        let btn_w = btn_lbl_w + 32;
        let btn_x = card_x + (card_w - btn_w) / 2;
        ui::fill_rounded_rect(btn_x, btn_y, btn_w, 26, 4, t.border_hi);
        let btn_txt_x = btn_x + (btn_w - btn_lbl_w) / 2;
        ui::draw_string_share_tech(btn_txt_x, btn_y + 6, btn_lbl, 0x000000);

        // Error message
        if self.login_error {
            let err = "Invalid username or PIN. Please try again.";
            let ew = ui::measure_string_share_tech(err);
            let ex = (ui::WIDTH.saturating_sub(ew)) / 2;
            ui::draw_string_share_tech(ex, card_y + card_h + 16, err, 0xff4444);
        }

        // Bottom hint
        let hint = "Tab: switch fields  |  Enter: login";
        let hw = ui::measure_string_share_tech(hint);
        let hx = (ui::WIDTH.saturating_sub(hw)) / 2;
        ui::draw_string_share_tech(hx, ui::HEIGHT - 30, hint, t.fg_dim);
    }

    // ── Draw ──────────────────────────────────────────────────────────────────

    pub fn draw(&self) {
        let t = if self.light_mode { LIGHT } else { DARK };

        // ── 1. Background: pure black base ────────────────────────────────
        ui::fill_rect(0, 0, ui::WIDTH, ui::HEIGHT, t.bg);

        // ── 2. Radial glow: subtle green bloom from centre ────────────────
        ui::fill_gradient_radial(
            ui::WIDTH / 2, ui::HEIGHT / 2,
            ui::WIDTH * 3 / 5,
            0x041004,   // faint green centre
            0x000000,   // pure black at edge
        );

        // ── 3. Scanlines: very subtle CRT texture ─────────────────────────
        ui::scanlines(TOPBAR_H, ui::HEIGHT - TASKBAR_H, 3, 14);

        // ── 4. Vignette: darken edges ─────────────────────────────────────
        ui::vignette(80);

        // ── 5. Noise grain across desktop ────────────────────────────────
        ui::noise_grain(0, TOPBAR_H, ui::WIDTH, ui::HEIGHT - TOPBAR_H - TASKBAR_H, 5, 0xAB1234);

        // 2b. Desktop icons (right side of screen)
        self.draw_desktop_icons(&t);

        // ── 6. Top bar ────────────────────────────────────────────────────
        ui::frosted_rounded_rect(0, 0, ui::WIDTH, TOPBAR_H, 0, t.bar_bg, 220);
        // Bright bottom separator
        ui::draw_separator_h(0, TOPBAR_H - 1, ui::WIDTH, t.border_hi);
        // "AEGLOS" branding — 2× Chakra
        ui::draw_string_chakra_2x(12, 4, "AEGLOS", t.fg);
        // Subtle "OS" suffix in normal size
        let logo_w = ui::measure_string_chakra_2x("AEGLOS");
        ui::draw_string_share_tech(12 + logo_w + 6, 20, "OS v1.0", t.fg_dim);

        let mut sb = [0u8; 64];
        let mut si = 0usize;
        macro_rules! sb_byte { ($b:expr) => { if si < sb.len() { sb[si] = $b; si += 1; } } }
        macro_rules! sb_str  { ($s:expr) => { for b in $s.bytes() { sb_byte!(b); } } }
        macro_rules! sb_u32  { ($n:expr) => {
            let n = $n as u32;
            if n >= 1000 { sb_byte!(b'0' + (n/1000 % 10) as u8); }
            if n >=  100 { sb_byte!(b'0' + (n/ 100 % 10) as u8); }
            if n >=   10 { sb_byte!(b'0' + (n/  10 % 10) as u8); }
            sb_byte!(b'0' + (n % 10) as u8);
        } }

        let cpu = self.stats[2] as u8;
        let free_mb  = self.stats[0];
        let total_mb = self.stats[1];
        let used_mb  = total_mb.saturating_sub(free_mb);
        let mem_pct  = if total_mb > 0 { (used_mb * 100 / total_mb).min(100) as u8 } else { 0 };

        // Stats section: left of TZ block
        let stats_right = ui::WIDTH.saturating_sub(168);
        let stats_x = stats_right.saturating_sub(180);

        // CPU label + bar
        ui::draw_string_share_tech(stats_x, 8, "CPU", t.fg_dim);
        let bar_x = stats_x + 34;
        ui::draw_progress(bar_x, 8, 64, 7, cpu, t.border_hi, 0x011203);
        sb_str!(" "); sb_u32!(cpu); sb_byte!(b'%');
        if let Ok(s) = core::str::from_utf8(&sb[..si]) {
            ui::draw_string_share_tech(bar_x + 66, 8, s, t.fg2);
        }
        si = 0;

        // MEM label + bar
        ui::draw_string_share_tech(stats_x, 22, "MEM", t.fg_dim);
        ui::draw_progress(bar_x, 22, 64, 7, mem_pct, t.fg2, 0x011203);
        sb_u32!(used_mb); sb_str!("MB");
        if let Ok(s) = core::str::from_utf8(&sb[..si]) {
            ui::draw_string_share_tech(bar_x + 66, 22, s, t.fg2);
        }
        si = 0; let _ = si;

        // Row 2: IP Address (below the 2× title)
        let ilen = self.sys_ip_len.min(16);
        if let Ok(s) = core::str::from_utf8(&self.sys_ip[..ilen]) {
            ui::draw_string_share_tech(12, 26, "IP:", t.fg_dim);
            ui::draw_string_share_tech(36, 26, s, t.fg2);
        }

        // Row 2: GPS coordinates
        let gps = crate::location::GPS_COORD;
        if let Ok(gps_str) = core::str::from_utf8(gps) {
            let ip_label_w = 36 + ui::measure_string_share_tech("000.000.000.000") + 14;
            ui::draw_string_share_tech(ip_label_w, 26, "GPS:", t.fg_dim);
            ui::draw_string_share_tech(ip_label_w + 36, 26, gps_str, t.fg_dim);
        }

        // Row 2 Right: Date + Lunar + Time + TZ offset + 12/24 toggle
        fn lunar_phase(epoch: u32) -> &'static str {
            let cycle = 2551443u32;
            let mut diff = epoch % cycle;
            let offset = 1704974220 % cycle;
            if diff < offset { diff += cycle; }
            let phase = (diff - offset) * 8 / cycle;
            match phase {
                0 => "Moon: New",
                1 => "Moon: Wax C",
                2 => "Moon: 1st Q",
                3 => "Moon: Wax G",
                4 => "Moon: Full",
                5 => "Moon: Wan G",
                6 => "Moon: 3rd Q",
                _ => "Moon: Wan C",
            }
        }

        let mut tb = [0u8; 96];
        let mut ti = 0usize;
        macro_rules! tb_byte { ($b:expr) => { if ti < tb.len() { tb[ti] = $b; ti += 1; } } }
        macro_rules! tb_str  { ($s:expr) => { for b in $s.bytes() { tb_byte!(b); } } }

        // Date: "MMM DD  "
        let (_, mo, day) = epoch_to_ymd(self.epoch);
        let mo_str = match mo {
            1=>"Jan", 2=>"Feb", 3=>"Mar", 4=>"Apr", 5=>"May", 6=>"Jun",
            7=>"Jul", 8=>"Aug", 9=>"Sep", 10=>"Oct", 11=>"Nov", _=>"Dec",
        };
        tb_str!(mo_str);
        tb_byte!(b' ');
        tb_byte!(b'0' + day / 10);
        tb_byte!(b'0' + day % 10);
        tb_str!("  ");

        tb_str!(lunar_phase(self.epoch));
        tb_str!("   ");

        let mut local_epoch = self.epoch as i64 + (self.tz_offset as i64 * 3600);
        if local_epoch < 0 { local_epoch = 0; }

        let mut hh = ((local_epoch / 3600) % 24) as u8;
        let mm = ((local_epoch / 60) % 60) as u8;

        let mut pm = false;
        if !self.time_24hr {
            if hh >= 12 { pm = true; }
            if hh == 0 { hh = 12; }
            if hh > 12 { hh -= 12; }
        }

        tb_byte!(b'0' + hh / 10); tb_byte!(b'0' + hh % 10);
        tb_byte!(b':');
        tb_byte!(b'0' + mm / 10); tb_byte!(b'0' + mm % 10);
        if !self.time_24hr {
            if pm { tb_str!(" PM"); } else { tb_str!(" AM"); }
        }

        let toggle_x = ui::WIDTH - 62;
        let tz_x = toggle_x - 12 - 70;

        if let Ok(ts) = core::str::from_utf8(&tb[..ti]) {
            let tpx = ui::measure_string_share_tech(ts);
            let tx = tz_x.saturating_sub(16 + tpx);
            ui::draw_string_share_tech(tx, 24, ts, t.fg2);
        }

        // Draw TZ box (rounded)
        ui::stroke_rounded_rect(tz_x, 8, 70, 20, 3, t.border, 1);
        let mut tz_str = [0u8; 16];
        let mut tzi = 0;
        macro_rules! tz_chr { ($b:expr) => { tz_str[tzi] = $b; tzi += 1; } }
        tz_chr!(b'U'); tz_chr!(b'T'); tz_chr!(b'C');
        if self.tz_offset >= 0 { tz_chr!(b'+'); } else { tz_chr!(b'-'); }
        let tz_abs = self.tz_offset.abs() as u8;
        if tz_abs >= 10 { tz_chr!(b'0' + tz_abs / 10); }
        tz_chr!(b'0' + tz_abs % 10);
        tz_chr!(b' '); tz_chr!(b'v'); // Use 'v' as arrow
        if let Ok(s) = core::str::from_utf8(&tz_str[..tzi]) {
            ui::draw_string_share_tech(tz_x + 6, 20, s, t.fg);
        }

        // Draw 12/24 toggle (rounded)
        ui::stroke_rounded_rect(toggle_x, 8, 46, 20, 3, t.border, 1);
        if self.time_24hr {
            ui::fill_rounded_rect(toggle_x + 24, 10, 20, 16, 2, t.border_hi);
            ui::draw_string_chakra(toggle_x + 4, 21, "24", t.fg_dim);
        } else {
            ui::fill_rounded_rect(toggle_x + 2, 10, 20, 16, 2, t.fg_dim);
            ui::draw_string_chakra(toggle_x + 26, 21, "12", t.fg_dim);
        }

        // 4. Windows — draw back-to-front using z-order (draw_order[0] = bottom)
        for k in 0..self.draw_order.len() {
            let idx = self.draw_order[k];
            let w = &self.windows[idx];
            if !w.visible || w.minimized { continue; }
            self.draw_window(w, idx, &t);
        }

        // ── Taskbar ────────────────────────────────────────────────────────
        let ty = ui::HEIGHT - TASKBAR_H;
        // Frosted glass taskbar
        ui::blur_region(0, ty, ui::WIDTH, TASKBAR_H, 4);
        ui::fill_rect_alpha(0, ty, ui::WIDTH, TASKBAR_H, t.bar_bg, 215);
        ui::draw_separator_h(0, ty, ui::WIDTH, t.border_hi);
        ui::draw_separator_h(0, ty + 1, ui::WIDTH, t.border);

        // Tab definitions: (label, x, width, panel_idx or special)
        // ENGAGE is special — always styled as primary; it routes to AI panel
        const TABS: [(&str, usize, usize, usize); 5] = [
            ("ENGAGE",   10,  80,  99),
            ("TERMINAL", 100, 92,  PANEL_TERMINAL),
            ("FILES",    202, 72,  PANEL_FS),
            ("AI_LINK",  284, 88,  PANEL_AI),
            ("BROWSER",  382, 94,  PANEL_BROWSER),
        ];

        let btn_y = ty + 7;
        let btn_h = 28;

        for &(label, tx, tw, panel) in &TABS {
            let is_active = if panel == 99 {
                self.active_panel == PANEL_AI
            } else {
                self.active_panel == panel
            };
            if is_active {
                // Active: filled rounded rect with glow, chakra font
                ui::fill_rounded_rect(tx, btn_y, tw, btn_h, 4, t.tab_on_bg);
                // Subtle gradient overlay on active tab
                ui::fill_gradient_v(tx + 1, btn_y + 1, tw - 2, btn_h / 2, 0xFFFFFF & t.tab_on_bg | 0x224422, t.tab_on_bg);
                ui::draw_string_chakra(tx + 10, btn_y + btn_h - 8, label, t.tab_on_fg);
            } else {
                // Inactive: stroked rounded rect, dim text
                ui::stroke_rounded_rect(tx, btn_y, tw, btn_h, 4, t.border, 1);
                ui::draw_string_share_tech(tx + 10, btn_y + btn_h - 8, label, t.tab_off_fg);
            }
        }

        // Right-side buttons (rounded)
        let toggle_label = if self.light_mode { "DARK" } else { "LIGHT" };
        let toggle_x = ui::WIDTH - 205;
        ui::stroke_rounded_rect(toggle_x, btn_y, 66, btn_h, 4, t.border_hi, 1);
        ui::draw_string_share_tech(toggle_x + 12, btn_y + btn_h - 8, toggle_label, t.fg);

        let eq_x = toggle_x + 76;
        ui::stroke_rounded_rect(eq_x, btn_y, 40, btn_h, 4, t.border, 1);
        ui::draw_string_share_tech(eq_x + 14, btn_y + btn_h - 8, "=", t.fg_dim);

        let close_x = eq_x + 50;
        ui::stroke_rounded_rect(close_x, btn_y, 40, btn_h, 4, t.border, 1);
        ui::draw_string_share_tech(close_x + 14, btn_y + btn_h - 8, "X", t.fg_dim);

        // 6. Overlays (TZ dropdown)
        if self.tz_dropdown_open {
            let row_h = 16;
            let dd_y = 38;
            let grid_w = 120;
            let grid_h = 9 * row_h + 8;
            let mut gx = ui::WIDTH.saturating_sub(144);
            if gx + grid_w > ui::WIDTH { gx = ui::WIDTH.saturating_sub(grid_w + 4); }
            ui::draw_shadow(gx, dd_y, grid_w, grid_h, 3, 3);
            ui::fill_rounded_rect(gx, dd_y, grid_w, grid_h, 4, t.panel_bg);
            ui::stroke_rounded_rect(gx, dd_y, grid_w, grid_h, 4, t.border, 1);

            for i in 0..27 {
                let off = i as i8 - 12;
                let c = i % 3;
                let r = i / 3;
                let px = gx + 4 + c * 38;
                let py = dd_y + 4 + r * row_h;
                let fg_c = if off == self.tz_offset { t.border_hi } else { t.fg };
                let mut ob = [0u8; 8];
                let mut oi = 0;
                if off >= 0 { ob[oi]=b'+'; oi+=1; } else { ob[oi]=b'-'; oi+=1; }
                let a = off.abs() as u8;
                if a >= 10 { ob[oi]=b'0'+a/10; oi+=1; }
                ob[oi]=b'0'+a%10; oi+=1;
                if let Ok(s) = core::str::from_utf8(&ob[..oi]) {
                    ui::draw_string_share_tech(px, py + 12, s, fg_c);
                }
            }
        }

        // ── Drag ghost icon (6.5) ──────────────────────────────────────────
        self.draw_drag_ghost(&t);

        // ── Launcher toast (6.2) ────────────────────────────────────────────
        if self.launcher_msg_timer > 0 && self.launcher_msg_len > 0 {
            if let Ok(msg) = core::str::from_utf8(&self.launcher_msg[..self.launcher_msg_len]) {
                let toast_w = ui::measure_string_share_tech(msg) + 24;
                let toast_h = 28usize;
                let toast_x = (ui::WIDTH - toast_w) / 2;
                let toast_y = ui::HEIGHT - TASKBAR_H - toast_h - 12;
                ui::fill_rounded_rect(toast_x, toast_y, toast_w, toast_h, 6, t.panel_bg);
                ui::stroke_rounded_rect(toast_x, toast_y, toast_w, toast_h, 6, t.border_hi, 1);
                ui::draw_string_share_tech(toast_x + 12, toast_y + 8, msg, t.fg);
            }
        }

        // ── Cursor ─────────────────────────────────────────────────────────
        let cursor_col = if self.light_mode { 0x1a8aaa } else { t.border_hi };
        let mx = self.mouse_x;
        let my = self.mouse_y;
        // Arrow cursor: 12px tall, tapers from 8 wide to 1 wide
        for row in 0..12usize {
            if my + row >= ui::HEIGHT { break; }
            let cols = if row < 8 { 8 - row } else { 1 };
            for col in 0..cols {
                if mx + col < ui::WIDTH {
                    if col == 0 || row == 0 || col + 1 == cols {
                        // Outline: dark border for visibility on any background
                        ui::put_pixel(mx + col, my + row, 0x000000);
                    }
                }
            }
            let fill_cols = if row < 7 { 7 - row } else { 0 };
            for col in 1..fill_cols {
                if mx + col < ui::WIDTH {
                    ui::put_pixel(mx + col, my + row, cursor_col);
                }
            }
        }
    }

    fn draw_window(&self, w: &Window, idx: usize, t: &Theme) {
        let r   = 8usize;  // corner radius
        let bc  = if w.active { t.border_hi } else { t.border };

        // ── 1. Blurred soft shadow ─────────────────────────────────────────
        ui::draw_blur_shadow(w.x, w.y, w.w, w.h, 14, 8);

        // ── 2. Frosted glass body ──────────────────────────────────────────
        // Blur whatever is behind the window, then tint it dark
        ui::blur_region(w.x, w.y, w.w, w.h, 5);
        ui::fill_rounded_rect_alpha(w.x, w.y, w.w, w.h, r, t.panel_bg, 220);

        // ── 3. Titlebar gradient (top portion of panel) ───────────────────
        let tb_light = if self.light_mode { 0xd0eaf0 } else { 0x0f1e0f };
        ui::fill_gradient_v(w.x + 1, w.y + 1, w.w - 2, TITLEBAR_H - 1,
                             tb_light, t.panel_bg);

        // ── 4. Glow / border ──────────────────────────────────────────────
        if w.active {
            // Corner glow dots for active window
            let glow_c = t.border_hi;
            let g = 6usize;
            // Top-left, top-right, bottom-left, bottom-right
            for gd in [(w.x, w.y), (w.x + w.w.saturating_sub(g), w.y),
                       (w.x, w.y + w.h.saturating_sub(g)),
                       (w.x + w.w.saturating_sub(g), w.y + w.h.saturating_sub(g))] {
                ui::fill_rounded_rect(gd.0, gd.1, g, g, 2, glow_c);
            }
        }
        ui::stroke_rounded_rect(w.x, w.y, w.w, w.h, r, bc, 1);

        // ── 5. Titlebar separator ─────────────────────────────────────────
        ui::draw_separator_h(w.x + r, w.y + TITLEBAR_H, w.w - r * 2, bc);

        // ── 6. Title text ─────────────────────────────────────────────────
        let title_col = if w.active { t.fg } else { t.fg2 };
        ui::draw_string_share_tech(w.x + 14, w.y + TITLEBAR_H - 4, w.title, title_col);

        // ── 7. Window control circles ─────────────────────────────────────
        let cir_y  = w.y + TITLEBAR_H / 2;
        let cir_x0 = w.x + w.w.saturating_sub(52);
        let close_c = if w.active { t.border_hi } else { 0x2a3a2a };
        ui::fill_circle(cir_x0,      cir_y, 5, 0x1a2a1a);  // minimize
        ui::fill_circle(cir_x0 + 18, cir_y, 5, 0x1a2a1a);  // maximize
        ui::fill_circle(cir_x0 + 36, cir_y, 5, close_c);   // close
        // Inner dots for active state
        if w.active {
            ui::fill_circle(cir_x0,      cir_y, 2, t.fg_dim);
            ui::fill_circle(cir_x0 + 18, cir_y, 2, t.fg_dim);
            ui::fill_circle(cir_x0 + 36, cir_y, 2, t.border_hi);
        }

        // ── 8. Resize handle ─────────────────────────────────────────────
        let hx = w.x + w.w - 3; let hy = w.y + w.h - 3;
        for dot in [(0i32,0i32),(-5,0),(0,-5),(-5,-5),(0,-10),(-10,0),(-10,-5)] {
            let px = hx as isize + dot.0 as isize;
            let py = hy as isize + dot.1 as isize;
            if px >= 0 && py >= 0 {
                ui::put_pixel(px as usize, py as usize, t.fg_dim);
            }
        }

        // ── 9. Panel content ──────────────────────────────────────────────
        match idx {
            0 => self.draw_terminal(w, t),
            1 => self.draw_fs(w, t),
            2 => self.draw_ai(w, t),
            3 => self.draw_browser(w, t),
            _ => {}
        }
    }

    fn draw_terminal(&self, w: &Window, t: &Theme) {
        let left_x      = w.x + 14;
        let max_x       = w.x + w.w - 14;
        let content_top = w.y + TITLEBAR_H + 4;
        let sep_y        = w.y + w.h - LINE_H - 24; // larger input area
        let input_y      = sep_y + 20;              // clear separator by 6px
        let avail_h      = sep_y.saturating_sub(content_top + 4);

        // ── Measure wrapped heights bottom-up to find which entries fit ──────
        let mut heights = [0usize; TERM_LINES];
        for i in 0..TERM_LINES {
            let (ref lb, ll) = self.term_history[i];
            if ll == 0 { heights[i] = LINE_H; continue; }
            let s = core::str::from_utf8(&lb[..ll]).unwrap_or("");
            heights[i] = ui::measure_wrapped_height(left_x, s, left_x, max_x, LINE_H);
        }

        // Find start index: accumulate from bottom until we'd exceed avail_h
        let mut used = 0usize;
        let mut start = TERM_LINES;
        for i in (0..TERM_LINES).rev() {
            if used + heights[i] > avail_h { break; }
            used += heights[i];
            start = i;
        }

        // Draw visible entries
        let mut ly = sep_y - used; // bottom-anchor the history
        for i in start..TERM_LINES {
            let (ref lb, ll) = self.term_history[i];
            let s = if ll > 0 { core::str::from_utf8(&lb[..ll]).unwrap_or("") } else { "" };
            let color = if s.starts_with("Welcome") || s.starts_with("WELCOME") {
                t.fg
            } else if s.starts_with("[AI] >") {
                t.fg_dim
            } else {
                t.fg2
            };
            if ll > 0 {
                ly = ui::draw_string_wrapped(left_x, ly, s, color, left_x, max_x, LINE_H, sep_y);
            } else {
                ly += LINE_H;
            }
        }

        // Separator above input — faded line
        ui::draw_separator_h(w.x + 10, sep_y, w.w - 20, t.border);

        // Input prompt with word-wrap for long input
        ui::draw_string_share_tech(left_x, input_y, "> ", t.fg);
        let prompt_off = left_x + 18;
        if self.active_panel == PANEL_TERMINAL && self.term_len > 0 {
            let s = core::str::from_utf8(&self.term_buffer[..self.term_len]).unwrap_or("");
            ui::draw_string_wrapped(prompt_off, input_y, s, t.fg, prompt_off, max_x, LINE_H, input_y + LINE_H * 2);
        }
        if self.active_panel == PANEL_TERMINAL {
            // cursor — vertical bar at accurate position after typed text
            let typed = core::str::from_utf8(&self.term_buffer[..self.term_len]).unwrap_or("");
            let cur_x = (prompt_off + ui::measure_string_share_tech(typed)).min(max_x - 4);
            ui::fill_rect(cur_x, input_y - 14, 2, LINE_H, t.fg);
        }
    }

    fn draw_fs(&self, w: &Window, t: &Theme) {
        let left_x = w.x + 14;
        let max_x  = w.x + w.w - 14;

        // Header: current path
        ui::fill_rect_alpha(w.x + 1, w.y + TITLEBAR_H + 1, w.w - 2, LINE_H + 4, t.bar_bg, 160);
        let mut path_buf = [0u8; 80];
        path_buf[..4].copy_from_slice(b"  / ");
        let mut pi = 4;
        for b in &self.fs_path[..self.fs_path_len] {
            if pi < 79 { path_buf[pi] = *b; pi += 1; }
        }
        if let Ok(s) = core::str::from_utf8(&path_buf[..pi]) {
            ui::draw_string_share_tech(left_x, w.y + TITLEBAR_H + 8, s, t.fg2);
        }
        ui::draw_separator_h(w.x + 10, w.y + TITLEBAR_H + LINE_H + 4, w.w - 20, t.border);

        let content_top    = w.y + TITLEBAR_H + LINE_H + 10;
        let content_bottom = w.y + w.h - 10;
        let mut ly = content_top;

        // ".." navigation if not at root
        let at_root = self.fs_path_len <= 1;
        if !at_root {
            let is_sel = self.fs_selected == 0;
            if is_sel { ui::fill_rounded_rect(w.x+6, ly-1, w.w-12, LINE_H+2, 2, t.bar_bg); }
            ui::draw_string_share_tech(left_x, ly, "[D]", t.border_hi);
            ui::draw_string_share_tech(left_x + 30, ly, "..", if is_sel { t.fg } else { t.fg2 });
            ly += LINE_H + 2;
        }

        for i in self.fs_scroll..self.fs_count {
            if ly + LINE_H > content_bottom { break; }
            let entry = &self.fs_entries[i];
            let sel_idx = if at_root { i } else { i + 1 };
            let is_sel = self.fs_selected == sel_idx;
            if is_sel { ui::fill_rounded_rect(w.x+6, ly-1, w.w-12, LINE_H+2, 2, t.bar_bg); }
            let icon_col = if entry.is_dir != 0 { t.border_hi } else { t.fg_dim };
            ui::draw_string_share_tech(left_x, ly, if entry.is_dir != 0 { "[D]" } else { "[F]" }, icon_col);
            let name = entry.name_str();
            ui::draw_string_clipped(left_x + 30, ly, name, if entry.is_dir != 0 { t.fg } else { t.fg2 }, max_x.saturating_sub(60));
            // Right-align size for files
            if entry.is_dir == 0 {
                let sz = entry.size;
                let mut sb = [0u8; 12]; let mut si = 0;
                let (sv, unit) = if sz >= 1024*1024 { (sz/1048576, "MB") }
                                  else if sz >= 1024 { (sz/1024, "KB") }
                                  else { (sz, "B") };
                if sv >= 1000 { sb[si]=b'0'+(sv/1000%10) as u8; si+=1; }
                if sv >= 100  { sb[si]=b'0'+(sv/100%10) as u8; si+=1; }
                if sv >= 10   { sb[si]=b'0'+(sv/10%10) as u8; si+=1; }
                sb[si]=b'0'+(sv%10) as u8; si+=1;
                for ub in unit.bytes() { if si<12 { sb[si]=ub; si+=1; } }
                if let Ok(s) = core::str::from_utf8(&sb[..si]) {
                    let sw = ui::measure_string_share_tech(s);
                    if max_x > sw + 4 { ui::draw_string_share_tech(max_x - sw, ly, s, t.fg_dim); }
                }
            }
            ly += LINE_H + 2;
        }

        // Empty dir message
        if self.fs_count == 0 && ly < content_bottom {
            ui::draw_string_share_tech(left_x, ly, "(empty directory)", t.fg_dim);
        }
    }

    fn draw_ai(&self, w: &Window, t: &Theme) {
        let left_x      = w.x + 14;
        let max_x       = w.x + w.w - 14;
        let content_top = w.y + TITLEBAR_H + 6;
        let sep_y       = w.y + w.h - LINE_H - 24; // larger input area
        let input_y     = sep_y + 20;              // clear separator by 6px
        let avail_h     = sep_y.saturating_sub(content_top + 4);

        // ── Measure each entry's wrapped height ───────────────────────────
        // For AI responses the text starts after "[AI]: " (~42px); for user
        // queries after "you: " (~35px).  We approximate by measuring from
        // left_x which is conservative (slightly over-estimates height).
        let mut heights = [0usize; AI_LINES];
        for i in 0..AI_LINES {
            let (ref buf, len, is_resp) = self.ai_history[i];
            if len == 0 { heights[i] = LINE_H; continue; }
            let s = core::str::from_utf8(&buf[..len]).unwrap_or("");
            let prefix_w = if is_resp { 42usize } else { 35usize }; // "[AI]: " / "you: "
            let text_x_start = left_x + prefix_w;
            heights[i] = ui::measure_wrapped_height(text_x_start, s, left_x, max_x, LINE_H);
        }

        // ── Find first visible entry (bottom-anchored) ────────────────────
        let mut used = 0usize;
        if self.llm_response_active { used += LINE_H; } // reserve for "thinking..."
        let mut start = AI_LINES;
        for i in (0..AI_LINES).rev() {
            if used + heights[i] > avail_h { break; }
            used += heights[i];
            start = i;
        }

        // ── Draw entries ──────────────────────────────────────────────────
        let mut ly = sep_y - used;
        for i in start..AI_LINES {
            let (ref buf, len, is_resp) = self.ai_history[i];
            if len == 0 { ly += LINE_H; continue; }
            let s = core::str::from_utf8(&buf[..len]).unwrap_or("");

            if is_resp {
                // Draw "[AI]: " prefix then wrapped response
                let mut cx = left_x;
                for byte in b"[AI]: ".iter() {
                    cx += ui::draw_char_share_tech(cx, ly, *byte, t.fg2);
                }
                ly = ui::draw_string_wrapped(cx, ly, s, t.fg, left_x, max_x, LINE_H, sep_y);
            } else {
                // Draw "you: " prefix then wrapped query
                let mut cx = left_x;
                for byte in b"you: ".iter() {
                    cx += ui::draw_char_share_tech(cx, ly, *byte, t.fg_dim);
                }
                ly = ui::draw_string_wrapped(cx, ly, s, t.fg2, left_x, max_x, LINE_H, sep_y);
            }
        }

        // Streaming partial response or "thinking..." indicator
        if self.llm_response_active && ly + LINE_H <= sep_y {
            if self.llm_len > 0 {
                // Show partial streaming text as it arrives
                let partial = core::str::from_utf8(&self.llm_buffer[..self.llm_len])
                    .unwrap_or("...");
                let mut cx = left_x;
                for byte in b"[AI]: ".iter() {
                    cx += ui::draw_char_share_tech(cx, ly, *byte, t.fg2);
                }
                let _ = ui::draw_string_wrapped(cx, ly, partial, t.fg, left_x, max_x, LINE_H, sep_y);
            } else {
                ui::draw_string_share_tech(left_x, ly, "thinking...", t.fg_dim);
            }
        }

        // ── Separator ─────────────────────────────────────────────────────
        ui::draw_separator_h(w.x + 10, sep_y, w.w - 20, t.border);

        // ── Input area ────────────────────────────────────────────────────
        ui::draw_string_share_tech(left_x, input_y, ">> ", t.fg2);
        let prompt_off = left_x + 27;
        if self.active_panel == PANEL_AI && self.term_len > 0 {
            let s = core::str::from_utf8(&self.term_buffer[..self.term_len]).unwrap_or("");
            ui::draw_string_wrapped(prompt_off, input_y, s, t.fg, prompt_off, max_x, LINE_H, input_y + LINE_H * 2);
            // cursor — vertical bar at accurate position
            let cur_x = (prompt_off + ui::measure_string_share_tech(s)).min(max_x - 4);
            ui::fill_rect(cur_x, input_y - 14, 2, LINE_H, t.fg);
        } else if self.active_panel == PANEL_AI {
            ui::draw_string_share_tech(prompt_off, input_y, "Type query...", t.fg_dim);
        }
    }

    fn draw_browser(&self, w: &Window, t: &Theme) {
        let left_x = w.x + 14;
        let max_x  = w.x + w.w - 14;

        // URL bar
        let bar_y = w.y + TITLEBAR_H + 6;
        let bar_h = 22;
        let bar_x = w.x + 10;
        let bar_w = w.w - 20;
        let bar_bg = if self.browser_url_editing { t.bar_bg } else { 0x060e06 };
        ui::fill_rounded_rect(bar_x, bar_y, bar_w, bar_h, 3, bar_bg);
        ui::stroke_rounded_rect(bar_x, bar_y, bar_w, bar_h, 3,
            if self.browser_url_editing { t.border_hi } else { t.border }, 1);
        ui::draw_string_share_tech(bar_x + 6, bar_y + bar_h - 6, "URL:", t.fg_dim);
        let url_str = core::str::from_utf8(&self.browser_url[..self.browser_url_len]).unwrap_or("");
        if self.browser_url_len > 0 {
            ui::draw_string_clipped(bar_x + 40, bar_y + bar_h - 6, url_str, t.fg, max_x);
        } else {
            ui::draw_string_share_tech(bar_x + 40, bar_y + bar_h - 6, "http://...", t.fg_dim);
        }
        if self.browser_url_editing {
            let cx = bar_x + 40 + ui::measure_string_share_tech(url_str);
            ui::fill_rect(cx.min(max_x - 2), bar_y + 5, 2, 12, t.fg);
        }

        // Status separator
        let sep_y = bar_y + bar_h + 4;
        ui::draw_separator_h(w.x + 10, sep_y, w.w - 20, t.border);

        // Status / loading
        if self.browser_loading {
            ui::draw_string_share_tech(left_x, sep_y + 4, "Fetching... (may take ~15s)", t.fg_dim);
        }

        // Content
        let content_top = sep_y + 20;
        let save_btn_h: usize = 20;
        let save_btn_w: usize = 60;
        let save_btn_x = w.x + w.w.saturating_sub(save_btn_w + 10);
        let save_btn_y = w.y + w.h.saturating_sub(save_btn_h + 8);
        let content_bottom = save_btn_y.saturating_sub(4);
        if self.browser_content_len > 0 && content_top < content_bottom {
            if let Ok(s) = core::str::from_utf8(&self.browser_content[..self.browser_content_len]) {
                ui::draw_string_wrapped(left_x, content_top, s, t.fg2, left_x, max_x, LINE_H, content_bottom);
            }
        } else if self.browser_content_len == 0 && !self.browser_loading {
            ui::draw_string_share_tech(left_x, content_top, "Click URL bar and type address, press Enter to fetch.", t.fg_dim);
            ui::draw_string_share_tech(left_x, content_top + LINE_H + 4, "Note: only http:// supported (HTTPS pending TLS).", t.fg_dim);
        }

        // Save button (bottom-right corner)
        if self.browser_content_len > 0 {
            ui::fill_rounded_rect(save_btn_x, save_btn_y, save_btn_w, save_btn_h, 3, t.border_hi);
            ui::draw_string_share_tech(save_btn_x + 10, save_btn_y + save_btn_h - 6, "[ Save ]", t.tab_on_fg);
        } else {
            ui::fill_rounded_rect(save_btn_x, save_btn_y, save_btn_w, save_btn_h, 3, t.border);
            ui::draw_string_share_tech(save_btn_x + 10, save_btn_y + save_btn_h - 6, "[ Save ]", t.fg_dim);
        }

        // Save feedback message
        if self.browser_save_msg_len > 0 {
            if let Ok(s) = core::str::from_utf8(&self.browser_save_msg[..self.browser_save_msg_len]) {
                ui::draw_string_share_tech(left_x, save_btn_y + 4, s, t.fg2);
            }
        }
    }

    fn draw_desktop_icons(&self, t: &Theme) {
        if self.desktop_icon_count == 0 { return; }
        let iw   = crate::icons::FOLDER_ICON_W; // 56
        let ih   = crate::icons::FOLDER_ICON_H; // 48
        let row_h = ih + 24; // icon + label + padding
        let ix   = ui::WIDTH.saturating_sub(iw + 16); // right-aligned, 16px margin
        let start_y = TOPBAR_H + 16;

        for i in 0..self.desktop_icon_count.min(10) {
            let entry = &self.desktop_icons[i];
            let iy = start_y + i * row_h;
            if iy + row_h > ui::HEIGHT - TASKBAR_H - 8 { break; }
            let is_dir = entry.is_dir != 0;

            if is_dir {
                // Blit the real Generic_Folder.png icon
                ui::blit_folder_icon(ix, iy);
            } else {
                // File — dog-eared page
                let fc = t.fg2;
                ui::fill_rect(ix + 8, iy + 4, 36, 40, fc);
                ui::fill_rect(ix + 26, iy + 4, 18, 16, t.panel_bg);
                for k in 0..18usize {
                    let px = ix + 26 + k;
                    let py = iy + 4 + (17 - k);
                    if px < ui::WIDTH && py < ui::HEIGHT { ui::put_pixel(px, py, fc); }
                }
            }

            // Label centred below icon
            let name = entry.name_str();
            let show_len = name.len().min(9);
            if let Ok(s) = core::str::from_utf8(&name.as_bytes()[..show_len]) {
                let sw = ui::measure_string_share_tech(s);
                let lx = if sw < iw { ix + (iw - sw) / 2 } else { ix };
                ui::draw_string_share_tech(lx, iy + ih + 6, s, t.fg_dim);
            }
        }
    }

    // ── App launcher helpers (6.2) ────────────────────────────────────────────

    /// Icon hit rectangle for desktop icon at index `i`.
    /// Returns (ix, iy, iw, ih) in screen coordinates.
    fn icon_rect(i: usize) -> (usize, usize, usize, usize) {
        let iw   = crate::icons::FOLDER_ICON_W;
        let ih   = crate::icons::FOLDER_ICON_H;
        let row_h = ih + 24;
        let ix   = ui::WIDTH.saturating_sub(iw + 16);
        let iy   = 38 + 16 + i * row_h; // TOPBAR_H + 16
        (ix, iy, iw, ih)
    }

    /// Exec the desktop icon at index `i` by passing its path to SYS_EXEC.
    /// The kernel reads the ELF from FAT32 and spawns a new EL0 task.
    fn launch_icon(&mut self, i: usize) {
        // Debounce: ignore launches within ~600 ms of the last one
        if self.last_launch_tick > 0 { return; }
        self.last_launch_tick = 60; // 60 ticks ≈ 600 ms

        // Route through the terminal panel instead of direct SYS_EXEC.
        // Direct SYS_EXEC spawns a second GUI process that fights for the
        // framebuffer (its login screen vs. our desktop → flickering loop).
        // Inserting `exec /name` into the terminal lets the user review and
        // press Enter, and the kernel EL0 exec replaces the terminal task —
        // only one GUI process ever owns the framebuffer at a time.
        let entry = &self.desktop_icons[i];
        let name_len = entry.name.iter().position(|&b| b == 0).unwrap_or(0);
        let n = name_len.min(120);
        let mut cmd = [0u8; 128];
        cmd[0] = b'e'; cmd[1] = b'x'; cmd[2] = b'e'; cmd[3] = b'c';
        cmd[4] = b' '; cmd[5] = b'/';
        cmd[6..6 + n].copy_from_slice(&entry.name[..n]);
        let cmd_len = 6 + n;
        self.term_buffer[..cmd_len].copy_from_slice(&cmd[..cmd_len]);
        self.term_len = cmd_len;
        self.bring_to_front(PANEL_TERMINAL);
        self.switch_panel(PANEL_TERMINAL);
        self.set_launcher_msg(b"Ready -- press Enter to run");
    }

    fn set_launcher_msg(&mut self, msg: &[u8]) {
        let n = msg.len().min(64);
        self.launcher_msg[..n].copy_from_slice(&msg[..n]);
        self.launcher_msg_len = n;
        self.launcher_msg_timer = 120;
    }

    /// Draw the ghost icon at drag position (6.5).
    fn draw_drag_ghost(&self, t: &Theme) {
        if let Some(i) = self.dragging_icon {
            if i >= self.desktop_icon_count { return; }
            let entry = &self.desktop_icons[i];
            let is_dir = entry.is_dir != 0;
            let x = self.drag_icon_x.saturating_sub(28);
            let y = self.drag_icon_y.saturating_sub(24);
            // Dim outline ghost
            if is_dir {
                ui::blit_folder_icon(x, y);
            } else {
                let fc = ui::blend(t.panel_bg, t.fg2, 128);
                ui::fill_rect(x + 8, y + 4, 36, 40, fc);
            }
            // Drop-zone hint: highlight window under cursor
            let cx = self.drag_icon_x;
            let cy = self.drag_icon_y;
            for k in (0..self.draw_order.len()).rev() {
                let wi = self.draw_order[k];
                let w = &self.windows[wi];
                if !w.visible || w.minimized { continue; }
                if cx >= w.x && cx < w.x + w.w && cy >= w.y && cy < w.y + w.h {
                    ui::draw_rect(w.x, w.y, w.w, w.h, t.border_hi, 2);
                    break;
                }
            }
        }
    }

    // ── Mouse input ───────────────────────────────────────────────────────────

    pub fn handle_mouse(&mut self, x: usize, y: usize, down: bool) {
        let prev_down = self.mouse_down;
        self.mouse_x = x;
        self.mouse_y = y;
        self.mouse_down = down;

        let ty = ui::HEIGHT - TASKBAR_H;

        // ── Resize drag ────────────────────────────────────────────────────
        if down {
            if let Some(idx) = self.resizing_win {
                let w = &mut self.windows[idx];
                let dw = x as isize - self.resize_origin_x;
                let dh = y as isize - self.resize_origin_y;
                let new_w = (self.resize_origin_w as isize + dw).max(220) as usize;
                let new_h = (self.resize_origin_h as isize + dh).max(120) as usize;
                w.w = new_w.min(ui::WIDTH.saturating_sub(w.x + 2));
                w.h = new_h.min((ui::HEIGHT - TASKBAR_H).saturating_sub(w.y + 2));
                return;
            }
            // ── Icon drag update (6.5) ────────────────────────────────────
            if let Some(_) = self.dragging_icon {
                self.drag_icon_x = x;
                self.drag_icon_y = y;
                return;
            }
            if let Some(_) = self.icon_pressed {
                let dx = (x as isize - self.icon_press_x as isize).unsigned_abs();
                let dy = (y as isize - self.icon_press_y as isize).unsigned_abs();
                if dx > 4 || dy > 4 {
                    self.dragging_icon = self.icon_pressed;
                    self.icon_pressed = None;
                    self.drag_icon_x = x;
                    self.drag_icon_y = y;
                    return;
                }
            }
        }

        if !prev_down && down {
            // ── Dropdowns Overlay hit test ────────────────────────────────
            if self.tz_dropdown_open {
                let row_h = 16;
                let dd_y = 32;
                let grid_w = 120;
                let grid_h = 9 * row_h + 8;
                let mut gx = ui::WIDTH - 144;
                if gx + grid_w > ui::WIDTH { gx = ui::WIDTH - grid_w - 4; }

                if x >= gx && x < gx + grid_w && y >= dd_y && y < dd_y + grid_h {
                    let rx = x.saturating_sub(gx + 4);
                    let ry = y.saturating_sub(dd_y + 4);
                    let c = rx / 38;
                    let r = ry / row_h;
                    if c < 3 && r < 9 {
                        let idx = r * 3 + c;
                        if idx < 27 {
                            self.tz_offset = idx as i8 - 12;
                            self.tz_dropdown_open = false;
                            return;
                        }
                    }
                }
                self.tz_dropdown_open = false;
            }

            // ── Desktop icon hit test (6.2 launcher / 6.5 drag) ──────────
            {
                let iw = crate::icons::FOLDER_ICON_W;
                let ih = crate::icons::FOLDER_ICON_H;
                let row_h = ih + 24;
                let ix = ui::WIDTH.saturating_sub(iw + 16);
                let start_y = TOPBAR_H + 16;
                let mut icon_hit = false;
                for i in 0..self.desktop_icon_count.min(10) {
                    let iy = start_y + i * row_h;
                    if iy + row_h > ui::HEIGHT - TASKBAR_H - 8 { break; }
                    if x >= ix && x < ix + iw && y >= iy && y < iy + ih + 24 {
                        let is_dir = self.desktop_icons[i].is_dir != 0;
                        if is_dir {
                            // Navigate FS panel to this folder
                            let name_len = self.desktop_icons[i].name.iter()
                                .position(|&b| b == 0).unwrap_or(0);
                            let mut name_buf = [0u8; 64];
                            let n = name_len.min(63);
                            name_buf[..n].copy_from_slice(&self.desktop_icons[i].name[..n]);
                            if let Ok(name) = core::str::from_utf8(&name_buf[..n]) {
                                self.fs_navigate(true, name);
                                self.windows[PANEL_FS].visible = true;
                                self.windows[PANEL_FS].minimized = false;
                                self.switch_panel(PANEL_FS);
                            }
                        } else {
                            // Begin potential drag or click — decide on release
                            self.icon_pressed = Some(i);
                            self.icon_press_x = x;
                            self.icon_press_y = y;
                        }
                        icon_hit = true;
                        break;
                    }
                }
                if icon_hit { return; }
            }

            // ── Topbar hit tests ───────────────────────────────────────────
            if y < TOPBAR_H {
                let tz_x = ui::WIDTH.saturating_sub(144);
                if x >= tz_x && x < tz_x + 70 && y >= 12 && y <= 32 {
                    self.tz_dropdown_open = !self.tz_dropdown_open;
                    return;
                }
                let t_x = ui::WIDTH.saturating_sub(62);
                if x >= t_x && x < t_x + 46 && y >= 12 && y <= 32 {
                    self.time_24hr = !self.time_24hr;
                    return;
                }
                return;
            }

            // ── Taskbar hit tests ──────────────────────────────────────────
            if y >= ty {
                let btn_y = ty + 7;
                let btn_h = btn_y + 28;

                const HIT_TABS: [(&str, usize, usize, usize); 5] = [
                    ("ENGAGE",   10,  80,  99),
                    ("TERMINAL", 100, 92,  PANEL_TERMINAL),
                    ("FILES",    202, 72,  PANEL_FS),
                    ("AI_LINK",  284, 88,  PANEL_AI),
                    ("BROWSER",  382, 94,  PANEL_BROWSER),
                ];
                let mut tab_hit = false;
                for &(_, tx, tw, panel) in &HIT_TABS {
                    if x >= tx && x < tx + tw && y >= btn_y && y < btn_h {
                        let target = if panel == 99 { PANEL_AI } else { panel };
                        // Toggle: if this panel's window is visible and active, minimize it
                        if self.active_panel == target
                            && target < self.windows.len()
                            && !self.windows[target].minimized
                            && self.windows[target].visible {
                            self.windows[target].minimized = true;
                        } else {
                            if target < self.windows.len() {
                                self.windows[target].visible = true;
                                self.windows[target].minimized = false;
                            }
                            self.switch_panel(target);
                        }
                        tab_hit = true;
                        return;
                    }
                }
                let _ = tab_hit;

                // LIGHT/DARK toggle
                let toggle_x = ui::WIDTH - 205;
                if x >= toggle_x && x < toggle_x + 66 && y >= btn_y && y < btn_h {
                    self.light_mode = !self.light_mode;
                    return;
                }
                return;
            }

            // ── Window hit tests (topmost first = reverse draw_order) ─────
            let mut clicked = None;
            for k in (0..self.draw_order.len()).rev() {
                let i = self.draw_order[k];
                let w = &self.windows[i];
                if !w.visible || w.minimized { continue; }
                if x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h {
                    clicked = Some(i);
                    break;
                }
            }

            if let Some(idx) = clicked {
                self.bring_to_front(idx);
                self.switch_panel(idx);

                // ── FS panel click in content area ────────────────────────
                if idx == PANEL_FS {
                    let w = &self.windows[idx];
                    let content_top = w.y + TITLEBAR_H + LINE_H + 10;
                    if y >= content_top {
                        let rel = (y - content_top) / (LINE_H + 2);
                        let at_root = self.fs_path_len <= 1;
                        if !at_root && rel == 0 {
                            // Clicked ".."
                            self.fs_navigate(true, "..");
                        } else {
                            let entry_idx = if at_root { rel } else { rel.saturating_sub(1) } + self.fs_scroll;
                            if entry_idx < self.fs_count {
                                let is_dir = self.fs_entries[entry_idx].is_dir != 0;
                                let name_len = self.fs_entries[entry_idx].name.iter().position(|&b| b == 0).unwrap_or(0);
                                let mut name_buf = [0u8; 64];
                                let copy = name_len.min(63);
                                name_buf[..copy].copy_from_slice(&self.fs_entries[entry_idx].name[..copy]);
                                if let Ok(name) = core::str::from_utf8(&name_buf[..copy]) {
                                    if is_dir {
                                        self.fs_navigate(true, name);
                                    }
                                }
                                self.fs_selected = rel + if !at_root { 0 } else { 0 };
                            }
                        }
                        return;
                    }
                }

                // ── Browser URL bar + Save button click ──────────────────
                if idx == PANEL_BROWSER {
                    let w = &self.windows[idx];
                    let bar_y = w.y + TITLEBAR_H + 6;
                    let bar_h = 22;
                    let bar_x = w.x + 10;
                    let bar_w = w.w - 20;
                    if x >= bar_x && x < bar_x + bar_w && y >= bar_y && y < bar_y + bar_h {
                        self.browser_url_editing = true;
                        return;
                    }
                    // Save button hit region (bottom-right)
                    let save_btn_h: usize = 20;
                    let save_btn_w: usize = 60;
                    let save_btn_x = w.x + w.w.saturating_sub(save_btn_w + 10);
                    let save_btn_y = w.y + w.h.saturating_sub(save_btn_h + 8);
                    if x >= save_btn_x && x < save_btn_x + save_btn_w
                        && y >= save_btn_y && y < save_btn_y + save_btn_h
                    {
                        self.save_browser_content();
                        return;
                    }
                }

                let w = &mut self.windows[idx];

                // ── Resize corner (bottom-right 20×20 zone) — checked first ──
                let corner_x = w.x + w.w.saturating_sub(20);
                let corner_y = w.y + w.h.saturating_sub(20);
                if x >= corner_x && y >= corner_y && !w.maximized {
                    self.resizing_win    = Some(idx);
                    self.resize_origin_x = x as isize;
                    self.resize_origin_y = y as isize;
                    self.resize_origin_w = w.w;
                    self.resize_origin_h = w.h;
                } else if y >= w.y && y < w.y + TITLEBAR_H {
                    let cir_x0 = w.x + w.w.saturating_sub(54);
                    if x >= cir_x0 + 28 && x < cir_x0 + 46 {
                        // Close circle
                        w.visible = false;
                    } else if x >= cir_x0 + 12 && x < cir_x0 + 28 {
                        // Maximize circle
                        if w.maximized {
                            w.maximized = false; w.w = w.max_w; w.h = w.max_h;
                        } else {
                            w.maximized = true; w.max_w = w.w; w.max_h = w.h;
                            w.w = ui::WIDTH - 2; w.h = ui::HEIGHT - TOPBAR_H - TASKBAR_H - 2;
                            w.x = 1; w.y = TOPBAR_H + 1;
                        }
                    } else if x >= cir_x0 && x < cir_x0 + 14 {
                        // Minimize circle
                        w.minimized = true;
                    } else {
                        // Drag start
                        self.dragging_win = Some(idx);
                        self.drag_offset_x = x as isize - w.x as isize;
                        self.drag_offset_y = y as isize - w.y as isize;
                    }
                }
            }
        } else if prev_down && !down {
            self.dragging_win = None;
            self.resizing_win = None;

            // ── Icon click / drop handling (6.2 / 6.5) ─────────────────
            if let Some(i) = self.icon_pressed {
                // Click without drag → launch ELF
                if self.desktop_icons[i].is_dir == 0 {
                    self.launch_icon(i);
                }
                self.icon_pressed = None;
            }
            if let Some(i) = self.dragging_icon {
                // Drop → check target window
                for k in (0..self.draw_order.len()).rev() {
                    let wi = self.draw_order[k];
                    let w = &self.windows[wi];
                    if !w.visible || w.minimized { continue; }
                    if x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h {
                        // Build command from icon name
                        let entry = &self.desktop_icons[i];
                        let name_len = entry.name.iter().position(|&b| b == 0).unwrap_or(0);
                        let n = name_len.min(60);
                        if wi == PANEL_TERMINAL {
                            // Insert `exec /name` into terminal input
                            let mut cmd = [0u8; 128];
                            cmd[0] = b'e'; cmd[1] = b'x'; cmd[2] = b'e'; cmd[3] = b'c';
                            cmd[4] = b' '; cmd[5] = b'/';
                            cmd[6..6 + n].copy_from_slice(&entry.name[..n]);
                            let cmd_len = 6 + n;
                            self.term_buffer[..cmd_len].copy_from_slice(&cmd[..cmd_len]);
                            self.term_len = cmd_len;
                            self.bring_to_front(PANEL_TERMINAL);
                            self.switch_panel(PANEL_TERMINAL);
                        } else if wi == PANEL_AI {
                            // Send a cat/summarise query to AI
                            let mut cmd = [0u8; 128];
                            let pfx = b"describe /";
                            cmd[..pfx.len()].copy_from_slice(pfx);
                            cmd[pfx.len()..pfx.len() + n].copy_from_slice(&entry.name[..n]);
                            let cmd_len = pfx.len() + n;
                            self.dispatch_ai(&cmd[..cmd_len]);
                            self.bring_to_front(PANEL_AI);
                            self.switch_panel(PANEL_AI);
                        }
                        break;
                    }
                }
                self.dragging_icon = None;
            }
        }

        if down {
            if let Some(idx) = self.dragging_win {
                let w = &mut self.windows[idx];
                if !w.maximized {
                    w.x = (x as isize - self.drag_offset_x)
                        .clamp(0, ui::WIDTH as isize - w.w as isize) as usize;
                    w.y = (y as isize - self.drag_offset_y)
                        .clamp(TOPBAR_H as isize, (ui::HEIGHT - TASKBAR_H) as isize - w.h as isize) as usize;
                }
            }
        }
    }
}
