//! Aeglos WASM runtime — MVP bytecode interpreter.
//!
//! Supports the WebAssembly 1.0 (MVP) binary format.
//! Floats (f32/f64) are parsed but trap at execution time.
//!
//! Host imports (module "aeglos"):
//!   - "log"      (ptr: i32, len: i32)          → writes to UART
//!   - "send_ipc" (tid: i32, msg_ptr: i32) → i32  → SYS_SEND
//!   - "recv_ipc" (msg_ptr: i32)           → i32  → SYS_RECV (non-blocking peek)
//!
//! Usage:
//!   let module = wasm::load(bytes)?;
//!   let result = module.call_export("main", &[])?;

extern crate alloc;
use alloc::vec::Vec;
use alloc::string::String;

pub mod memory;
mod interp;

pub use memory::LinearMemory;

// ─── Capability constants (re-exported from stub) ─────────────────────────────
pub const WASM_LINEAR_MEM_SIZE: usize = memory::WASM_MAX_PAGES * memory::WASM_PAGE_SIZE;
pub const WASM_DEFAULT_CAPS: u64 = crate::syscall::CAP_SEND | crate::syscall::CAP_RECV;

// ─── Value types ─────────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum ValType { I32, I64, F32, F64 }

impl ValType {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x7F => Some(ValType::I32),
            0x7E => Some(ValType::I64),
            0x7D => Some(ValType::F32),
            0x7C => Some(ValType::F64),
            _ => None,
        }
    }
}

// ─── Section-parsed structures ────────────────────────────────────────────────

#[derive(Clone)]
pub struct FuncType {
    pub params:  Vec<ValType>,
    pub results: Vec<ValType>,
}

#[derive(Clone)]
pub struct Import {
    pub module: String,
    pub name:   String,
    pub kind:   ImportKind,
}

#[derive(Clone)]
pub enum ImportKind {
    Func(u32),   // type index
    Table,
    Memory(u32, Option<u32>), // min, max pages
    Global,
}

#[derive(Clone)]
pub struct Export {
    pub name: String,
    pub kind: ExportKind,
    pub idx:  u32,
}

#[derive(Clone, PartialEq)]
pub enum ExportKind { Func, Table, Memory, Global }

#[derive(Clone)]
pub struct GlobalDef {
    pub val_type: ValType,
    pub mutable:  bool,
    pub init_val: u64,   // constant-folded init expression
}

#[derive(Clone)]
pub struct FuncBody {
    pub locals: Vec<(u32, ValType)>, // (count, type)
    pub code:   Vec<u8>,             // raw bytecode
}

#[derive(Clone)]
pub struct DataSegment {
    pub mem_idx: u32,
    pub offset:  u32,   // constant i32.const expression
    pub data:    Vec<u8>,
}

#[derive(Clone)]
pub struct ElemSegment {
    pub table_idx: u32,
    pub offset:    u32,
    pub func_idxs: Vec<u32>,
}

// ─── Parsed module ────────────────────────────────────────────────────────────

pub struct WasmModule {
    pub types:      Vec<FuncType>,
    pub imports:    Vec<Import>,
    pub func_types: Vec<u32>,      // type index for each local function
    pub table_size: u32,           // initial funcref table size
    pub mem_min:    u32,           // initial memory pages
    pub mem_max:    Option<u32>,
    pub globals:    Vec<GlobalDef>,
    pub exports:    Vec<Export>,
    pub start:      Option<u32>,
    pub elem:       Vec<ElemSegment>,
    pub code:       Vec<FuncBody>,
    pub data:       Vec<DataSegment>,
    /// Capability bitmask when spawning as a task.
    pub caps: u64,
}

impl WasmModule {
    /// Number of imported functions.
    pub fn import_func_count(&self) -> usize {
        self.imports.iter().filter(|i| matches!(i.kind, ImportKind::Func(_))).count()
    }

    /// Total function count (imports + local).
    pub fn total_func_count(&self) -> usize {
        self.import_func_count() + self.code.len()
    }

    /// Type of function at absolute index (imports + local).
    pub fn func_type(&self, idx: u32) -> Option<&FuncType> {
        let n_imports = self.import_func_count();
        if (idx as usize) < n_imports {
            // Find the idx-th imported function
            let mut fi = 0u32;
            for imp in &self.imports {
                if let ImportKind::Func(ti) = imp.kind {
                    if fi == idx { return self.types.get(ti as usize); }
                    fi += 1;
                }
            }
            None
        } else {
            let local = idx as usize - n_imports;
            let ti = *self.func_types.get(local)? as usize;
            self.types.get(ti)
        }
    }

