//! WebAssembly MVP bytecode interpreter.
//!
//! Executes instructions on a value stack.  Control flow (block/loop/if/br)
//! is handled by a per-frame label stack.  Each function call pushes a new
//! `Frame` with its own locals and label stack.
//!
//! All values are stored as `u64` on the stack.  i32 values are zero-extended.
//! The WASM type system ensures they are interpreted correctly per instruction.

extern crate alloc;
use alloc::vec::Vec;
use alloc::vec;

use super::memory::LinearMemory;
use super::{WasmModule, WasmError};
use super::{read_leb128_u32, read_leb128_i32, read_leb128_i64};

// ─── Value stack limits ───────────────────────────────────────────────────────
const MAX_STACK_DEPTH: usize  = 4096;
const MAX_CALL_DEPTH:  usize  = 256;

// ─── Control-flow label ───────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq)]
enum LabelKind { Block, Loop, If }

#[derive(Clone)]
struct Label {
    kind:        LabelKind,
    /// Number of values to keep when branching/ending.
    arity:       usize,
    /// Value-stack depth at the point the label was pushed.
    base_sp:     usize,
    /// PC to jump to on `br` (loops: top of loop body; block/if: after `end`).
    br_target:   usize,
    /// PC just past the matching `end` opcode (used by block/if on normal fall-through).
    end_pc:      usize,
}

// ─── Call frame ───────────────────────────────────────────────────────────────

struct Frame {
    func_idx: u32,
    pc:       usize,
    locals:   Vec<u64>,
    /// Value-stack depth at frame entry (before arguments were pushed).
    base_sp:  usize,
    /// Labels pushed inside this frame.
    labels:   Vec<Label>,
}

// ─── Table (indirect calls) ───────────────────────────────────────────────────

/// Function-reference table entry.
#[derive(Copy, Clone)]
struct TableEntry {
    func_idx: u32,
    valid:    bool,
}

// ─── Interpreter ─────────────────────────────────────────────────────────────

pub struct Interpreter<'m> {
    module:  &'m WasmModule,
    mem:     LinearMemory,
    globals: Vec<u64>,
    table:   Vec<TableEntry>,
    stack:   Vec<u64>,
}

impl<'m> Interpreter<'m> {
    pub fn new(module: &'m WasmModule) -> Result<Self, WasmError> {
        // Allocate linear memory
        let min = module.mem_min.max(1) as usize;
        let mem = LinearMemory::new(min).ok_or(WasmError::OutOfMemory)?;

        // Initialize globals
        let mut globals = Vec::new();
        for gd in &module.globals {
            globals.push(gd.init_val);
        }

        // Initialize function table
        let tsz = module.table_size as usize;
        let mut table = vec![TableEntry { func_idx: 0, valid: false }; tsz.max(1)];

        // Apply element segments
        for seg in &module.elem {
            for (i, fi) in seg.func_idxs.iter().enumerate() {
                let ti = seg.offset as usize + i;
                if ti < table.len() {
                    table[ti] = TableEntry { func_idx: *fi, valid: true };
                }
            }
        }

        let mut rt = Interpreter {
            module, mem, globals, table,
            stack: Vec::with_capacity(64),
        };

        // Apply data segments
        for seg in &module.data {
            if seg.mem_idx == 0 {
                if !rt.mem.write_bytes(seg.offset, &seg.data) {
                    return Err(WasmError::MemoryFault(seg.offset));
                }
            }
        }

        // Run start function if present
        if let Some(start_idx) = module.start {
            rt.call(start_idx, &[])?;
        }

        Ok(rt)
    }

    pub fn call(&mut self, func_idx: u32, args: &[u64]) -> Result<Vec<u64>, WasmError> {
        let n_imports = self.module.import_func_count() as u32;

        if func_idx < n_imports {
            // Host import call
            return self.call_host(func_idx, args);
        }

        let local_idx = (func_idx - n_imports) as usize;
        let body      = self.module.code.get(local_idx).ok_or(WasmError::Trap("bad func idx"))?;
        let ft        = self.module.func_type(func_idx).ok_or(WasmError::Trap("no func type"))?;

        // Push args onto value stack
        let base_sp = self.stack.len();
        for a in args { self.push(*a)?; }

        // Build locals: args + zero-initialised local vars
        let mut locals = Vec::new();
        for i in 0..ft.params.len() {
            locals.push(if i < args.len() { args[i] } else { 0 });
        }
        // local variable groups
        for (count, _vt) in &body.locals {
            for _ in 0..*count { locals.push(0); }
        }

        // Pop args off value stack (frame owns them as locals)
        let n_params = ft.params.len();
        if self.stack.len() < base_sp + n_params {
            return Err(WasmError::Trap("not enough args"));
        }
        self.stack.truncate(base_sp);

        let code = body.code.clone();
        let n_results = ft.results.len();

        let mut frames: Vec<Frame> = Vec::with_capacity(16);
        frames.push(Frame {
            func_idx,
            pc: 0,
            locals,
            base_sp,
            labels: Vec::new(),
        });

        loop {
            if frames.is_empty() { break; }
            if frames.len() > MAX_CALL_DEPTH {
                return Err(WasmError::StackOverflow);
            }

            let fi   = frames.last().unwrap().func_idx;
            let nimp = self.module.import_func_count() as u32;
            let code_ref: &[u8] = {
                let li = (fi - nimp) as usize;
                &self.module.code[li].code
            };

            let result = self.step_frame(frames.last_mut().unwrap(), code_ref)?;

            match result {
                StepResult::Continue => {}

                StepResult::Return(vals) => {
                    let frame = frames.pop().unwrap();
                    // Restore stack to frame's base
                    self.stack.truncate(frame.base_sp);
                    // Push return values
                    for v in vals { self.push(v)?; }
                }

                StepResult::Call(callee_idx, call_args) => {
                    if callee_idx < nimp {
                        let rets = self.call_host(callee_idx, &call_args)?;
                        for v in rets { self.push(v)?; }
                    } else {
                        let li   = (callee_idx - nimp) as usize;
                        let body = &self.module.code[li];
                        let ft   = self.module.func_type(callee_idx)
                            .ok_or(WasmError::Trap("bad callee type"))?;

                        let mut new_locals = Vec::new();
                        for i in 0..ft.params.len() {
                            new_locals.push(if i < call_args.len() { call_args[i] } else { 0 });
                        }
                        for (cnt, _) in &body.locals {
                            for _ in 0..*cnt { new_locals.push(0); }
                        }

                        let new_base = self.stack.len();
                        frames.push(Frame {
                            func_idx: callee_idx,
                            pc: 0,
                            locals: new_locals,
                            base_sp: new_base,
                            labels: Vec::new(),
                        });
                    }
                }
            }
        }

        // Collect results from value stack
        let sp = self.stack.len();
        if sp < base_sp + n_results {
            return Err(WasmError::Trap("missing return values"));
        }
        let results = self.stack[sp - n_results..].to_vec();
        self.stack.truncate(sp - n_results);
        Ok(results)
    }

