//! Scalar LIR/native equivalence verifier for captured Flash cdecl methods.
//! Native instructions are decoded only to verify effects and the returned expression;
//! the replacement is emitted from LIR, not from the native instruction sequence.
use super::*;
use crate::cpu::{EAX, EBP, EBX, EDI, ESI, ESP};
use iced_x86::{Decoder, DecoderOptions, Instruction as X86, Mnemonic as M, OpKind, Register};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Expr {
    Constant(u32),
    Arg(u32),
    HeapAddress(u32),
    HeapRead(u32),
    Op(u8, Vec<Expr>),
    Select(Box<Expr>, Box<Expr>, Box<Expr>),
    Stack(i32),
    Argv(i32),
    Saved(u8),
    Env,
    Argc,
    Frame,
    Limit,
}
fn constant(e: &Expr) -> Option<u32> {
    if let Expr::Constant(n) = e {
        Some(*n)
    } else {
        None
    }
}
fn bool_expr(e: &Expr) -> bool {
    matches!(e, Expr::Op(op::I32_EQ..=op::I32_GE_U, _))
}
fn binary(o: u8, mut a: Expr, mut b: Expr) -> Expr {
    // Expr trees, not an algebra solver: normalize only bit-exact identities.
    match o {
        op::I32_GT_S => return binary(op::I32_LT_S, b, a),
        op::I32_GE_S => return binary(op::I32_LE_S, b, a),
        op::I32_GT_U => return binary(op::I32_LT_U, b, a),
        op::I32_GE_U => return binary(op::I32_LE_U, b, a),
        _ => {}
    }
    if matches!(
        o,
        op::I32_ADD
            | op::I32_AND
            | op::I32_OR
            | op::I32_XOR
            | op::I32_MUL
            | op::I32_EQ
            | op::I32_NE
    ) && a > b
    {
        std::mem::swap(&mut a, &mut b);
    }
    if a == b && matches!(o, op::I32_XOR | op::I32_SUB) {
        return Expr::Constant(0);
    }
    if let (Some(a), Some(b)) = (constant(&a), constant(&b)) {
        let n = match o {
            op::I32_ADD => a.wrapping_add(b),
            op::I32_SUB => a.wrapping_sub(b),
            op::I32_MUL => a.wrapping_mul(b),
            op::I32_AND => a & b,
            op::I32_OR => a | b,
            op::I32_XOR => a ^ b,
            op::I32_SHL => a.wrapping_shl(b),
            op::I32_SHR_U => a.wrapping_shr(b),
            op::I32_SHR_S => (a as i32).wrapping_shr(b) as u32,
            op::I32_EQ => u32::from(a == b),
            op::I32_NE => u32::from(a != b),
            op::I32_LT_S => u32::from((a as i32) < b as i32),
            op::I32_LT_U => u32::from(a < b),
            op::I32_GT_S => u32::from((a as i32) > b as i32),
            op::I32_GT_U => u32::from(a > b),
            op::I32_LE_S => u32::from((a as i32) <= b as i32),
            op::I32_LE_U => u32::from(a <= b),
            op::I32_GE_S => u32::from((a as i32) >= b as i32),
            op::I32_GE_U => u32::from(a >= b),
            _ => return Expr::Op(o, vec![Expr::Constant(a), Expr::Constant(b)]),
        };
        return Expr::Constant(n);
    }
    if matches!(o, op::I32_SHL | op::I32_SHR_U | op::I32_SHR_S) {
        if let Expr::Constant(n) = b {
            b = Expr::Constant(n & 31);
        }
        if let Expr::Op(op::I32_AND, ref xs) = b {
            if let Some((i, _)) = xs
                .iter()
                .enumerate()
                .find(|(_, v)| constant(v).is_some_and(|n| n & 31 == 31))
            {
                b = xs[1 - i].clone();
            }
        }
    }
    if o == op::I32_AND {
        if let Some(mask) = constant(&a) {
            if mask == u32::MAX || (mask & 1 == 1 && bool_expr(&b)) {
                return b;
            }
            if mask == 0 {
                return a;
            }
        }
    }
    if matches!(o, op::I32_ADD | op::I32_OR | op::I32_XOR) && constant(&a) == Some(0) {
        return b;
    }
    if matches!(o, op::I32_SUB | op::I32_SHL | op::I32_SHR_U | op::I32_SHR_S)
        && constant(&b) == Some(0)
    {
        return a;
    }
    Expr::Op(o, vec![a, b])
}
fn select(c: Expr, yes: Expr, no: Expr) -> Expr {
    if yes == no {
        return yes;
    }
    if let Some(v) = constant(&c) {
        return if v != 0 { yes } else { no };
    }
    if constant(&yes) == Some(1) && constant(&no) == Some(0) && bool_expr(&c) {
        return c;
    }
    if constant(&yes) == Some(0) && constant(&no) == Some(1) {
        if let Expr::Op(opcode, ref args) = c {
            let opposite = match opcode {
                op::I32_EQ => op::I32_NE,
                op::I32_NE => op::I32_EQ,
                op::I32_LT_S => op::I32_GE_S,
                op::I32_LT_U => op::I32_GE_U,
                op::I32_LE_S => op::I32_GT_S,
                op::I32_LE_U => op::I32_GT_U,
                _ => 0,
            };
            if opposite != 0 {
                return binary(opposite, args[0].clone(), args[1].clone());
            }
        }
    }
    if let Expr::Op(opcode, ref args) = c {
        match opcode {
            op::I32_LE_S => {
                return select(
                    binary(op::I32_LT_S, args[1].clone(), args[0].clone()),
                    no,
                    yes,
                )
            }
            op::I32_LE_U => {
                return select(
                    binary(op::I32_LT_U, args[1].clone(), args[0].clone()),
                    no,
                    yes,
                )
            }
            op::I32_NE => {
                return select(
                    binary(op::I32_EQ, args[0].clone(), args[1].clone()),
                    no,
                    yes,
                )
            }
            _ => {}
        }
    }
    Expr::Select(Box::new(c), Box::new(yes), Box::new(no))
}