    /// Find an exported function by name.
    pub fn find_export(&self, name: &str) -> Option<u32> {
        self.exports.iter().find(|e| e.kind == ExportKind::Func && e.name == name)
            .map(|e| e.idx)
    }

    /// Execute an exported function by name.
    pub fn call_export(&self, name: &str, args: &[u64]) -> Result<Vec<u64>, WasmError> {
        let idx = self.find_export(name).ok_or(WasmError::ExportNotFound)?;
        let mut rt = interp::Interpreter::new(self)?;
        rt.call(idx, args)
    }
}

// ─── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum WasmError {
    InvalidMagic,
    InvalidSection,
    UnsupportedSection(u8),
    OutOfMemory,
    ExportNotFound,
    TypeMismatch,
    Trap(&'static str),
    MemoryFault(u32),
    DivisionByZero,
    StackOverflow,
    Unimplemented(&'static str),
}

impl core::fmt::Display for WasmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

// ─── LEB128 decoder ──────────────────────────────────────────────────────────

/// Decode unsigned LEB128. Returns (value, bytes_consumed).
pub(crate) fn read_leb128_u32(data: &[u8], pos: usize) -> Option<(u32, usize)> {
    let mut result = 0u32;
    let mut shift  = 0u32;
    let mut i = pos;
    loop {
        if i >= data.len() { return None; }
        let b = data[i];
        i += 1;
        result |= ((b & 0x7F) as u32) << shift;
        shift  += 7;
        if b & 0x80 == 0 { break; }
        if shift >= 35  { return None; }
    }
    Some((result, i - pos))
}

/// Decode signed LEB128 (i32).
pub(crate) fn read_leb128_i32(data: &[u8], pos: usize) -> Option<(i32, usize)> {
    let mut result = 0i32;
    let mut shift  = 0u32;
    let mut i = pos;
    loop {
        if i >= data.len() { return None; }
        let b = data[i];
        i += 1;
        result |= ((b & 0x7F) as i32) << shift;
        shift  += 7;
        if b & 0x80 == 0 {
            if shift < 32 && (b & 0x40) != 0 {
                result |= -(1i32 << shift);
            }
            break;
        }
        if shift >= 35 { return None; }
    }
    Some((result, i - pos))
}

/// Decode signed LEB128 (i64).
pub(crate) fn read_leb128_i64(data: &[u8], pos: usize) -> Option<(i64, usize)> {
    let mut result = 0i64;
    let mut shift  = 0u32;
    let mut i = pos;
    loop {
        if i >= data.len() { return None; }
        let b = data[i];
        i += 1;
        result |= ((b & 0x7F) as i64) << shift;
        shift  += 7;
        if b & 0x80 == 0 {
            if shift < 64 && (b & 0x40) != 0 {
                result |= -(1i64 << shift);
            }
            break;
        }
        if shift >= 70 { return None; }
    }
    Some((result, i - pos))
}

// ─── Binary parser ────────────────────────────────────────────────────────────

struct Parser<'a> {
    data: &'a [u8],
    pos:  usize,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Self { Parser { data, pos: 0 } }

    fn remaining(&self) -> usize { self.data.len() - self.pos }
    fn eof(&self) -> bool { self.pos >= self.data.len() }

    fn byte(&mut self) -> Option<u8> {
        if self.eof() { return None; }
        let b = self.data[self.pos];
        self.pos += 1;
        Some(b)
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos + n > self.data.len() { return None; }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }

    fn u32_leb(&mut self) -> Option<u32> {
        let (v, n) = read_leb128_u32(self.data, self.pos)?;
        self.pos += n;
        Some(v)
    }

    fn i32_leb(&mut self) -> Option<i32> {
        let (v, n) = read_leb128_i32(self.data, self.pos)?;
        self.pos += n;
        Some(v)
    }

    fn i64_leb(&mut self) -> Option<i64> {
        let (v, n) = read_leb128_i64(self.data, self.pos)?;
        self.pos += n;
        Some(v)
    }

    fn name(&mut self) -> Option<String> {
        let len = self.u32_leb()? as usize;
        let bytes = self.bytes(len)?;
        // Accept valid UTF-8 or replace invalid sequences
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    fn val_type(&mut self) -> Option<ValType> {
        ValType::from_byte(self.byte()?)
    }

    fn vec_val_types(&mut self) -> Option<Vec<ValType>> {
        let n = self.u32_leb()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n { v.push(self.val_type()?); }
        Some(v)
    }

    /// Parse a constant expression (init_expr): one instruction + 0x0B end.
    fn const_expr(&mut self) -> Option<u64> {
        let op = self.byte()?;
        let val = match op {
            0x41 => self.i32_leb()? as i64 as u64,  // i32.const
            0x42 => self.i64_leb()? as u64,          // i64.const
            0x43 => { self.bytes(4)?; 0 },           // f32.const (skip)
            0x44 => { self.bytes(8)?; 0 },           // f64.const (skip)
            _ => return None,
        };
        let end = self.byte()?;
        if end != 0x0B { return None; } // expected 'end'
        Some(val)
    }

    fn limits(&mut self) -> Option<(u32, Option<u32>)> {
        let flag = self.byte()?;
        let min  = self.u32_leb()?;
        let max  = if flag == 1 { Some(self.u32_leb()?) } else { None };
        Some((min, max))
    }
}

// ─── Top-level load ───────────────────────────────────────────────────────────

/// Parse a WASM binary and return a `WasmModule` ready to interpret.
pub fn load(bytes: &[u8]) -> Result<WasmModule, WasmError> {
    if bytes.len() < 8 { return Err(WasmError::InvalidMagic); }
    if &bytes[0..4] != b"\0asm" { return Err(WasmError::InvalidMagic); }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != 1 { return Err(WasmError::InvalidMagic); }

    let mut m = WasmModule {
        types: Vec::new(), imports: Vec::new(), func_types: Vec::new(),
        table_size: 0, mem_min: 1, mem_max: None,
        globals: Vec::new(), exports: Vec::new(),
        start: None, elem: Vec::new(), code: Vec::new(), data: Vec::new(),
        caps: WASM_DEFAULT_CAPS,
    };

    let mut p = Parser::new(&bytes[8..]);

    while !p.eof() {
        let section_id  = p.byte().ok_or(WasmError::InvalidSection)?;
        let section_len = p.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
        if p.remaining() < section_len { return Err(WasmError::InvalidSection); }
        let sec_data = &p.data[p.pos..p.pos + section_len];
        p.pos += section_len;

        let mut sp = Parser::new(sec_data);

        match section_id {
            0 => { /* custom section — skip */ }

            1 => { // Type
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    let tag = sp.byte().ok_or(WasmError::InvalidSection)?;
                    if tag != 0x60 { return Err(WasmError::InvalidSection); }
                    let params  = sp.vec_val_types().ok_or(WasmError::InvalidSection)?;
                    let results = sp.vec_val_types().ok_or(WasmError::InvalidSection)?;
                    m.types.push(FuncType { params, results });
                }
            }

            2 => { // Import
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    let module = sp.name().ok_or(WasmError::InvalidSection)?;
                    let name   = sp.name().ok_or(WasmError::InvalidSection)?;
                    let kind_b = sp.byte().ok_or(WasmError::InvalidSection)?;
                    let kind = match kind_b {
                        0x00 => ImportKind::Func(sp.u32_leb().ok_or(WasmError::InvalidSection)?),
                        0x01 => { // table
                            sp.byte().ok_or(WasmError::InvalidSection)?; // elemtype (funcref=0x70)
                            sp.limits().ok_or(WasmError::InvalidSection)?;
                            ImportKind::Table
                        }
                        0x02 => { // memory
                            let (min, max) = sp.limits().ok_or(WasmError::InvalidSection)?;
                            ImportKind::Memory(min, max)
                        }
                        0x03 => { // global
                            sp.val_type().ok_or(WasmError::InvalidSection)?;
                            sp.byte().ok_or(WasmError::InvalidSection)?; // mutability
                            ImportKind::Global
                        }
                        _ => return Err(WasmError::InvalidSection),
                    };
                    m.imports.push(Import { module, name, kind });
                }
            }

            3 => { // Function
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    m.func_types.push(sp.u32_leb().ok_or(WasmError::InvalidSection)?);
                }
            }

            4 => { // Table
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    sp.byte().ok_or(WasmError::InvalidSection)?; // elemtype
                    let (min, _max) = sp.limits().ok_or(WasmError::InvalidSection)?;
                    m.table_size = m.table_size.max(min);
                }
            }

            5 => { // Memory
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for i in 0..n {
                    let (min, max) = sp.limits().ok_or(WasmError::InvalidSection)?;
                    if i == 0 { m.mem_min = min; m.mem_max = max; }
                }
            }

            6 => { // Global
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    let vt  = sp.val_type().ok_or(WasmError::InvalidSection)?;
                    let mut_ = sp.byte().ok_or(WasmError::InvalidSection)? != 0;
                    let init = sp.const_expr().ok_or(WasmError::InvalidSection)?;
                    m.globals.push(GlobalDef { val_type: vt, mutable: mut_, init_val: init });
                }
            }

            7 => { // Export
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    let name   = sp.name().ok_or(WasmError::InvalidSection)?;
                    let kind_b = sp.byte().ok_or(WasmError::InvalidSection)?;
                    let kind   = match kind_b {
                        0 => ExportKind::Func,
                        1 => ExportKind::Table,
                        2 => ExportKind::Memory,
                        3 => ExportKind::Global,
                        _ => return Err(WasmError::InvalidSection),
                    };
                    let idx = sp.u32_leb().ok_or(WasmError::InvalidSection)?;
                    m.exports.push(Export { name, kind, idx });
                }
            }

            8 => { // Start
                m.start = Some(sp.u32_leb().ok_or(WasmError::InvalidSection)?);
            }

            9 => { // Element
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    let table_idx = sp.u32_leb().ok_or(WasmError::InvalidSection)?;
                    let offset    = sp.const_expr().ok_or(WasmError::InvalidSection)? as u32;
                    let cnt       = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                    let mut idxs  = Vec::with_capacity(cnt);
                    for _ in 0..cnt {
                        idxs.push(sp.u32_leb().ok_or(WasmError::InvalidSection)?);
                    }
                    m.elem.push(ElemSegment { table_idx, offset, func_idxs: idxs });
                }
            }

            10 => { // Code
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    let body_sz = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                    let body    = Parser::new(sp.bytes(body_sz).ok_or(WasmError::InvalidSection)?);
                    m.code.push(parse_func_body(body)?);
                }
            }

            11 => { // Data
                let n = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                for _ in 0..n {
                    let mem_idx = sp.u32_leb().ok_or(WasmError::InvalidSection)?;
                    let offset  = sp.const_expr().ok_or(WasmError::InvalidSection)? as u32;
                    let len     = sp.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
                    let raw     = sp.bytes(len).ok_or(WasmError::InvalidSection)?;
                    m.data.push(DataSegment { mem_idx, offset, data: raw.to_vec() });
                }
            }

            12 => { // DataCount (WASM bulk-memory extension) — just skip
                let _ = sp.u32_leb();
            }

            _ => {
                // Unknown section — skip (already advanced past it above)
            }
        }
    }

    Ok(m)
}