    // ── Single-frame step ─────────────────────────────────────────────────────

    fn step_frame(&mut self, frame: &mut Frame, code: &[u8]) -> Result<StepResult, WasmError> {
        // Execute until return / call / end-of-function
        loop {
            if frame.pc >= code.len() {
                // Implicit return at end of function
                return Ok(StepResult::Return(self.collect_results(frame)?));
            }

            let op = code[frame.pc];
            frame.pc += 1;

            match op {
                // ── Control ──────────────────────────────────────────────────
                0x00 => return Err(WasmError::Trap("unreachable")),
                0x01 => {} // nop

                0x02 => { // block (blocktype)
                    let (arity, pc_after) = self.parse_blocktype(code, frame.pc)?;
                    frame.pc = pc_after;
                    let end_pc = find_end(code, frame.pc - pc_after + pc_after, frame.pc)?;
                    let sp = self.stack.len();
                    frame.labels.push(Label {
                        kind: LabelKind::Block, arity,
                        base_sp: sp, br_target: end_pc + 1, end_pc,
                    });
                }

                0x03 => { // loop (blocktype)
                    let (_, pc_after) = self.parse_blocktype(code, frame.pc)?;
                    let loop_start = pc_after; // br jumps back here
                    frame.pc = pc_after;
                    let end_pc = find_end(code, frame.pc - 1, frame.pc)?;
                    let sp = self.stack.len();
                    frame.labels.push(Label {
                        kind: LabelKind::Loop, arity: 0,
                        base_sp: sp, br_target: loop_start, end_pc,
                    });
                }

                0x04 => { // if (blocktype)
                    let (arity, pc_after) = self.parse_blocktype(code, frame.pc)?;
                    frame.pc = pc_after;
                    let cond = self.pop()? as i32;
                    let end_pc = find_end(code, frame.pc - 1, frame.pc)?;
                    let sp = self.stack.len();
                    if cond != 0 {
                        frame.labels.push(Label {
                            kind: LabelKind::If, arity,
                            base_sp: sp, br_target: end_pc + 1, end_pc,
                        });
                    } else {
                        // Jump to else or end
                        let else_pc = find_else(code, frame.pc)?;
                        if let Some(ep) = else_pc {
                            frame.pc = ep + 1; // skip else opcode, enter else body
                            frame.labels.push(Label {
                                kind: LabelKind::If, arity,
                                base_sp: sp, br_target: end_pc + 1, end_pc,
                            });
                        } else {
                            frame.pc = end_pc + 1; // skip entire if block
                        }
                    }
                }

                0x05 => { // else
                    // Reached end of then-branch; jump to end
                    if let Some(lbl) = frame.labels.last() {
                        let ep = lbl.end_pc + 1;
                        frame.pc = ep;
                        frame.labels.pop();
                    }
                }

                0x0B => { // end
                    if let Some(lbl) = frame.labels.pop() {
                        // Normal end of block/loop/if — keep `arity` values
                        let keep = lbl.arity;
                        let target_sp = lbl.base_sp;
                        self.trim_stack(target_sp, keep)?;
                    } else {
                        // End of function
                        return Ok(StepResult::Return(self.collect_results(frame)?));
                    }
                }

                0x0C => { // br (labelidx)
                    let (depth, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("br: bad leb"))?;
                    frame.pc += n;
                    self.do_br(frame, depth as usize)?;
                }

                0x0D => { // br_if (labelidx)
                    let (depth, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("br_if: bad leb"))?;
                    frame.pc += n;
                    let cond = self.pop()? as i32;
                    if cond != 0 { self.do_br(frame, depth as usize)?; }
                }

                0x0E => { // br_table
                    let (cnt, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("br_table: bad leb"))?;
                    frame.pc += n;
                    let mut targets = Vec::with_capacity(cnt as usize + 1);
                    for _ in 0..=cnt {
                        let (t, n2) = read_leb128_u32(code, frame.pc)
                            .ok_or(WasmError::Trap("br_table: bad leb2"))?;
                        frame.pc += n2;
                        targets.push(t as usize);
                    }
                    let idx = self.pop()? as usize;
                    let depth = if idx < targets.len() - 1 { targets[idx] } else { *targets.last().unwrap() };
                    self.do_br(frame, depth)?;
                }

                0x0F => { // return
                    return Ok(StepResult::Return(self.collect_results(frame)?));
                }

                0x10 => { // call
                    let (fidx, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("call: bad leb"))?;
                    frame.pc += n;
                    let ft = self.module.func_type(fidx)
                        .ok_or(WasmError::Trap("call: unknown func"))?;
                    let n_params = ft.params.len();
                    let args = self.pop_n(n_params)?;
                    return Ok(StepResult::Call(fidx, args));
                }

                0x11 => { // call_indirect (typeidx, tableidx)
                    let (type_idx, n1) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("call_indirect: bad leb"))?;
                    frame.pc += n1;
                    let (_tidx, n2) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("call_indirect: bad leb2"))?;
                    frame.pc += n2;
                    let tbl_idx = self.pop()? as usize;
                    let entry = self.table.get(tbl_idx).copied()
                        .ok_or(WasmError::Trap("call_indirect: table oob"))?;
                    if !entry.valid { return Err(WasmError::Trap("call_indirect: null ref")); }
                    // Type check
                    let actual_ft = self.module.func_type(entry.func_idx)
                        .ok_or(WasmError::Trap("call_indirect: no type"))?;
                    let expected_ft = self.module.types.get(type_idx as usize)
                        .ok_or(WasmError::Trap("call_indirect: no type idx"))?;
                    if actual_ft.params.len()  != expected_ft.params.len()
                    || actual_ft.results.len() != expected_ft.results.len() {
                        return Err(WasmError::Trap("call_indirect: type mismatch"));
                    }
                    let n_params = expected_ft.params.len();
                    let args = self.pop_n(n_params)?;
                    return Ok(StepResult::Call(entry.func_idx, args));
                }

                // ── Parametric ───────────────────────────────────────────────
                0x1A => { self.pop()?; } // drop
                0x1B => { // select
                    let cond = self.pop()? as i32;
                    let b    = self.pop()?;
                    let a    = self.pop()?;
                    self.push(if cond != 0 { a } else { b })?;
                }

                // ── Variables ────────────────────────────────────────────────
                0x20 => { // local.get
                    let (idx, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("local.get: bad leb"))?;
                    frame.pc += n;
                    let v = *frame.locals.get(idx as usize)
                        .ok_or(WasmError::Trap("local.get: oob"))?;
                    self.push(v)?;
                }
                0x21 => { // local.set
                    let (idx, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("local.set: bad leb"))?;
                    frame.pc += n;
                    let v = self.pop()?;
                    let slot = frame.locals.get_mut(idx as usize)
                        .ok_or(WasmError::Trap("local.set: oob"))?;
                    *slot = v;
                }
                0x22 => { // local.tee
                    let (idx, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("local.tee: bad leb"))?;
                    frame.pc += n;
                    let v = *self.stack.last().ok_or(WasmError::Trap("local.tee: empty"))?;
                    let slot = frame.locals.get_mut(idx as usize)
                        .ok_or(WasmError::Trap("local.tee: oob"))?;
                    *slot = v;
                }
                0x23 => { // global.get
                    let (idx, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("global.get: bad leb"))?;
                    frame.pc += n;
                    let v = *self.globals.get(idx as usize)
                        .ok_or(WasmError::Trap("global.get: oob"))?;
                    self.push(v)?;
                }
                0x24 => { // global.set
                    let (idx, n) = read_leb128_u32(code, frame.pc)
                        .ok_or(WasmError::Trap("global.set: bad leb"))?;
                    frame.pc += n;
                    let v = self.pop()?;
                    let slot = self.globals.get_mut(idx as usize)
                        .ok_or(WasmError::Trap("global.set: oob"))?;
                    *slot = v;
                }

                // ── Memory ops ───────────────────────────────────────────────
                0x28 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u32(a).ok_or(WasmError::MemoryFault(a))?; self.push(v as u64)?; }
                0x29 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u64(a).ok_or(WasmError::MemoryFault(a))?; self.push(v)?; }
                0x2A | 0x2B => { // f32/f64 load — skip memarg, push 0
                    self.skip_memarg(code, &mut frame.pc)?; self.pop()?; self.push(0)?;
                }
                0x2C => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u8(a).ok_or(WasmError::MemoryFault(a))? as i8  as i32 as u64; self.push(v)?; }
                0x2D => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u8(a).ok_or(WasmError::MemoryFault(a))? as u64; self.push(v)?; }
                0x2E => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u16(a).ok_or(WasmError::MemoryFault(a))? as i16 as i32 as u64; self.push(v)?; }
                0x2F => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u16(a).ok_or(WasmError::MemoryFault(a))? as u64; self.push(v)?; }
                0x30 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u8(a).ok_or(WasmError::MemoryFault(a))? as i8  as i64 as u64; self.push(v)?; }
                0x31 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u8(a).ok_or(WasmError::MemoryFault(a))? as u64; self.push(v)?; }
                0x32 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u16(a).ok_or(WasmError::MemoryFault(a))? as i16 as i64 as u64; self.push(v)?; }
                0x33 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u16(a).ok_or(WasmError::MemoryFault(a))? as u64; self.push(v)?; }
                0x34 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u32(a).ok_or(WasmError::MemoryFault(a))? as i32 as i64 as u64; self.push(v)?; }
                0x35 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.mem.load_u32(a).ok_or(WasmError::MemoryFault(a))? as u64; self.push(v)?; }

                0x36 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.pop()? as u32; if !self.mem.store_u32(a, v) { return Err(WasmError::MemoryFault(a)); } }
                0x37 => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.pop()?;        if !self.mem.store_u64(a, v) { return Err(WasmError::MemoryFault(a)); } }
                0x38 | 0x39 => { // f32/f64 store — skip memarg, pop value
                    self.skip_memarg(code, &mut frame.pc)?; self.pop()?;
                }
                0x3A => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.pop()? as u8;  if !self.mem.store_u8(a, v)  { return Err(WasmError::MemoryFault(a)); } }
                0x3B => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.pop()? as u16; if !self.mem.store_u16(a, v) { return Err(WasmError::MemoryFault(a)); } }
                0x3C => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.pop()? as u8;  if !self.mem.store_u8(a, v)  { return Err(WasmError::MemoryFault(a)); } }
                0x3D => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.pop()? as u16; if !self.mem.store_u16(a, v) { return Err(WasmError::MemoryFault(a)); } }
                0x3E => { let a = self.mem_ea(code, &mut frame.pc)?; let v = self.pop()? as u32; if !self.mem.store_u32(a, v) { return Err(WasmError::MemoryFault(a)); } }

                0x3F => { // memory.size
                    frame.pc += 1; // reserved byte
                    self.push(self.mem.size() as u64)?;
                }
                0x40 => { // memory.grow
                    frame.pc += 1; // reserved byte
                    let delta = self.pop()? as u32;
                    let old = self.mem.grow(delta);
                    self.push(old as u64)?;
                }

                // ── Constants ────────────────────────────────────────────────
                0x41 => { let (v, n) = read_leb128_i32(code, frame.pc).ok_or(WasmError::Trap("i32.const"))?; frame.pc += n; self.push(v as u64)?; }
                0x42 => { let (v, n) = read_leb128_i64(code, frame.pc).ok_or(WasmError::Trap("i64.const"))?; frame.pc += n; self.push(v as u64)?; }
                0x43 => { frame.pc += 4; self.push(0)?; } // f32.const — stub
                0x44 => { frame.pc += 8; self.push(0)?; } // f64.const — stub

                // ── i32 comparisons ──────────────────────────────────────────
                0x45 => { let a = self.pop()? as i32; self.push((a == 0) as u64)?; } // i32.eqz
                0x46 => { let (b, a) = self.pop2()?; self.push((a as i32 == b as i32) as u64)?; } // i32.eq
                0x47 => { let (b, a) = self.pop2()?; self.push((a as i32 != b as i32) as u64)?; } // i32.ne
                0x48 => { let (b, a) = self.pop2()?; self.push(((a as i32) <  (b as i32)) as u64)?; } // i32.lt_s
                0x49 => { let (b, a) = self.pop2()?; self.push(((a as u32) <  (b as u32)) as u64)?; } // i32.lt_u
                0x4A => { let (b, a) = self.pop2()?; self.push(((a as i32) >  (b as i32)) as u64)?; } // i32.gt_s
                0x4B => { let (b, a) = self.pop2()?; self.push(((a as u32) >  (b as u32)) as u64)?; } // i32.gt_u
                0x4C => { let (b, a) = self.pop2()?; self.push(((a as i32) <= (b as i32)) as u64)?; } // i32.le_s
                0x4D => { let (b, a) = self.pop2()?; self.push(((a as u32) <= (b as u32)) as u64)?; } // i32.le_u
                0x4E => { let (b, a) = self.pop2()?; self.push(((a as i32) >= (b as i32)) as u64)?; } // i32.ge_s
                0x4F => { let (b, a) = self.pop2()?; self.push(((a as u32) >= (b as u32)) as u64)?; } // i32.ge_u

                // ── i64 comparisons ──────────────────────────────────────────
                0x50 => { let a = self.pop()? as i64; self.push((a == 0) as u64)?; } // i64.eqz
                0x51 => { let (b, a) = self.pop2()?; self.push((a as i64 == b as i64) as u64)?; }
                0x52 => { let (b, a) = self.pop2()?; self.push((a as i64 != b as i64) as u64)?; }
                0x53 => { let (b, a) = self.pop2()?; self.push(((a as i64) <  (b as i64)) as u64)?; }
                0x54 => { let (b, a) = self.pop2()?; self.push((a <  b) as u64)?; } // i64.lt_u
                0x55 => { let (b, a) = self.pop2()?; self.push(((a as i64) >  (b as i64)) as u64)?; }
                0x56 => { let (b, a) = self.pop2()?; self.push((a >  b) as u64)?; } // i64.gt_u
                0x57 => { let (b, a) = self.pop2()?; self.push(((a as i64) <= (b as i64)) as u64)?; }
                0x58 => { let (b, a) = self.pop2()?; self.push((a <= b) as u64)?; } // i64.le_u
                0x59 => { let (b, a) = self.pop2()?; self.push(((a as i64) >= (b as i64)) as u64)?; }
                0x5A => { let (b, a) = self.pop2()?; self.push((a >= b) as u64)?; } // i64.ge_u

                // ── f32/f64 comparisons — stub (push 0) ──────────────────────
                0x5B..=0x66 => { self.pop2()?; self.push(0)?; }

                // ── i32 arithmetic ───────────────────────────────────────────
                0x67 => { let a = self.pop()? as u32; self.push(a.leading_zeros() as u64)?; } // clz
                0x68 => { let a = self.pop()? as u32; self.push(a.trailing_zeros() as u64)?; } // ctz
                0x69 => { let a = self.pop()? as u32; self.push(a.count_ones() as u64)?; } // popcnt
                0x6A => { let (b, a) = self.pop2()?; self.push((a as u32).wrapping_add(b as u32) as u64)?; }
                0x6B => { let (b, a) = self.pop2()?; self.push((a as u32).wrapping_sub(b as u32) as u64)?; }
                0x6C => { let (b, a) = self.pop2()?; self.push((a as u32).wrapping_mul(b as u32) as u64)?; }
                0x6D => { let (b, a) = self.pop2()?; // i32.div_s
                    let (ai, bi) = (a as i32, b as i32);
                    if bi == 0 { return Err(WasmError::DivisionByZero); }
                    self.push(ai.wrapping_div(bi) as u64)?;
                }
                0x6E => { let (b, a) = self.pop2()?; // i32.div_u
                    if b as u32 == 0 { return Err(WasmError::DivisionByZero); }
                    self.push((a as u32 / b as u32) as u64)?;
                }
                0x6F => { let (b, a) = self.pop2()?; // i32.rem_s
                    let (ai, bi) = (a as i32, b as i32);
                    if bi == 0 { return Err(WasmError::DivisionByZero); }
                    self.push(ai.wrapping_rem(bi) as u64)?;
                }
                0x70 => { let (b, a) = self.pop2()?; // i32.rem_u
                    if b as u32 == 0 { return Err(WasmError::DivisionByZero); }
                    self.push((a as u32 % b as u32) as u64)?;
                }
                0x71 => { let (b, a) = self.pop2()?; self.push(((a as u32) & (b as u32)) as u64)?; }
                0x72 => { let (b, a) = self.pop2()?; self.push(((a as u32) | (b as u32)) as u64)?; }
                0x73 => { let (b, a) = self.pop2()?; self.push(((a as u32) ^ (b as u32)) as u64)?; }
                0x74 => { let (b, a) = self.pop2()?; self.push(((a as u32).wrapping_shl(b as u32)) as u64)?; }
                0x75 => { let (b, a) = self.pop2()?; self.push(((a as i32).wrapping_shr(b as u32)) as u64)?; }
                0x76 => { let (b, a) = self.pop2()?; self.push(((a as u32).wrapping_shr(b as u32)) as u64)?; }
                0x77 => { let (b, a) = self.pop2()?; self.push((a as u32).rotate_left(b as u32) as u64)?; }
                0x78 => { let (b, a) = self.pop2()?; self.push((a as u32).rotate_right(b as u32) as u64)?; }

                // ── i64 arithmetic ───────────────────────────────────────────
                0x79 => { let a = self.pop()?; self.push(a.leading_zeros() as u64)?; }
                0x7A => { let a = self.pop()?; self.push(a.trailing_zeros() as u64)?; }
                0x7B => { let a = self.pop()?; self.push(a.count_ones() as u64)?; }
                0x7C => { let (b, a) = self.pop2()?; self.push(a.wrapping_add(b))?; }
                0x7D => { let (b, a) = self.pop2()?; self.push(a.wrapping_sub(b))?; }
                0x7E => { let (b, a) = self.pop2()?; self.push(a.wrapping_mul(b))?; }
                0x7F => { let (b, a) = self.pop2()?; // i64.div_s
                    let (ai, bi) = (a as i64, b as i64);
                    if bi == 0 { return Err(WasmError::DivisionByZero); }
                    self.push(ai.wrapping_div(bi) as u64)?;
                }
                0x80 => { let (b, a) = self.pop2()?; // i64.div_u
                    if b == 0 { return Err(WasmError::DivisionByZero); }
                    self.push(a / b)?;
                }
                0x81 => { let (b, a) = self.pop2()?; // i64.rem_s
                    let (ai, bi) = (a as i64, b as i64);
                    if bi == 0 { return Err(WasmError::DivisionByZero); }
                    self.push(ai.wrapping_rem(bi) as u64)?;
                }
                0x82 => { let (b, a) = self.pop2()?; // i64.rem_u
                    if b == 0 { return Err(WasmError::DivisionByZero); }
                    self.push(a % b)?;
                }
                0x83 => { let (b, a) = self.pop2()?; self.push(a & b)?; }
                0x84 => { let (b, a) = self.pop2()?; self.push(a | b)?; }
                0x85 => { let (b, a) = self.pop2()?; self.push(a ^ b)?; }
                0x86 => { let (b, a) = self.pop2()?; self.push(a.wrapping_shl(b as u32))?; }
                0x87 => { let (b, a) = self.pop2()?; self.push((a as i64).wrapping_shr(b as u32) as u64)?; }
                0x88 => { let (b, a) = self.pop2()?; self.push(a.wrapping_shr(b as u32))?; }
                0x89 => { let (b, a) = self.pop2()?; self.push(a.rotate_left(b as u32))?; }
                0x8A => { let (b, a) = self.pop2()?; self.push(a.rotate_right(b as u32))?; }

                // ── f32/f64 arithmetic — stub (pop, push 0) ──────────────────
                0x8B..=0xA6 => { self.pop()?; self.push(0)?; }

                // ── Conversions ──────────────────────────────────────────────
                0xA7 => { let a = self.pop()?; self.push((a as u32) as u64)?; }  // i32.wrap_i64
                0xA8 => { let a = self.pop()? as i64; self.push(a as i32 as u64)?; } // i32.trunc_f32_s (stub)
                0xA9 => { let a = self.pop()?; self.push(a & 0xFFFFFFFF)?; }  // i32.trunc_f32_u (stub)
                0xAA => { let a = self.pop()? as i64; self.push(a as i32 as u64)?; } // i32.trunc_f64_s (stub)
                0xAB => { let a = self.pop()?; self.push(a & 0xFFFFFFFF)?; }  // i32.trunc_f64_u (stub)
                0xAC => { let a = self.pop()? as i32; self.push(a as i64 as u64)?; } // i64.extend_i32_s
                0xAD => { let a = self.pop()? as u32; self.push(a as u64)?; }  // i64.extend_i32_u
                0xAE..=0xB1 => { let a = self.pop()?; self.push(a)?; } // i64.trunc_f* (stub passthrough)
                0xB2..=0xB9 => { let a = self.pop()?; self.push(a)?; } // f* convert (stub passthrough)
                0xBA => { let a = self.pop()?; self.push(a)?; } // f64.promote_f32 (stub)
                0xBB => { let a = self.pop()?; self.push(a)?; } // f32.demote_f64 (stub)
                0xBC => { let a = self.pop()?; self.push(a & 0xFFFFFFFF)?; } // i32.reinterpret_f32
                0xBD => { let a = self.pop()?; self.push(a)?; }  // i64.reinterpret_f64
                0xBE => { let a = self.pop()?; self.push(a)?; }  // f32.reinterpret_i32
                0xBF => { let a = self.pop()?; self.push(a)?; }  // f64.reinterpret_i64

                // ── Sign extension (MVP extension, widely supported) ──────────
                0xC0 => { let a = self.pop()? as i8;  self.push(a as i32 as u64)?; } // i32.extend8_s
                0xC1 => { let a = self.pop()? as i16; self.push(a as i32 as u64)?; } // i32.extend16_s
                0xC2 => { let a = self.pop()? as i8;  self.push(a as i64 as u64)?; } // i64.extend8_s
                0xC3 => { let a = self.pop()? as i16; self.push(a as i64 as u64)?; } // i64.extend16_s
                0xC4 => { let a = self.pop()? as i32; self.push(a as i64 as u64)?; } // i64.extend32_s

                _ => return Err(WasmError::Unimplemented("unknown opcode")),
            }
        }
    }

    // ── Host import dispatch ──────────────────────────────────────────────────

    fn call_host(&mut self, import_idx: u32, args: &[u64]) -> Result<Vec<u64>, WasmError> {
        let mut fi = 0u32;
        for imp in &self.module.imports {
            if let super::ImportKind::Func(_) = imp.kind {
                if fi == import_idx {
                    return self.dispatch_host_import(&imp.module.clone(), &imp.name.clone(), args);
                }
                fi += 1;
            }
        }
        Err(WasmError::Trap("unknown host import"))
    }

    fn dispatch_host_import(&mut self, module: &str, name: &str, args: &[u64])
        -> Result<Vec<u64>, WasmError>
    {
        let uart = crate::drivers::uart::Uart::new();
        match (module, name) {
            ("aeglos", "log") => {
                // log(ptr: i32, len: i32)
                let ptr = args.get(0).copied().unwrap_or(0) as u32;
                let len = args.get(1).copied().unwrap_or(0) as u32;
                if let Some(bytes) = self.mem.read_bytes(ptr, len) {
                    for &b in bytes {
                        if b == b'\n' { uart.puts("\r\n"); } else { uart.putc(b); }
                    }
                }
                Ok(vec![])
            }
            ("aeglos", "send_ipc") => {
                // send_ipc(tid: i32, msg_ptr: i32) → i32
                let tid = args.get(0).copied().unwrap_or(0) as usize;
                let ptr = args.get(1).copied().unwrap_or(0) as u32;
                let bytes = self.mem.read_bytes(ptr, 32)
                    .ok_or(WasmError::MemoryFault(ptr))?;
                let mut data = [0u8; 32];
                data.copy_from_slice(bytes);
                let msg = crate::ipc::Message { sender: crate::process::current_tid(), data };
                let ok = crate::process::scheduler::send_message(tid, msg).is_ok();
                Ok(vec![if ok { 0 } else { !0u64 }])
            }
            ("aeglos", "recv_ipc") => {
                // recv_ipc(msg_ptr: i32) → i32 (0=ok, -1=empty)
                // Uses non-blocking peek: pops from mailbox but does NOT block.
                let ptr = args.get(0).copied().unwrap_or(0) as u32;
                if let Some(msg) = crate::process::scheduler::recv_message() {
                    if self.mem.write_bytes(ptr, &msg.data[..32]) {
                        return Ok(vec![0]);
                    }
                }
                Ok(vec![!0u64])
            }
            ("env", "abort") | ("env", "__assert_fail") => {
                Err(WasmError::Trap("wasm abort"))
            }

            // ── WASI snapshot_preview1 ────────────────────────────────────
            ("wasi_snapshot_preview1", "args_get") => {
                // args_get(argv: i32, argv_buf: i32) -> i32
                // No args; write nothing, return 0 (success).
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "args_sizes_get") => {
                // args_sizes_get(argc_ptr: i32, buf_size_ptr: i32) -> i32
                let argc_ptr = args.get(0).copied().unwrap_or(0) as u32;
                let buf_ptr  = args.get(1).copied().unwrap_or(0) as u32;
                self.mem.store_u32(argc_ptr, 0);
                self.mem.store_u32(buf_ptr, 0);
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "environ_get") => {
                // environ_get(environ: i32, environ_buf: i32) -> i32
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "environ_sizes_get") => {
                // environ_sizes_get(count_ptr: i32, buf_size_ptr: i32) -> i32
                let count_ptr = args.get(0).copied().unwrap_or(0) as u32;
                let buf_ptr   = args.get(1).copied().unwrap_or(0) as u32;
                self.mem.store_u32(count_ptr, 0);
                self.mem.store_u32(buf_ptr, 0);
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "fd_write") => {
                // fd_write(fd: i32, iovs: i32, iovs_len: i32, nwritten: i32) -> i32
                let fd        = args.get(0).copied().unwrap_or(0) as i32;
                let iovs      = args.get(1).copied().unwrap_or(0) as u32;
                let iovs_len  = args.get(2).copied().unwrap_or(0) as u32;
                let nwritten  = args.get(3).copied().unwrap_or(0) as u32;
                let mut total = 0u32;
                if fd == 1 || fd == 2 {
                    // stdout / stderr — read iovec structs and UART-print
                    for i in 0..iovs_len {
                        let iov_base = iovs + i * 8;
                        let buf_ptr = self.mem.load_u32(iov_base).unwrap_or(0);
                        let buf_len = self.mem.load_u32(iov_base + 4).unwrap_or(0);
                        if let Some(bytes) = self.mem.read_bytes(buf_ptr, buf_len) {
                            for &b in bytes {
                                if b == b'\n' { uart.puts("\r\n"); } else { uart.putc(b); }
                            }
                            total += buf_len;
                        }
                    }
                }
                self.mem.store_u32(nwritten, total);
                Ok(vec![0]) // ESUCCESS
            }
            ("wasi_snapshot_preview1", "fd_read") => {
                // fd_read(fd: i32, iovs: i32, iovs_len: i32, nread: i32) -> i32
                let nread = args.get(3).copied().unwrap_or(0) as u32;
                self.mem.store_u32(nread, 0);
                Ok(vec![8]) // EBADF for non-stdin
            }
            ("wasi_snapshot_preview1", "proc_exit") => {
                // proc_exit(code: i32) — terminates the task
                let _code = args.get(0).copied().unwrap_or(0) as i32;
                // Equivalent to SYS_EXIT: just trap to stop execution cleanly
                Err(WasmError::Trap("proc_exit"))
            }
            ("wasi_snapshot_preview1", "clock_time_get") => {
                // clock_time_get(id: i32, precision: i64, time_ptr: i32) -> i32
                let time_ptr = args.get(2).copied().unwrap_or(0) as u32;
                // Return current ticks as nanoseconds (100 Hz = 10_000_000 ns/tick)
                let ticks = crate::process::scheduler::total_ticks() as u64;
                let nanos = ticks.wrapping_mul(10_000_000u64);
                // Write 64-bit little-endian nanosecond timestamp
                self.mem.store_u32(time_ptr, nanos as u32);
                self.mem.store_u32(time_ptr + 4, (nanos >> 32) as u32);
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "fd_close") => {
                // fd_close(fd: i32) -> i32
                let fd = args.get(0).copied().unwrap_or(0) as usize;
                crate::fs::fat32::close(fd);
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "fd_seek") => {
                // fd_seek(fd: i32, offset: i64, whence: i32, newoffset_ptr: i32) -> i32
                Ok(vec![8]) // EBADF — we don't support seek yet
            }
            ("wasi_snapshot_preview1", "fd_fdstat_get") => {
                // fd_fdstat_get(fd: i32, stat_ptr: i32) -> i32
                let fd = args.get(0).copied().unwrap_or(0) as i32;
                if fd == 0 || fd == 1 || fd == 2 {
                    Ok(vec![0]) // success for std fds
                } else {
                    Ok(vec![8]) // EBADF
                }
            }
            ("wasi_snapshot_preview1", "fd_prestat_get") |
            ("wasi_snapshot_preview1", "fd_prestat_dir_name") => {
                Ok(vec![8]) // EBADF — no pre-opened directories
            }
            ("wasi_snapshot_preview1", "path_open") => {
                Ok(vec![8]) // EBADF stub
            }

            // ── aeglos file I/O extensions ────────────────────────────────
            ("aeglos", "file_open") => {
                // file_open(path_ptr: i32, path_len: i32) -> i32 (fd or -1)
                let ptr = args.get(0).copied().unwrap_or(0) as u32;
                let len = args.get(1).copied().unwrap_or(0) as u32;
                let result = if let Some(bytes) = self.mem.read_bytes(ptr, len) {
                    let path = core::str::from_utf8(bytes).unwrap_or("");
                    match crate::fs::fat32::open(path) {
                        Some(fd) => fd as i32,
                        None     => -1,
                    }
                } else { -1 };
                Ok(vec![result as u64])
            }
            ("aeglos", "file_read") => {
                // file_read(fd: i32, buf_ptr: i32, len: i32) -> i32 (bytes or -1)
                let fd      = args.get(0).copied().unwrap_or(0) as usize;
                let buf_ptr = args.get(1).copied().unwrap_or(0) as u32;
                let buf_len = args.get(2).copied().unwrap_or(0) as u32;
                // Read into a temporary kernel buffer, then copy to WASM memory
                let cap = (buf_len as usize).min(4096);
                let mut tmp = alloc::vec![0u8; cap];
                let n = crate::fs::fat32::read(fd, &mut tmp[..cap]);
                let result = if n > 0 {
                    let slice = &tmp[..n as usize];
                    if self.mem.write_bytes(buf_ptr, slice) { n as i32 } else { -1 }
                } else { n as i32 };
                Ok(vec![result as u64])
            }
            ("aeglos", "file_write") => {
                // file_write(fd: i32, buf_ptr: i32, len: i32) -> i32 (bytes or -1)
                let fd      = args.get(0).copied().unwrap_or(0) as usize;
                let buf_ptr = args.get(1).copied().unwrap_or(0) as u32;
                let buf_len = args.get(2).copied().unwrap_or(0) as u32;
                let result = if let Some(bytes) = self.mem.read_bytes(buf_ptr, buf_len) {
                    let owned: alloc::vec::Vec<u8> = bytes.to_vec();
                    crate::fs::fat32::write(fd, &owned) as i32
                } else { -1 };
                Ok(vec![result as u64])
            }
            ("aeglos", "file_close") => {
                // file_close(fd: i32)
                let fd = args.get(0).copied().unwrap_or(0) as usize;
                crate::fs::fat32::close(fd);
                Ok(vec![])
            }
            ("aeglos", "yield") => {
                // yield() — cooperative CPU yield to the scheduler.
                // WASM apps that run event-loops should call this once per iteration.
                crate::process::scheduler::yield_cpu();
                Ok(vec![])
            }
            ("aeglos", "ai_infer") => {
                // ai_infer(prompt_ptr: i32, prompt_len: i32, out_ptr: i32, out_max: i32) -> i32
                // Sends a prompt to Numenor (TID 1) via IPC; blocks until the response arrives.
                // Writes the response to WASM linear memory.
                // Returns bytes written, or -1 on failure.
                let prompt_ptr = args.get(0).copied().unwrap_or(0) as u32;
                let prompt_len = args.get(1).copied().unwrap_or(0) as u32;
                let out_ptr    = args.get(2).copied().unwrap_or(0) as u32;
                let out_max    = args.get(3).copied().unwrap_or(0) as usize;

                let result: i32 = if let Some(prompt_bytes) = self.mem.read_bytes(prompt_ptr, prompt_len) {
                    let owned: alloc::vec::Vec<u8> = prompt_bytes.to_vec();
                    let mut data = [0u8; 32];
                    data[0..8].copy_from_slice(&2u64.to_le_bytes()); // AI_OP_INFER
                    data[8..16].copy_from_slice(&(owned.as_ptr() as u64).to_le_bytes());
                    data[16..24].copy_from_slice(&(owned.len() as u64).to_le_bytes());
                    let msg = crate::ipc::Message { sender: crate::process::current_tid(), data };
                    if crate::process::scheduler::send_message(1, msg).is_ok() {
                        // Spin-yield until Numenor sends a reply.
                        let mut n_bytes: i32 = 0;
                        loop {
                            if let Some(reply) = crate::process::scheduler::recv_message() {
                                if reply.sender == 1 {
                                    let resp_ptr = u64::from_le_bytes(
                                        reply.data[8..16].try_into().unwrap_or([0; 8])
                                    ) as usize;
                                    let resp_len = u64::from_le_bytes(
                                        reply.data[16..24].try_into().unwrap_or([0; 8])
                                    ) as usize;
                                    if resp_ptr != 0 && resp_len > 0 {
                                        let n = resp_len.min(out_max);
                                        let slice = unsafe {
                                            core::slice::from_raw_parts(resp_ptr as *const u8, n)
                                        };
                                        n_bytes = if self.mem.write_bytes(out_ptr, slice) {
                                            n as i32
                                        } else { -1 };
                                    }
                                    break;
                                }
                                // Message from someone else — re-queue is not possible here;
                                // discard and keep waiting (Numenor is the only expected sender).
                            } else {
                                crate::process::scheduler::yield_cpu();
                            }
                        }
                        n_bytes
                    } else { -1 }
                } else { -1 };
                Ok(vec![result as u64])
            }
            ("aeglos", "http_get") => {
                // http_get(url_ptr: i32, url_len: i32, out_ptr: i32, out_len: i32) -> i32
                let url_ptr = args.get(0).copied().unwrap_or(0) as u32;
                let url_len = args.get(1).copied().unwrap_or(0) as u32;
                let out_ptr = args.get(2).copied().unwrap_or(0) as u32;
                let out_len = args.get(3).copied().unwrap_or(0) as u32;
                let result: i32 = if let Some(url_bytes) = self.mem.read_bytes(url_ptr, url_len) {
                    let url_str = core::str::from_utf8(url_bytes).unwrap_or("");
                    match crate::net::http::parse_url(url_str) {
                        Some((https, host, port, path)) => {
                            if https {
                                -6 // TLS not supported
                            } else {
                                let cap = (out_len as usize).min(8192);
                                let mut tmp = alloc::vec![0u8; cap];
                                match crate::net::http::http_get(host, path, port, &mut tmp[..cap]) {
                                    crate::net::http::HttpResult::Ok(n) => {
                                        if self.mem.write_bytes(out_ptr, &tmp[..n]) { n as i32 } else { -1 }
                                    }
                                    crate::net::http::HttpResult::DnsError => -1,
                                    crate::net::http::HttpResult::TcpError => -2,
                                    crate::net::http::HttpResult::HttpError(_) => -3,
                                    crate::net::http::HttpResult::Timeout => -4,
                                    crate::net::http::HttpResult::BufferTooSmall => -5,
                                }
                            }
                        }
                        None => -1,
                    }
                } else { -1 };
                Ok(vec![result as u64])
            }

            ("wasi_snapshot_preview1", "random_get") => {
                // random_get(buf: i32, buf_len: i32) -> i32
                // Fill with pseudo-random bytes derived from the hardware cycle counter.
                let buf_ptr = args.get(0).copied().unwrap_or(0) as u32;
                let buf_len = args.get(1).copied().unwrap_or(0) as u32;
                let mut seed: u64;
                unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) seed); }
                // LCG with 64-bit state
                let cap = (buf_len as usize).min(65536);
                let mut tmp = alloc::vec![0u8; cap];
                for b in tmp.iter_mut() {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    *b = (seed >> 56) as u8;
                }
                if self.mem.write_bytes(buf_ptr, &tmp) { Ok(vec![0]) } else { Ok(vec![8]) }
            }
            ("wasi_snapshot_preview1", "poll_oneoff") => {
                // poll_oneoff(in: i32, out: i32, nsubscriptions: i32, nevents: i32) -> i32
                // Stub: write nevents=0, return success.
                let nevents_ptr = args.get(3).copied().unwrap_or(0) as u32;
                self.mem.store_u32(nevents_ptr, 0);
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "sched_yield") => {
                // sched_yield() -> i32
                crate::process::scheduler::yield_cpu();
                Ok(vec![0])
            }
            ("wasi_snapshot_preview1", "sock_accept")
            | ("wasi_snapshot_preview1", "sock_recv")
            | ("wasi_snapshot_preview1", "sock_send")
            | ("wasi_snapshot_preview1", "sock_shutdown") => {
                // Networking not routed through WASI sockets — use aeglos.http_get instead.
                Ok(vec![58u64]) // ENOTSUP
            }

            _ => {
                uart.puts("[wasm] unknown import: ");
                uart.puts(module); uart.puts("::"); uart.puts(name); uart.puts("\r\n");
                // Return zeroes — unknown imports are non-fatal; the module may not call them.
                Ok(vec![0])
            }
        }
    }

    // ── Stack helpers ─────────────────────────────────────────────────────────

    #[inline]
    fn push(&mut self, v: u64) -> Result<(), WasmError> {
        if self.stack.len() >= MAX_STACK_DEPTH { return Err(WasmError::StackOverflow); }
        self.stack.push(v);
        Ok(())
    }

    #[inline]
    fn pop(&mut self) -> Result<u64, WasmError> {
        self.stack.pop().ok_or(WasmError::Trap("stack underflow"))
    }

    #[inline]
    fn pop2(&mut self) -> Result<(u64, u64), WasmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        Ok((b, a))
    }

    fn pop_n(&mut self, n: usize) -> Result<Vec<u64>, WasmError> {
        if self.stack.len() < n { return Err(WasmError::Trap("pop_n underflow")); }
        let start = self.stack.len() - n;
        let vals = self.stack[start..].to_vec();
        self.stack.truncate(start);
        Ok(vals)
    }

    /// Trim the stack to `base_sp`, then push the top `keep` values back.
    fn trim_stack(&mut self, base_sp: usize, keep: usize) -> Result<(), WasmError> {
        let sp = self.stack.len();
        if sp < base_sp { return Err(WasmError::Trap("trim_stack underflow")); }
        if keep > 0 && sp >= keep {
            let top: Vec<u64> = self.stack[sp - keep..].to_vec();
            self.stack.truncate(base_sp);
            for v in top { self.push(v)?; }
        } else {
            self.stack.truncate(base_sp);
        }
        Ok(())
    }

    fn collect_results(&mut self, frame: &Frame) -> Result<Vec<u64>, WasmError> {
        let ft = self.module.func_type(frame.func_idx)
            .ok_or(WasmError::Trap("collect_results: no type"))?;
        let n = ft.results.len();
        let sp = self.stack.len();
        if sp < frame.base_sp + n { return Err(WasmError::Trap("missing results")); }
        let vals = self.stack[sp - n..].to_vec();
        self.stack.truncate(sp - n);
        Ok(vals)
    }

    // ── br helper ─────────────────────────────────────────────────────────────

    fn do_br(&mut self, frame: &mut Frame, depth: usize) -> Result<(), WasmError> {
        let label_len = frame.labels.len();
        if depth >= label_len {
            // br out of function — treat as return
            return Err(WasmError::Trap("br depth too large"));
        }
        let lbl_idx = label_len - 1 - depth;
        let lbl = frame.labels[lbl_idx].clone();

        // Preserve `arity` values
        self.trim_stack(lbl.base_sp, lbl.arity)?;

        // Jump
        frame.pc = lbl.br_target;

        // Pop consumed labels (all labels above + the label itself for non-loops)
        if lbl.kind == LabelKind::Loop {
            // loop: keep the label, just jump back
            frame.labels.truncate(lbl_idx + 1);
        } else {
            frame.labels.truncate(lbl_idx);
        }
        Ok(())
    }

    // ── Memory address helpers ────────────────────────────────────────────────

    /// Parse memarg (align + offset LEB128) and return effective address.
    fn mem_ea(&mut self, code: &[u8], pc: &mut usize) -> Result<u32, WasmError> {
        let (_align, n1) = read_leb128_u32(code, *pc).ok_or(WasmError::Trap("memarg align"))?;
        *pc += n1;
        let (offset, n2) = read_leb128_u32(code, *pc).ok_or(WasmError::Trap("memarg offset"))?;
        *pc += n2;
        let base = self.pop()? as u32;
        Ok(base.wrapping_add(offset))
    }

    fn skip_memarg(&self, code: &[u8], pc: &mut usize) -> Result<(), WasmError> {
        let (_, n1) = read_leb128_u32(code, *pc).ok_or(WasmError::Trap("memarg"))?;
        *pc += n1;
        let (_, n2) = read_leb128_u32(code, *pc).ok_or(WasmError::Trap("memarg2"))?;
        *pc += n2;
        Ok(())
    }

    // ── Blocktype helper ──────────────────────────────────────────────────────

    fn parse_blocktype(&self, code: &[u8], pc: usize) -> Result<(usize, usize), WasmError> {
        if pc >= code.len() { return Err(WasmError::Trap("blocktype eof")); }
        let b = code[pc];
        // 0x40 = empty type, 0x7F..0x7C = single value type, otherwise type index
        let (arity, advance) = match b {
            0x40 => (0usize, 1usize),
            0x7F | 0x7E | 0x7D | 0x7C => (1, 1),
            _ => {
                // Signed LEB128 type index (multi-value)
                let (ti, n) = read_leb128_i32(code, pc)
                    .ok_or(WasmError::Trap("blocktype leb"))?;
                let ft = self.module.types.get(ti as usize)
                    .ok_or(WasmError::Trap("blocktype: no type"))?;
                (ft.results.len(), n)
            }
        };
        Ok((arity, pc + advance))
    }
}