fn trace_expression(trace: &Trace, offsets: &[u32]) -> Result<Expr, String> {
    trace_expressions(trace,offsets).map(|(result,_)|result)
}

fn trace_expressions(trace: &Trace, offsets: &[u32]) -> Result<(Expr,Vec<Expr>), String> {
    if trace.values.len() > 256 {
        return Err("captured SSA complexity limit".into());
    }
    if trace.result != Type::I32
        || trace.params.len() != offsets.len()
        || trace.params.iter().any(|t| *t != Type::I32)
    {
        return Err("pure native bridge supports raw i32 parameters/results only".into());
    }
    let mut values: Vec<Expr> = Vec::new();
    let mut result = None;
    for instruction in &trace.instructions {
        match instruction {
            super::Instruction::Guard { .. } => {
                return Err("captured expression must not contain host-supplied guards".into())
            }
            super::Instruction::Return(i) => result = Some(values[*i].clone()),
            super::Instruction::Value(id) => {
                let value = match &trace.values[*id].expr {
                    super::Expr::Int(n) => Expr::Constant(*n as u32),
                    super::Expr::Param(n) => if offsets[*n] & 0x8000_0000 != 0 {
                        Expr::HeapRead(offsets[*n] & 0x7fff_ffff)
                    } else { Expr::Arg(offsets[*n]) },
                    super::Expr::Negi(a) => {
                        binary(op::I32_SUB, Expr::Constant(0), values[*a].clone())
                    }
                    super::Expr::Noti(a) => {
                        binary(op::I32_XOR, Expr::Constant(u32::MAX), values[*a].clone())
                    }
                    super::Expr::Numeric { opcode, args } if args.len() == 2 => {
                        binary(*opcode, values[args[0]].clone(), values[args[1]].clone())
                    }
                    super::Expr::Select { condition, yes, no } => select(
                        values[*condition].clone(),
                        values[*yes].clone(),
                        values[*no].clone(),
                    ),
                    _ => {
                        return Err("captured expression is not an audited integer operation".into())
                    }
                };
                if expression_nodes(&value, 8192) > 8192 {
                    return Err("captured expression expansion limit".into());
                }
                values.push(value);
            }
        }
    }
    result.map(|result|(result,values)).ok_or_else(|| "captured expression has no return".into())
}
fn expression_nodes(value: &Expr, budget: usize) -> usize {
    if budget == 0 {
        return 1;
    }
    let children: Vec<&Expr> = match value {
        Expr::Op(_, args) => args.iter().collect(),
        Expr::Select(c, a, b) => vec![c, a, b],
        _ => return 1,
    };
    let mut count = 1;
    for child in children {
        count += expression_nodes(child, budget.saturating_sub(count));
        if count > budget {
            break;
        }
    }
    count
}

struct Machine<'a> {
    bytes: &'a [u8],
    entry: u32,
    core: u32,
    regs: Vec<Expr>,
    stack: BTreeMap<i32, Expr>,
    current_frame: Expr,
    linked: bool,
    restored: bool,
    stack_guard: Option<i32>,
    low: i32,
    last_cmp: Option<(Expr, Expr)>,
    allowed_argv: &'a [u32],
    allowed_heap: &'a [u32],
    store_fields: &'a [u32],
    heap_values: BTreeMap<u32, Expr>,
    heap_stores: Vec<(u32, Expr)>,
}
impl Clone for Machine<'_> {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes,
            entry: self.entry,
            core: self.core,
            regs: self.regs.clone(),
            stack: self.stack.clone(),
            current_frame: self.current_frame.clone(),
            linked: self.linked,
            restored: self.restored,
            stack_guard: self.stack_guard,
            low: self.low,
            last_cmp: self.last_cmp.clone(),
            allowed_argv: self.allowed_argv,
            allowed_heap: self.allowed_heap,
            store_fields: self.store_fields,
            heap_values: self.heap_values.clone(),
            heap_stores: self.heap_stores.clone(),
        }
    }
}