/// Validate WASM bytecode structure before loading.
///
/// Checks magic bytes, version, and that all section IDs are in the valid MVP
/// range (0–12). Returns `Ok(())` if the binary appears structurally sound.
pub fn validate(bytecode: &[u8]) -> Result<(), &'static str> {
    if bytecode.len() < 8 {
        return Err("too short to be a valid WASM module");
    }
    if &bytecode[0..4] != b"\0asm" {
        return Err("invalid WASM magic bytes");
    }
    let version = u32::from_le_bytes([bytecode[4], bytecode[5], bytecode[6], bytecode[7]]);
    if version != 1 {
        return Err("unsupported WASM version (expected 1)");
    }

    let mut pos = 8usize;
    let data = bytecode;
    while pos < data.len() {
        // Section ID
        if pos >= data.len() { break; }
        let section_id = data[pos];
        pos += 1;
        if section_id > 12 {
            return Err("section ID out of valid range (0-12)");
        }
        // Section length (LEB128)
        let (section_len, n) = read_leb128_u32(data, pos)
            .ok_or("malformed section length")?;
        pos += n;
        let section_len = section_len as usize;
        if pos + section_len > data.len() {
            return Err("section extends past end of module");
        }
        pos += section_len;
    }
    Ok(())
}

fn parse_func_body(mut p: Parser<'_>) -> Result<FuncBody, WasmError> {
    let n_local_groups = p.u32_leb().ok_or(WasmError::InvalidSection)? as usize;
    let mut locals = Vec::new();
    for _ in 0..n_local_groups {
        let count = p.u32_leb().ok_or(WasmError::InvalidSection)?;
        let vt    = p.val_type().ok_or(WasmError::InvalidSection)?;
        locals.push((count, vt));
    }
    // Remaining bytes are the bytecode (including the final 0x0B end).
    let code = p.data[p.pos..].to_vec();
    Ok(FuncBody { locals, code })
}