// ─── Step result ─────────────────────────────────────────────────────────────

enum StepResult {
    Continue,
    Return(Vec<u64>),
    Call(u32, Vec<u64>),
}

// ─── Control flow scanners ───────────────────────────────────────────────────

/// Find the PC of the matching `end` opcode, accounting for nesting.
fn find_end(code: &[u8], _block_pc: usize, start: usize) -> Result<usize, WasmError> {
    let mut depth = 1usize;
    let mut i = start;
    while i < code.len() {
        let op = code[i];
        i += 1;
        match op {
            0x02 | 0x03 | 0x04 => {
                depth += 1;
                // skip blocktype
                if i < code.len() {
                    let b = code[i];
                    if b == 0x40 || (0x7C..=0x7F).contains(&b) {
                        i += 1;
                    } else {
                        // type index LEB128
                        if let Some((_, n)) = read_leb128_i32(code, i) { i += n; }
                    }
                }
            }
            0x0B => {
                depth -= 1;
                if depth == 0 { return Ok(i - 1); }
            }
            // Skip operands for instructions with immediate operands
            0x0C | 0x0D => { if let Some((_, n)) = read_leb128_u32(code, i) { i += n; } }
            0x0E => { // br_table: vec + default
                if let Some((cnt, n)) = read_leb128_u32(code, i) {
                    i += n;
                    for _ in 0..=cnt {
                        if let Some((_, n2)) = read_leb128_u32(code, i) { i += n2; }
                    }
                }
            }
            0x10 | 0x11 => {
                if let Some((_, n)) = read_leb128_u32(code, i) { i += n; }
                if op == 0x11 { if let Some((_, n)) = read_leb128_u32(code, i) { i += n; } }
            }
            0x20..=0x24 => { if let Some((_, n)) = read_leb128_u32(code, i) { i += n; } }
            0x28..=0x3E => { // memory ops: align + offset LEB
                if let Some((_, n)) = read_leb128_u32(code, i) { i += n; }
                if let Some((_, n)) = read_leb128_u32(code, i) { i += n; }
            }
            0x3F | 0x40 => { i += 1; } // memory.size / memory.grow reserved byte
            0x41 => { if let Some((_, n)) = read_leb128_i32(code, i) { i += n; } }
            0x42 => { if let Some((_, n)) = read_leb128_i64(code, i) { i += n; } }
            0x43 => { i += 4; }
            0x44 => { i += 8; }
            _ => {}
        }
    }
    Err(WasmError::Trap("find_end: no matching end"))
}