fn reg(r: Register) -> Result<(usize, u32), String> {
    if r.is_gpr32() {
        Ok(((r as u32 - Register::EAX as u32) as usize, 32))
    } else if (Register::AL..=Register::BL).contains(&r) {
        Ok(((r as u32 - Register::AL as u32) as usize, 8))
    } else {
        Err(format!("unsupported scalar register {r:?}"))
    }
}
impl Machine<'_> {
    fn source(&self, ins: &X86, i: u32) -> Result<Expr, String> {
        match ins.op_kind(i) {
            OpKind::Register => {
                let (r, width) = reg(ins.op_register(i))?;
                Ok(if width == 32 {
                    self.regs[r].clone()
                } else {
                    binary(op::I32_AND, self.regs[r].clone(), Expr::Constant(255))
                })
            }
            OpKind::Memory => {
                if ins.memory_size().size() != 4 {
                    return Err("noninteger native memory access".into());
                }
                let address = self.address(ins)?;
                let value = match address {
                    Expr::Stack(0) => Expr::Saved(8),
                    Expr::Stack(4) => Expr::Env,
                    Expr::Stack(8) => Expr::Argc,
                    Expr::Stack(12) => Expr::Argv(0),
                    Expr::Stack(offset) if offset < 0 => self
                        .stack
                        .get(&offset)
                        .cloned()
                        .ok_or("native reads an uninitialized spill")?,
                    Expr::Argv(offset) if self.allowed_argv.contains(&(offset as u32)) => {
                        Expr::Arg(offset as u32)
                    }
                    Expr::Constant(a) if a == self.core + 32 => Expr::Limit,
                    Expr::Constant(a) if a == self.core + 48 => self.current_frame.clone(),
                    Expr::HeapAddress(offset) if self.allowed_heap.contains(&offset) =>
                        self.heap_values.get(&offset).cloned().unwrap_or(Expr::HeapRead(offset)),
                    _ => return Err(format!("native heap/unknown read {address:?}")),
                };
                Ok(value)
            }
            OpKind::Immediate8 | OpKind::Immediate8to32 | OpKind::Immediate32 => {
                Ok(Expr::Constant(ins.immediate(i) as u32))
            }
            kind => Err(format!("unsupported scalar operand {kind:?}")),
        }
    }
    fn address(&self, ins: &X86) -> Result<Expr, String> {
        if ins.segment_prefix() != Register::None || ins.memory_index() != Register::None {
            return Err("indexed/segmented native memory not audited".into());
        }
        let displacement = ins.memory_displacement32();
        if ins.memory_base() == Register::None {
            return Ok(Expr::Constant(displacement));
        }
        let (base, width) = reg(ins.memory_base())?;
        if width != 32 {
            return Err("non32-bit address base".into());
        }
        let offset = displacement as i32;
        match &self.regs[base] {
            Expr::Stack(a) => Ok(Expr::Stack(
                a.checked_add(offset)
                    .ok_or("native stack offset overflow")?,
            )),
            Expr::Argv(a) => Ok(Expr::Argv(
                a.checked_add(offset).ok_or("native argv offset overflow")?,
            )),
            Expr::Constant(a) => Ok(Expr::Constant(a.wrapping_add(displacement))),
            Expr::Arg(0) if !self.allowed_heap.is_empty() => Ok(Expr::HeapAddress(displacement)),
            Expr::HeapAddress(a) => Ok(Expr::HeapAddress(a.wrapping_add(displacement))),
            _ => Err("native address is not a proven stack/argv/core location".into()),
        }
    }
    fn assign(&mut self, ins: &X86, value: Expr) -> Result<(), String> {
        if expression_nodes(&value, 8192) > 8192 {
            return Err("native symbolic expression limit".into());
        }
        match ins.op0_kind() {
            OpKind::Register => {
                let (r, width) = reg(ins.op0_register())?;
                self.regs[r] = if width == 32 {
                    value
                } else {
                    binary(
                        op::I32_OR,
                        binary(op::I32_AND, self.regs[r].clone(), Expr::Constant(!255)),
                        binary(op::I32_AND, value, Expr::Constant(255)),
                    )
                };
                Ok(())
            }
            OpKind::Memory if ins.memory_size().size() == 4 => {
                match self.address(ins)? {
                    Expr::Stack(offset) if offset >= self.low && offset <= -4 => {
                        if offset % 4 != 0 {
                            return Err(
                                "unaligned native spill can overlap another live slot".into()
                            );
                        }
                        self.stack.insert(offset, value);
                    }
                    Expr::Constant(a) if a == self.core + 48 => {
                        if let Expr::Stack(offset) = value {
                            if self.linked || offset >= 0 {
                                return Err("native MethodFrame is not a single scoped link".into());
                            }
                            self.linked = true;
                            self.current_frame = Expr::Stack(offset);
                        } else if value == Expr::Frame && self.linked && !self.restored {
                            self.current_frame = value;
                            self.restored = true;
                        } else {
                            return Err("native MethodFrame write is not restored".into());
                        }
                    }
                    Expr::HeapAddress(offset) if self.store_fields.contains(&offset) && self.heap_stores.len()<8 => {
                        self.heap_stores.push((offset, value.clone()));
                        self.heap_values.insert(offset, value);
                    }
                    _ => return Err("native writes outside nonescaping stack/MethodFrame".into()),
                }
                Ok(())
            }
            _ => Err("unsupported native destination".into()),
        }
    }
    fn condition(&self, mn: M) -> Result<Expr, String> {
        let (a, b) = self
            .last_cmp
            .clone()
            .ok_or("condition without an audited compare")?;
        let opcode = match mn {
            M::Sete | M::Je => op::I32_EQ,
            M::Setne | M::Jne => op::I32_NE,
            M::Setb | M::Jb => op::I32_LT_U,
            M::Setbe | M::Jbe => op::I32_LE_U,
            M::Seta | M::Ja => op::I32_GT_U,
            M::Setae | M::Jae => op::I32_GE_U,
            M::Setl | M::Jl => op::I32_LT_S,
            M::Setle | M::Jle => op::I32_LE_S,
            M::Setg | M::Jg => op::I32_GT_S,
            M::Setge | M::Jge => op::I32_GE_S,
            _ => return Err("native condition is not a scalar comparison".into()),
        };
        Ok(binary(opcode, a, b))
    }
}

pub struct Verified {
    pub len: usize,
    pub stack_low: i32,
    pub frame_offset: i32,
    pub instructions: u32,
}

/// Prove the native no-overflow path has the exact LIR integer result and no heap/GC effects.
pub fn verify_native(
    bytes: &[u8],
    entry: u32,
    core: u32,
    trace: &Trace,
    offsets: &[u32],
) -> Result<Verified, String> {
    if core.checked_add(52).is_none()
        || bytes.len() > 4096
        || bytes.len() < 8
        || offsets.len() > 16
        || offsets
            .iter()
            .any(|&n| !(4..=64).contains(&n) || n % 4 != 0)
    {
        return Err("invalid native leaf bounds".into());
    }
    let wanted = trace_expression(trace, offsets)?;
    verify_native_effects(bytes, entry, core, wanted, offsets, &[], &[])
}

/// All receiver fields are explicit, aligned word accesses. The only accepted
/// mutation is one integer write whose value is also the verified return value.
pub fn verify_memory_native(bytes: &[u8], entry: u32, core: u32, trace: &Trace,
    bindings: &[u32], fields: &[u32], store: Option<u32>, argc: u32) -> Result<Verified, String> {
    let result = trace.instructions.iter().find_map(|ins|if let super::Instruction::Return(id)=ins{Some(*id)}else{None}).ok_or("memory trace has no return")?;
    let stores: Vec<_> = store.map(|offset|(offset,result)).into_iter().collect();
    verify_memory_effects(bytes,entry,core,trace,bindings,fields,&stores,argc)
}

pub fn verify_memory_effects(bytes: &[u8], entry: u32, core: u32, trace: &Trace,
    bindings: &[u32], fields: &[u32], stores: &[(u32,usize)], argc: u32) -> Result<Verified, String> {
    if argc > 16 || bindings.len() > 16 || fields.is_empty() || fields.len() > 16
        || fields.iter().any(|&n| n > 4092 || n % 4 != 0)
        || stores.len()>8 || stores.iter().any(|&(offset,id)|!fields.contains(&offset) || id>=trace.values.len())
        || bindings.iter().any(|&n| if n & 0x8000_0000 != 0 { !fields.contains(&(n & 0x7fff_ffff)) } else { n > argc * 4 || n % 4 != 0 }) {
        return Err("invalid captured memory contract".into());
    }
    let arguments: Vec<_> = (0..=argc).map(|i|i*4).collect();
    let (wanted,values) = trace_expressions(trace, bindings)?;
    let effects: Vec<_> = stores.iter().map(|&(offset,id)|(offset,values[id].clone())).collect();
    verify_native_effects(bytes, entry, core, wanted, &arguments, fields, &effects)
}