/// Find the PC of the `else` opcode at the same nesting level, if any.
fn find_else(code: &[u8], start: usize) -> Result<Option<usize>, WasmError> {
    let mut depth = 1usize;
    let mut i = start;
    while i < code.len() {
        let op = code[i];
        i += 1;
        match op {
            0x02 | 0x03 | 0x04 => {
                depth += 1;
                if i < code.len() {
                    let b = code[i];
                    if b == 0x40 || (0x7C..=0x7F).contains(&b) { i += 1; }
                    else if let Some((_, n)) = read_leb128_i32(code, i) { i += n; }
                }
            }
            0x05 => { if depth == 1 { return Ok(Some(i - 1)); } }
            0x0B => {
                depth -= 1;
                if depth == 0 { return Ok(None); }
            }
            0x0C | 0x0D => { if let Some((_, n)) = read_leb128_u32(code, i) { i += n; } }
            0x0E => {
                if let Some((cnt, n)) = read_leb128_u32(code, i) {
                    i += n;
                    for _ in 0..=cnt { if let Some((_, n2)) = read_leb128_u32(code, i) { i += n2; } }
                }
            }
            0x10 => { if let Some((_, n)) = read_leb128_u32(code, i) { i += n; } }
            0x11 => {
                if let Some((_, n)) = read_leb128_u32(code, i) { i += n; }
                if let Some((_, n)) = read_leb128_u32(code, i) { i += n; }
            }
            0x20..=0x24 => { if let Some((_, n)) = read_leb128_u32(code, i) { i += n; } }
            0x28..=0x3E => {
                if let Some((_, n)) = read_leb128_u32(code, i) { i += n; }
                if let Some((_, n)) = read_leb128_u32(code, i) { i += n; }
            }
            0x3F | 0x40 => { i += 1; }
            0x41 => { if let Some((_, n)) = read_leb128_i32(code, i) { i += n; } }
            0x42 => { if let Some((_, n)) = read_leb128_i64(code, i) { i += n; } }
            0x43 => { i += 4; }
            0x44 => { i += 8; }
            _ => {}
        }
    }
    Ok(None)
}