fn verify_native_effects(bytes: &[u8], entry: u32, core: u32, wanted: Expr,
    offsets: &[u32], fields: &[u32], stores: &[(u32,Expr)]) -> Result<Verified, String> {
    if core.checked_add(52).is_none() || bytes.len() > 4096 || bytes.len() < 8
        || entry.checked_add(bytes.len() as u32).is_none() { return Err("invalid native effect bounds".into()); }
    let store_fields: Vec<_> = stores.iter().map(|&(offset,_)|offset).collect();
    let mut m = Machine {
        bytes,
        entry,
        core,
        regs: (0..8)
            .map(|i| {
                if i == ESP {
                    Expr::Stack(0)
                } else {
                    Expr::Saved(i as u8)
                }
            })
            .collect(),
        stack: BTreeMap::new(),
        current_frame: Expr::Frame,
        linked: false,
        restored: false,
        stack_guard: None,
        low: 0,
        last_cmp: None,
        allowed_argv: offsets,
        allowed_heap: fields,
        store_fields: &store_fields,
        heap_values: BTreeMap::new(),
        heap_stores: Vec::new(),
    };
    // Up to four forward diamonds. Each arm is explored with the machine state
    // at its branch, so nested selections are explicit rather than flattened by
    // assuming one shared alternate path.
    fn explore<'b>(
        m: &mut Machine<'b>,
        mut offset: usize,
        depth: u32,
        decoded: &mut BTreeSet<usize>,
        seen: &mut BTreeSet<usize>,
        binding_end: &mut usize,
        maximum_low: &mut i32,
        steps: &mut u32,
        stores: &[(u32, Expr)],
    ) -> Result<Expr, String> {
        if depth > 4 {
            return Err("native conditional nesting exceeds four diamonds".into());
        }
        loop {
            if *steps >= 256 || !seen.insert(offset) || offset >= m.bytes.len() {
                return Err("native leaf loops, repeats, or leaves capture".into());
            }
            if decoded.insert(offset) {
                *steps += 1;
            }
            let mut decoder = Decoder::with_ip(
                32,
                &m.bytes[offset..],
                m.entry as u64 + offset as u64,
                DecoderOptions::NONE,
            );
            let ins = decoder.decode();
            if ins.is_invalid() {
                return Err("invalid native leaf instruction".into());
            }
            if ins.has_lock_prefix()
                || ins.has_rep_prefix()
                || ins.has_repne_prefix()
                || ins.segment_prefix() != Register::None
            {
                return Err("native leaf instruction prefixes are outside the verified ABI".into());
            }
            offset += ins.len();
            let mn = ins.mnemonic();
            if !matches!(
                mn,
                M::Sete
                    | M::Setne
                    | M::Setb
                    | M::Setbe
                    | M::Seta
                    | M::Setae
                    | M::Setl
                    | M::Setle
                    | M::Setg
                    | M::Setge
                    | M::Movzx
            ) && (0..ins.op_count()).any(|i| {
                ins.op_kind(i) == OpKind::Register
                    && !ins.op_register(i).is_gpr32()
                    && !(i == 1
                        && matches!(mn, M::Shl | M::Shr | M::Sar)
                        && ins.op_register(i) == Register::CL)
            }) {
                return Err("native integer effects require 32-bit operands".into());
            }
            match mn {
                M::Push => {
                    if ins.op0_kind() != OpKind::Register || !ins.op0_register().is_gpr32() {
                        return Err("only register saves are audited native pushes".into());
                    }
                    let value = m.source(&ins, 0)?;
                    let Expr::Stack(sp) = m.regs[ESP] else {
                        return Err("unproven native ESP".into());
                    };
                    let sp = sp.checked_sub(4).ok_or("stack underflow")?;
                    if sp < -4080 || sp % 4 != 0 {
                        return Err("native push exceeds aligned stack bound".into());
                    }
                    m.low = m.low.min(sp);
                    m.regs[ESP] = Expr::Stack(sp);
                    m.stack.insert(sp, value);
                }
                M::Pop => {
                    if ins.op0_kind() != OpKind::Register
                        || !ins.op0_register().is_gpr32()
                        || ins.op0_register() == Register::ESP
                    {
                        return Err("unsupported native pop destination".into());
                    }
                    let Expr::Stack(sp) = m.regs[ESP] else {
                        return Err("unproven native ESP".into());
                    };
                    let value = m
                        .stack
                        .get(&sp)
                        .cloned()
                        .ok_or("uninitialized native pop")?;
                    m.assign(&ins, value)?;
                    m.regs[ESP] = Expr::Stack(sp + 4);
                }
                M::Mov | M::Movzx => {
                    let value = m.source(&ins, 1)?;
                    m.assign(&ins, value)?;
                }
                M::Lea => {
                    let value = m.address(&ins)?;
                    m.assign(&ins, value)?;
                }
                M::Cmp | M::Test => {
                    let (a, b) = (m.source(&ins, 0)?, m.source(&ins, 1)?);
                    m.last_cmp = Some(if mn == M::Cmp {
                        (a, b)
                    } else {
                        (binary(op::I32_AND, a, b), Expr::Constant(0))
                    });
                }
                M::Jae if m.stack_guard.is_none() => {
                    if !m.linked || m.restored {
                        return Err("unexpected native branch".into());
                    }
                    let Some((Expr::Stack(frame), Expr::Limit)) = m.last_cmp.clone() else {
                        return Err("branch is not MethodFrame stack-limit check".into());
                    };
                    if m.current_frame != Expr::Stack(frame) || frame < m.low || frame >= 0 {
                        return Err("invalid MethodFrame stack offset".into());
                    }
                    let target = ins
                        .near_branch32()
                        .checked_sub(m.entry)
                        .ok_or("branch outside native method")? as usize;
                    if target <= offset || target >= m.bytes.len() {
                        return Err("native overflow branch target out of bounds".into());
                    }
                    // The skipped path may call/throw. It is never taken by the replacement:
                    // the wrapper enforces exactly the >= stack-limit condition before committing.
                    m.stack_guard = Some(frame);
                    offset = target;
                    m.last_cmp = None;
                }
                M::Je | M::Jne | M::Jl | M::Jle | M::Jg | M::Jge | M::Jb | M::Jbe | M::Ja
                | M::Jae => {
                    if m.stack_guard.is_none() {
                        return Err("conditional branch precedes the stack-limit guard".into());
                    }
                    let target = ins
                        .near_branch32()
                        .checked_sub(m.entry)
                        .ok_or("branch outside capture")? as usize;
                    if target <= offset || target >= m.bytes.len() {
                        return Err("backward/outside native branch".into());
                    }
                    let condition = m.condition(mn)?;
                    // Taken means the condition is true. The fall-through arm is
                    // therefore the false selection, matching `cmovi condition yes no`.
                    // Clone before either arm: exploring one must not mutate the
                    // machine seen by the other.
                    let mut fallthrough = m.clone();
                    let no = explore(
                        &mut fallthrough,
                        offset,
                        depth + 1,
                        decoded,
                        seen,
                        binding_end,
                        maximum_low,
                        steps,
                        stores,
                    )?;
                    let mut taken = m.clone();
                    let yes = explore(
                        &mut taken,
                        target,
                        depth + 1,
                        decoded,
                        seen,
                        binding_end,
                        maximum_low,
                        steps,
                        stores,
                    )?;
                    return Ok(select(condition, yes, no));
                }
                M::Add | M::Sub | M::And | M::Or | M::Xor | M::Shl | M::Shr | M::Sar | M::Imul => {
                    let a = if mn == M::Imul && ins.op_count() == 3 {
                        m.source(&ins, 1)?
                    } else {
                        m.source(&ins, 0)?
                    };
                    let b = m.source(
                        &ins,
                        if mn == M::Imul && ins.op_count() == 3 {
                            2
                        } else {
                            1
                        },
                    )?;
                    let value = if let (Expr::Stack(sp), Some(n)) = (&a, constant(&b)) {
                        let sp = match mn {
                            M::Add => sp.checked_add(n as i32),
                            M::Sub => sp.checked_sub(n as i32),
                            _ => None,
                        }
                        .ok_or("invalid native stack arithmetic")?;
                        if !(-4096..=0).contains(&sp) {
                            return Err("native stack allocation outside audited bound".into());
                        }
                        m.low = m.low.min(sp);
                        Expr::Stack(sp)
                    } else {
                        let opcode = match mn {
                            M::Add => op::I32_ADD,
                            M::Sub => op::I32_SUB,
                            M::And => op::I32_AND,
                            M::Or => op::I32_OR,
                            M::Xor => op::I32_XOR,
                            M::Shl => op::I32_SHL,
                            M::Shr => op::I32_SHR_U,
                            M::Sar => op::I32_SHR_S,
                            _ => op::I32_MUL,
                        };
                        binary(opcode, a, b)
                    };
                    m.last_cmp = None;
                    m.assign(&ins, value)?;
                }
                M::Neg | M::Not => {
                    let a = m.source(&ins, 0)?;
                    let value = if mn == M::Neg {
                        binary(op::I32_SUB, Expr::Constant(0), a)
                    } else {
                        binary(op::I32_XOR, Expr::Constant(u32::MAX), a)
                    };
                    m.last_cmp = None;
                    m.assign(&ins, value)?;
                }
                M::Sete
                | M::Setne
                | M::Setb
                | M::Setbe
                | M::Seta
                | M::Setae
                | M::Setl
                | M::Setle
                | M::Setg
                | M::Setge => {
                    let value = m.condition(mn)?;
                    m.assign(&ins, value)?;
                }
                M::Ret if ins.op_count() == 0 => {
                    if m.heap_stores.as_slice() != stores {
                        return Err("native/LIR heap write effects differ".into());
                    }
                    if m.stack_guard.is_none()
                        || m.regs[ESP] != Expr::Stack(0)
                        || !m.restored
                        || m.current_frame != Expr::Frame
                    {
                        return Err("native return lacks restored frame".into());
                    }
                    if [EBX, EBP, ESI, EDI]
                        .iter()
                        .any(|&i| m.regs[i] != Expr::Saved(i as u8))
                    {
                        return Err("native leaf does not restore callee-saved GPRs".into());
                    }
                    *binding_end = (*binding_end).max(offset);
                    *maximum_low = (*maximum_low).min(m.low);
                    return Ok(m.regs[EAX].clone());
                }
                M::Nop => {}
                _ => return Err(format!("unsupported native leaf effect {mn:?}")),
            }
        }
    }

    let mut decoded = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut binding_end = 0;
    let mut maximum_low = 0;
    let mut steps = 0;
    let actual = explore(
        &mut m,
        0,
        0,
        &mut decoded,
        &mut seen,
        &mut binding_end,
        &mut maximum_low,
        &mut steps,
        stores,
    )?;
    if actual != wanted {
        return Err(format!(
            "native/LIR return expression differs: {actual:?} != {wanted:?}"
        ));
    }
    Ok(Verified {
        len: binding_end,
        stack_low: maximum_low,
        frame_offset: m.stack_guard.ok_or("native leaf lacks stack-limit guard")?,
        instructions: steps,
    })
}

/// Install only after equivalence verification; no shape-specific arithmetic body is synthesized.
pub fn install(
    mem: &mut crate::mem::Memory,
    jit: &mut crate::jit::JitState,
    entry: u32,
    core: u32,
    native: &[u8],
    source: &str,
    offsets: &[u32],
    argc: u32,
) -> Result<usize, String> {
    if source.len() > 65536
        || source.contains("fw__")
        || argc > 16
        || offsets.is_empty()
        || offsets.len() > 16
        || offsets.iter().any(|&o| o > argc * 4)
    {
        return Err("captured expression limits/argc mismatch".into());
    }
    let semantic = Trace::parse(source).map_err(|e| e.to_string())?;
    let verified = verify_native(native, entry, core, &semantic, offsets)?;
    let layout = jit.lir_layout(mem);
    let scalar = super::fast_region::compile(
        &semantic,
        &layout,
        &super::fast_region::Abi {
            entry,
            core,
            guest_size: u32::try_from(mem.size()).map_err(|_| "guest too large")?,
            argc,
            offsets: offsets.to_vec(),
            stack_low: verified.stack_low,
            frame_offset: verified.frame_offset,
            instruction_cost: verified.instructions,
        },
    )
    .map_err(|e| e.to_string())?;
    let guards = vec![(entry, native[..verified.len].to_vec())];
    let n = jit.install_precompiled(mem, &layout, &scalar.wasm, &[(scalar.region, guards)])?;
    jit.mark_lir_entry(entry);
    Ok(n)
}

pub fn install_memory(mem: &mut crate::mem::Memory, jit: &mut crate::jit::JitState,
    entry: u32, core: u32, native: &[u8], source: &str, bindings: &[u32], fields: &[u32],
    store: Option<u32>, argc: u32) -> Result<usize, String> {
    if source.len() > 65536 { return Err("captured memory trace size limit".into()); }
    let semantic = Trace::parse(source).map_err(|e|e.to_string())?;
    let result = semantic.instructions.iter().find_map(|ins|if let super::Instruction::Return(id)=ins{Some(*id)}else{None}).ok_or("memory trace has no return")?;
    let stores: Vec<_> = store.map(|offset|(offset,result)).into_iter().collect();
    install_memory_effects(mem,jit,entry,core,native,&semantic,bindings,fields,&stores,argc)
}

pub fn install_memory_effects(mem: &mut crate::mem::Memory, jit: &mut crate::jit::JitState,
    entry: u32, core: u32, native: &[u8], semantic: &Trace, bindings: &[u32], fields: &[u32],
    stores: &[(u32,usize)], argc: u32) -> Result<usize, String> {
    let verified = verify_memory_effects(native,entry,core,semantic,bindings,fields,stores,argc)?;
    let layout = jit.lir_layout(mem);
    let compiled = super::memory_region::compile_effects(semantic,&layout,&super::memory_region::Abi {
        entry, core, guest_size: u32::try_from(mem.size()).map_err(|_|"guest too large")?,
        bindings: bindings.to_vec(), fields: fields.to_vec(), store: None, argc,
        stack_low: verified.stack_low, frame_offset: verified.frame_offset, instruction_cost: verified.instructions,
    },stores).map_err(|e|e.to_string())?;
    let count = jit.install_precompiled(mem,&layout,&compiled.wasm,
        &[(compiled.region,vec![(entry,native[..verified.len].to_vec())])])?;
    jit.mark_lir_entry(entry);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binary_leaf(opcode: &[u8]) -> Vec<u8> {
        let mut v = hex("558bec83ec18895dfc8b4d088b55108d45f08b1d50000300894df4895df08905500003008b1d400003003bc38b5dfc7308e8000000008b55108b42048b4a08");
        v.extend_from_slice(opcode);
        v.extend(hex("8b4df0890d500003008be55dc3"));
        v
    }
    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks_exact(2)
            .map(|x| u8::from_str_radix(std::str::from_utf8(x).unwrap(), 16).unwrap())
            .collect()
    }
    #[test]
    fn native_equivalence_accepts_audited_arithmetic_and_rejects_changed_result() {
        for (operation, native) in [
            ("addi", vec![0x03, 0xc1]),
            ("subi", vec![0x2b, 0xc1]),
            ("xori", vec![0x33, 0xc1]),
            ("andi", vec![0x23, 0xc1]),
            ("ori", vec![0x0b, 0xc1]),
            ("muli", vec![0x0f, 0xaf, 0xc1]),
            ("lshi", vec![0xd3, 0xe0]),
            ("rshi", vec![0xd3, 0xf8]),
            ("rshui", vec![0xd3, 0xe8]),
        ] {
            let source =
                format!("params i32 i32\na = parami 0\nb = parami 1\nr = {operation} a b\nreti r");
            let trace = Trace::parse(&source).unwrap();
            let bytes = binary_leaf(&native);
            let proof = verify_native(&bytes, 0x10000, 0x30020, &trace, &[4, 8])
                .unwrap_or_else(|e| panic!("{operation}: {e}"));
            assert_eq!(proof.len, bytes.len());
            assert_eq!(proof.stack_low, -28);
            assert_eq!(proof.frame_offset, -20);
            let wrong =
                Trace::parse("params i32 i32\na = parami 0\nb = parami 1\nr = subi b a\nreti r")
                    .unwrap();
            assert!(
                verify_native(&bytes, 0x10000, 0x30020, &wrong, &[4, 8]).is_err(),
                "{operation}"
            );
            for at in [0, 6, 24, 27, 30, 36, 42, 47, bytes.len() - 1] {
                let mut broken = bytes.clone();
                broken[at] = 0xcc;
                assert!(
                    verify_native(&broken, 0x10000, 0x30020, &trace, &[4, 8]).is_err(),
                    "accepted modified {operation} at {at}"
                );
            }
        }
    }
    #[test]
    fn native_conditional_diamond_proves_both_returns() {
        let mut bytes = binary_leaf(&[]);
        bytes.truncate(0x39);
        bytes.extend(hex(
            "8b42048b4a083bc17d12b8010000008b4df0890d500003008be55dc333c08b4df0890d500003008be55dc3",
        ));
        let trace = Trace::parse("params i32 i32\na = parami 0\nb = parami 1\nr = lti a b\nreti r")
            .unwrap();
        let proof = verify_native(&bytes, 0x10000, 0x30020, &trace, &[4, 8]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(proof.len, 100);
        bytes[85] = 0x0b; // corrupts the taken-path xor eax,eax
        assert!(verify_native(&bytes, 0x10000, 0x30020, &trace, &[4, 8]).is_err());

        // a < 0 ? -1 : (a > 0 ? 1 : 0), using the same short forward diamonds
        // observed in the real Flash sign method.
        let mut nested = binary_leaf(&[]);
        nested.truncate(0x39);
        nested.extend(hex("8b420483f8007d12b8ffffffff8b4df0890d500003008be55dc383f8007e12b8010000008b4df0890d500003008be55dc333c08b4df0890d500003008be55dc3"));
        let nested_trace = Trace::parse("params i32 i32\na = parami 0\nzero = immi 0\none = immi 1\nminus = immi -1\nnegative = lti a zero\npositive = gti a zero\ninner = cmovi positive one zero\nr = cmovi negative minus inner\nreti r").unwrap();
        assert!(verify_native(&nested, 0x10000, 0x30020, &nested_trace, &[4, 8]).unwrap_or_else(|e| panic!("{e}")).len > 0);
        nested[0x55] ^= 1;
        assert!(verify_native(&nested, 0x10000, 0x30020, &nested_trace, &[4, 8]).is_err());
    }
    #[test]
    fn native_unknown_heap_effect_or_callee_clobber_fails_closed() {
        let trace =
            Trace::parse("params i32 i32\na = parami 0\nb = parami 1\nr = subi a b\nreti r")
                .unwrap();
        let bytes = binary_leaf(&[0x2b, 0xc1]);
        for (at, patch) in [
            (0x39, vec![0x8b, 0x02, 0x90]),
            (0x41, vec![0x89, 0x0a, 0x90]),
            (0x41, vec![0x31, 0xdb, 0x90]),
        ] {
            let mut broken = bytes.clone();
            broken[at..at + patch.len()].copy_from_slice(&patch);
            assert!(verify_native(&broken, 0x10000, 0x30020, &trace, &[4, 8]).is_err());
        }
    }

    #[test]
    fn native_repeated_and_dead_stores_are_not_silently_removed() {
        // A spill aliasing a separately saved register is tracked symbolically;
        // overwriting its saved value makes the final restore fail verification.
        let trace =
            Trace::parse("params i32 i32\na = parami 0\nb = parami 1\nr = subi a b\nreti r")
                .unwrap();
        let mut bytes = binary_leaf(&[0x2b, 0xc1]);
        bytes.splice(0x41..0x41, [0x89, 0x45, 0xfc]); // overwrite saved EBX after the restore
                                                      // Native return no longer changes EBX because it was restored earlier;
                                                      // this dead stack write is an allowed cdecl difference, not a heap write.
        assert!(verify_native(&bytes, 0x10000, 0x30020, &trace, &[4, 8]).is_ok());
        bytes.splice(0x44..0x44, [0x8b, 0x5d, 0xfc]); // reload the overwritten slot into EBX
        assert!(verify_native(&bytes, 0x10000, 0x30020, &trace, &[4, 8]).is_err());
    }

    #[test]
    fn adversarial_dead_read_byte_compare_and_overlapping_spill_reject() {
        let trace =
            Trace::parse("params i32 i32\na = parami 0\nb = parami 1\nr = subi a b\nreti r")
                .unwrap();
        let code = binary_leaf(&[0x2b, 0xc1]);
        let mut dead_read = code.clone();
        dead_read.splice(0x39..0x39, [0x8b, 0x4a, 0x40]); // discarded argv+64
        assert!(verify_native(&dead_read, 0x10000, 0x30020, &trace, &[4, 8]).is_err());
        let mut overlap = code.clone();
        overlap.splice(0x41..0x41, [0x89, 0x45, 0xff]); // saved EBP overlap
        assert!(verify_native(&overlap, 0x10000, 0x30020, &trace, &[4, 8]).is_err());
        let mut byte_compare = code.clone();
        byte_compare.splice(0x3f..0x41, [0x38, 0xc8]); // cmp al,cl
        assert!(verify_native(&byte_compare, 0x10000, 0x30020, &trace, &[4, 8]).is_err());
    }

    #[test]
    fn receiver_fields_require_matching_reads_and_exact_write_effects() {
        let prefix = hex("558bec83ec18895dfc8b4d088b45108d55f08b1d50000300894df4895df08915500003008b1d400003003bd38b5dfc7308e8000000008b4510");
        let tail = hex("8b4df0890d500003008be55dc3");
        for (body, source, bindings, fields, store, argc) in [
            ("8b088b4158", "params i32\na = parami 0\nreti a", vec![0x80000058], vec![88], None, 0),
            ("8b108b42588b4a5403c1", "params i32 i32\na = parami 0\nb = parami 1\nr = addi a b\nreti r", vec![0x80000058,0x80000054], vec![88,84], None, 0),
            ("8b088b50048951588b4158", "params i32\na = parami 0\nreti a", vec![4], vec![88], Some(88), 1),
            ("8b088b50048b415803c28941588b4158", "params i32 i32\na = parami 0\nb = parami 1\nr = addi a b\nreti r", vec![0x80000058,4], vec![88], Some(88), 1),
        ] {
            let mut native = prefix.clone(); native.extend(hex(body)); native.extend(&tail);
            let trace = Trace::parse(source).unwrap();
            assert!(verify_memory_native(&native,0x10000,0x30020,&trace,&bindings,&fields,store,argc).is_ok(), "{body}");
            assert!(verify_native(&native,0x10000,0x30020,&trace,&bindings).is_err(), "ordinary leaf must not silently accept heap effects");
            if store.is_some() {
                assert!(verify_memory_native(&native,0x10000,0x30020,&trace,&bindings,&fields,None,argc).is_err());
                let wrong = Trace::parse(&format!("params {}\na = immi 99\nreti a",vec!["i32";bindings.len()].join(" "))).unwrap();
                assert!(verify_memory_native(&native,0x10000,0x30020,&wrong,&bindings,&fields,store,argc).is_err());
            }
            let mut dead = prefix.clone(); dead.extend(hex("8b088b4964")); dead.extend(hex(body)); dead.extend(&tail);
            assert!(verify_memory_native(&dead,0x10000,0x30020,&trace,&bindings,&fields,store,argc).is_err(), "unrecorded receiver read must not be removed");
        }
    }
}
