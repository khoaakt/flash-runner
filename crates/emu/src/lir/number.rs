//! Captured Flash Number leaves: independent native effect proof, raw-bit SSA, and the real
//! cdecl/x87 return ABI.
//!
//! The adapter is a self-contained wasm function (no host imports). It mirrors, guard for guard, the
//! interpreter's host-FP fast path that the emulator JIT already inlines (`jit_fp.rs`): MXCSR must be
//! round-to-nearest, no FTZ/DAZ, PE already sticky; operands normal or zero; results finite with
//! exponent >= 2 or an exact zero that cannot be an underflow. Anything else, or a full x87 stack,
//! returns S_RETRY with nothing mutated and the original native method runs. The x87 push (one
//! `FLD m64` of a normal/zero double) is done inline. A successful return tail-chains to the caller
//! region like every other LIR adapter. Static SIMD masks are rechecked (bytes + read permission) on
//! every invocation. `native_return` / `native_expression` below are the exact soft-float oracle the
//! tests compare the adapter against; they are not part of the runtime.
use super::memory_region::RECEIVER_FIELD;
use super::{error, Error, Expr as LirExpr, Instruction as LirInstruction, Trace, Type};
use crate::cpu::{Cpu, EAX, EBP, EBX, EDI, ESI, ESP};
use crate::fpu::{Fpu, F80};
use crate::jit_x86::{Layout, RegionOut, CACHE_ENTRIES, S_NEXT, S_RETRY};
use crate::wasm_emit::{op, Code, ModuleBuilder, I32};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic as M, OpKind, Register};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Number {
    Arg(u32),
    Neg(Box<Number>),
    Binary(u32, Box<Number>, Box<Number>),
    /// A double read from static guest memory (a NanoJIT constant-pool slot), bound by address.
    Const(u64),
    /// An int32 argv argument converted to a double (exact: `cvtsi2sd` / `fild`; no flag, no rounding).
    FromInt(u32),
    /// An int32 argv argument that has not been converted. Only `i2d` may consume it.
    RawInt(u32),
    /// `cvttsd2si`: the double truncated toward zero to an int32. Only ever the root of an int-returning
    /// expression, and only used where |x| < 2^31 (the native range check falls back to a helper call).
    Trunc(Box<Number>),
    /// A double read from a receiver object field (`[argv[0] + offset]`) that no earlier native store changed.
    Field(u32),
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Saved(usize), Stack(i32), Argv(i32), IntArg(u32), Constant(u32), Env, Argc, Frame, Limit,
    /// `argv[0]`: the receiver object pointer. An address based on it is a `Field`.
    Receiver, Field(u32),
    Number(Number),
}

/// Bit-exact native operation, deliberately not the normalized wasm f64 opcode.
/// 0 identity, 1 XOR sign, 2 add, 3 sub, 4 mul, 5 div.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation { pub kind: u32, pub a: u32, pub b: u32 }

pub const MAX_INPUTS: usize = 4;
pub const MAX_CONSTS: usize = 2;
pub const MAX_STEPS: usize = 5;
/// Value table: argv inputs 0..MAX_INPUTS, then constants, then one value per step.
pub const CONST0: usize = MAX_INPUTS;
pub const STEP0: usize = MAX_INPUTS + MAX_CONSTS;

/// One straight-line SSE step. `kind` is 1 XOR sign, 2 add, 3 sub, 4 mul, 5 div; `a`/`b` index
/// the value table (argv inputs, constants, then one value per earlier step). Negation uses `b == a`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step { pub kind: u32, pub a: u32, pub b: u32 }

/// Bounded straight-line SSE program over raw argv Numbers and receiver fields. `inputs` are bindings in SSA
/// parameter order: an argv byte offset, or `RECEIVER_FIELD | field offset` for a double read from the receiver; `consts` are the bits of the doubles the native code loads from static memory
/// (each bound to its address in `Verified::statics`). Step `i` produces value `STEP0 + i`; the last
/// step is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub inputs: Vec<u32>,
    /// Per input: an int32 slot (converted to a double on load) rather than a double slot.
    pub ints: Vec<bool>,
    pub consts: Vec<u64>,
    pub steps: Vec<Step>,
    /// Value index returned: any input, constant or step.
    pub result: u32,
    /// Ordered native stores to receiver fields: (field offset, value index). Values are written only after every
    /// guard and the whole computation have succeeded.
    pub stores: Vec<(u32, u32)>,
}
impl Program {
    #[cfg(test)]
    /// Operand order and sequence are part of the encoding: count in bits 0..3, then one
    /// 11-bit step (kind:3, a:4, b:4) per operation.
    pub fn pack(&self) -> u64 {
        self.steps.iter().enumerate().fold(self.steps.len() as u64, |bits, (i, s)|
            bits | ((s.kind as u64 | (s.a as u64) << 3 | (s.b as u64) << 7) << (3 + 11 * i)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape { Simple(Operation), Program(Program) }

/// Returns the wanted result tree, the lowering shape, and the exact ordered list of
/// flag-raising SSE effects (binary operations; negation is a flagless bit flip).
fn expression(trace: &Trace, offsets: &[u32], stores: &[(u32, usize)]) -> Result<(Number, Shape, Vec<Number>, Vec<(u32, Number)>), String> {
    if trace.params.len() != offsets.len() || trace.values.len() > 32 {
        return Err("Number bridge requires a bounded SSA".into());
    }
    // Per SSA value: its tree, its value-table index, and how often it is used.
    let mut values: Vec<(Number, usize)> = Vec::new();
    let mut uses: Vec<u32> = Vec::new();
    let mut steps: Vec<Step> = Vec::new();
    let mut consts: Vec<u64> = Vec::new();
    let mut result = None;
    let mut effects = Vec::new();
    for i in &trace.instructions {
        match i {
            LirInstruction::Value(id) => {
                let value = match &trace.values[*id].expr {
                    LirExpr::Param(n) => match (trace.params[*n], offsets[*n] & RECEIVER_FIELD != 0) {
                        (Type::F64, false) => (Number::Arg(offsets[*n]), *n),
                        (Type::F64, true) => (Number::Field(offsets[*n] & !RECEIVER_FIELD), *n),
                        (Type::I32, false) => (Number::RawInt(offsets[*n]), *n),
                        (Type::I32, true) => return Err("int receiver fields belong to the memory family".into()),
                    },
                    LirExpr::Numeric { opcode: op::F64_CONVERT_I32_S, args } if args.len() == 1 => {
                        // The input slot already holds the converted double; no step, no flag.
                        let Number::RawInt(offset) = values[args[0]].0 else { return Err("i2d of a non-argument".into()); };
                        uses[args[0]] += 1;
                        (Number::FromInt(offset), values[args[0]].1)
                    }
                    LirExpr::Double(bits) => {
                        let at = match consts.iter().position(|c| c == bits) {
                            Some(at) => at,
                            None if consts.len() < MAX_CONSTS => { consts.push(*bits); consts.len() - 1 }
                            None => return Err("Number expression has too many distinct constants".into()),
                        };
                        (Number::Const(*bits), CONST0 + at)
                    }
                    LirExpr::D2i(arg) => {
                        uses[*arg] += 1;
                        let a = values[*arg].1 as u32;
                        steps.push(Step { kind: 6, a, b: a });
                        (Number::Trunc(Box::new(values[*arg].0.clone())), STEP0 + steps.len() - 1)
                    }
                    LirExpr::Numeric { opcode: 0x9a, args } if args.len() == 1 => {
                        uses[args[0]] += 1;
                        let a = values[args[0]].1 as u32;
                        steps.push(Step { kind: 1, a, b: a });
                        (Number::Neg(Box::new(values[args[0]].0.clone())), STEP0 + steps.len() - 1)
                    }
                    LirExpr::Numeric { opcode, args } if args.len() == 2 => {
                        let kind = match *opcode { op::F64_ADD => 2, op::F64_SUB => 3,
                            op::F64_MUL => 4, op::F64_DIV => 5,
                            _ => return Err("unproved Number arithmetic".into()) };
                        uses[args[0]] += 1; uses[args[1]] += 1;
                        steps.push(Step { kind, a: values[args[0]].1 as u32, b: values[args[1]].1 as u32 });
                        let value = Number::Binary(kind, Box::new(values[args[0]].0.clone()), Box::new(values[args[1]].0.clone()));
                        effects.push(value.clone());
                        (value, STEP0 + steps.len() - 1)
                    }
                    _ => return Err("unproved Number SSA operation".into()),
                };
                values.push(value); uses.push(0);
            }
            LirInstruction::Return(id) => { uses[*id] += 1; result = Some(*id); }
            _ => return Err("Number trace must not contain user guards".into()),
        }
    }
    let result = result.ok_or("Number trace has no return")?;
    // Native stores to receiver fields consume their values (a store is an effect, not a dead computation).
    let mut store_trees = Vec::new();
    let mut program_stores = Vec::new();
    for &(field, id) in stores {
        if id >= values.len() || trace.values[id].ty != Type::F64 || field > 4088 || field % 4 != 0 || stores.len() > 4 {
            return Err("invalid Number field store".into());
        }
        uses[id] += 1;
        store_trees.push((field, values[id].0.clone()));
        program_stores.push((field, values[id].1 as u32));
    }
    // A value nothing consumes would be a native SSE effect (flags) the replacement omits or
    // reorders. With no dead values the returned value is necessarily the last step.
    if values.iter().zip(&uses).any(|((n, _), u)| *u == 0 && !matches!(n, Number::Arg(_) | Number::Const(_) | Number::FromInt(_) | Number::RawInt(_))) {
        return Err("Number expression has a dead FP value".into());
    }
    let wanted = values[result].0.clone();
    // An int-returning expression is exactly one truncation at the root; a double-returning one has none.
    if (trace.result == Type::I32) != matches!(wanted, Number::Trunc(_)) {
        return Err("Number result type does not match its root operation".into());
    }
    // Raw-argument single operations keep the original compact shape; everything else is a program.
    let simple = if !stores.is_empty() { None } else { match &wanted {
        Number::Arg(a) if steps.is_empty() => Some(Operation { kind: 0, a: *a, b: *a }),
        Number::Neg(a) if steps.len() == 1 => match **a { Number::Arg(a) => Some(Operation { kind: 1, a, b: a }), _ => None },
        Number::Binary(kind, a, b) if steps.len() == 1 => match (&**a, &**b) {
            (Number::Arg(a), Number::Arg(b)) => Some(Operation { kind: *kind, a: *a, b: *b }),
            _ => None,
        },
        _ => None,
    } };
    let result_index = values[result].1 as u32;
    let shape = match simple {
        Some(operation) => Shape::Simple(operation),
        None if (steps.is_empty() && !matches!(wanted, Number::FromInt(_) | Number::Field(_))
                && !(!program_stores.is_empty() && matches!(wanted, Number::Arg(_))))
            || steps.len() > MAX_STEPS || offsets.len() > MAX_INPUTS => {
            return Err("Number expression exceeds the bounded program limits".into());
        }
        None => Shape::Program(Program {
            inputs: offsets.to_vec(), ints: trace.params.iter().map(|t| *t == Type::I32).collect(), consts, steps, result: result_index,
            stores: program_stores,
        }),
    };
    Ok((wanted, shape, effects, store_trees))
}

#[cfg(test)]
/// Destination physical x87 register for a native `FLD`, or None when the destination is occupied
/// (the caller's x87 stack would overflow) or TOP is invalid. Pure: no state is touched.
fn fld_destination(f: &Fpu) -> Option<usize> {
    if f.top > 7 { return None; }
    let next = (f.top.wrapping_sub(1) & 7) as usize;
    if (f.tag >> (2 * next)) & 3 != 3 { return None; }
    Some(next)
}
#[cfg(test)]
/// The architectural effect of one `FLD m64` with the given (already computed) double bits.
fn commit_fld(f: &mut Fpu, next: usize, bits: u64, ip: u32, dp: u32) {
    f.sw &= !0x0200; // fbegin: FLD clears C1, conversion sets IE/DE if required.
    f.last_ip = ip;
    f.last_dp = dp;
    let value = f.f64_to_f80(bits);
    f.st[next] = value;
    f.tag = (f.tag & !(3 << (2 * next))) | (Fpu::tag_of(value) << (2 * next));
    f.top = next as u8;
}

#[cfg(test)]
/// Emulate exactly one native FLD m64 after the selected exact SSE operation.
/// All refusal guards precede mutation. No allocation, guest access, call/GC,
/// pending fault or dependency on host FP state. An occupied destination falls
/// back rather than overwriting the caller's existing x87 stack on overflow.
pub fn native_return(cpu: &mut Cpu, a: u64, b: u64, kind: u32, ip: u32, dp: u32) -> u32 {
    if kind > 5 { return S_RETRY; }
    let Some(next) = fld_destination(&cpu.fpu) else { return S_RETRY; };
    let bits = if kind < 2 { a ^ if kind == 1 { 1 << 63 } else { 0 } } else {
        use crate::sse_fp::{self as fp, Env, F64};
        let mut env = Env::new(cpu.mxcsr);
        let result = match kind { 2 => fp::add::<F64>(a, b, &mut env, false),
            3 => fp::add::<F64>(a, b, &mut env, true),
            4 => fp::mul::<F64>(a, b, &mut env), _ => fp::div::<F64>(a, b, &mut env) };
        cpu.mxcsr |= env.fl;
        result
    };
    commit_fld(&mut cpu.fpu, next, bits, ip, dp);
    0
}

#[cfg(test)]
/// Run a packed `Program` (see `Program::pack`) with the exact SSE semantics of the native
/// instruction sequence, then commit one `FLD m64`. Each step builds its FP environment from the
/// running MXCSR exactly as consecutive native instructions do; sticky flags accumulate in a local
/// copy and are committed together with the x87 push, so a refusal mutates nothing.
/// Malformed programs (unreachable from `compile`) are refused.
pub fn native_expression(cpu: &mut Cpu, inputs: [u64; MAX_INPUTS], consts: [u64; MAX_CONSTS], program: u64, result: usize, ip: u32, dp: u32) -> u32 {
    native_expression_values(cpu, inputs, consts, program, result, ip, dp).0
}
#[cfg(test)]
/// As `native_expression`, also returning the value table so tests can derive the expected field stores.
pub fn native_expression_values(cpu: &mut Cpu, inputs: [u64; MAX_INPUTS], consts: [u64; MAX_CONSTS], program: u64, result: usize, ip: u32, dp: u32)
    -> (u32, [u64; STEP0 + MAX_STEPS]) {
    let none = [0u64; STEP0 + MAX_STEPS];
    let count = (program & 7) as usize;
    if count > MAX_STEPS || result >= STEP0 + MAX_STEPS || (count == 0 && result >= MAX_INPUTS) { return (S_RETRY, none); }
    use crate::sse_fp::{self as fp, Env, F64};
    let mut values = [0u64; STEP0 + MAX_STEPS];
    values[..MAX_INPUTS].copy_from_slice(&inputs);
    values[CONST0..STEP0].copy_from_slice(&consts);
    let mut mxcsr = cpu.mxcsr;
    let mut int_result = None;
    for i in 0..count {
        let step = program >> (3 + 11 * i);
        let (kind, a, b) = ((step & 7) as u32, ((step >> 3) & 15) as usize, ((step >> 7) & 15) as usize);
        if !(1..=6).contains(&kind) || a >= STEP0 + i || b >= STEP0 + i || (kind == 1 && a != b)
            || (kind == 6 && (a != b || i + 1 != count)) { return (S_RETRY, none); }
        let (x, y) = (values[a], values[b]);
        values[STEP0 + i] = if kind == 1 { x ^ 1 << 63 } else if kind == 6 {
            // cvttsd2si: exact interpreter semantics. 0x80000000 is the "indefinite" the native range check
            // sends to a helper call, which the adapter leaves to the native method.
            let mut env = Env::new(mxcsr);
            let v = fp::f2i::<F64>(x, true, &mut env);
            mxcsr |= env.fl;
            if v == 0x8000_0000 { return (S_RETRY, none); }
            int_result = Some(v);
            v as u64
        } else {
            let mut env = Env::new(mxcsr);
            let result = match kind { 2 => fp::add::<F64>(x, y, &mut env, false), 3 => fp::add::<F64>(x, y, &mut env, true),
                4 => fp::mul::<F64>(x, y, &mut env), _ => fp::div::<F64>(x, y, &mut env) };
            mxcsr |= env.fl;
            result
        };
    }
    if program >> (3 + 11 * count) != 0 { return (S_RETRY, none); }
    if let Some(v) = int_result {
        cpu.mxcsr = mxcsr; cpu.r[EAX] = v;
        return (0, values);
    }
    let Some(next) = fld_destination(&cpu.fpu) else { return (S_RETRY, none); };
    cpu.mxcsr = mxcsr;
    commit_fld(&mut cpu.fpu, next, values[result], ip, dp);
    (0, values)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataPointer { Argv(u32), Stack(i32), Field(u32) }
#[derive(Clone, Debug)]
pub struct Verified {
    pub len: usize,
    pub stack_low: i32,
    pub frame_offset: i32,
    pub instructions: u32,
    pub fld_ip: u32,
    pub data_pointer: DataPointer,
    pub shape: Shape,
    pub masks: Vec<(u32, [u8; 16])>,
    /// Static guest doubles the native code loads (address, bits); rechecked on every invocation.
    pub statics: Vec<(u32, u64)>,
    /// Doubles the native code materializes from code immediates (`mov r32,imm` + `cvtsi2sd`, or the
    /// `xorpd x,x` zero idiom). They live in bytes the SMC binding already covers.
    pub immediates: Vec<u64>,
    pub argv_size: u32,
    /// Number of argv arguments the proof read, counting whole-double gaps as unread parameters.
    pub argc: u32,
    /// The method returns an int32 in EAX (`Std.int`): no x87 push, `fld_ip`/`data_pointer` are unused.
    pub int_result: bool,
    /// Every receiver field the native code touched (dead reads included), sorted. All are preflighted.
    pub fields: Vec<u32>,
}

struct Machine<'a> {
    core: u32,
    regs: Vec<Value>,
    xmm: Vec<Option<Number>>,
    stack: BTreeMap<i32, (usize, Value)>,
    frame: Value,
    linked: bool,
    restored: bool,
    low: i32,
    guard: Option<i32>,
    cmp: Option<(Value, Value)>,
    offsets: &'a [u32],
    ints: &'a [u32],
    /// Receiver fields: current value after native stores, every accessed offset (dead reads included), store order.
    fields: BTreeMap<u32, Number>,
    accessed: std::cell::RefCell<BTreeSet<u32>>,
    store_log: Vec<(u32, Number)>,
}
fn gpr(r: Register) -> Result<usize, String> {
    if r.is_gpr32() { Ok(r.number()) } else { Err("non32-bit native Number GPR".into()) }
}
fn xmm(r: Register) -> Result<usize, String> {
    if (Register::XMM0..=Register::XMM7).contains(&r) { Ok(r.number()) }
    else { Err("non-XMM native Number SIMD operand".into()) }
}
impl Machine<'_> {
    fn address(&self, i: &Instruction) -> Result<Value, String> {
        if i.segment_prefix() != Register::None || i.memory_index() != Register::None {
            return Err("indexed/segmented Number address".into());
        }
        let d = i.memory_displacement32();
        if i.memory_base() == Register::None { return Ok(Value::Constant(d)); }
        let add = |a: i32| a.checked_add(d as i32).ok_or("Number address overflows");
        match self.regs[gpr(i.memory_base())?] {
            Value::Stack(a) => Ok(Value::Stack(add(a)?)),
            Value::Argv(a) => Ok(Value::Argv(add(a)?)),
            Value::Receiver => {
                let field = d as i32;
                if !(0..=4088).contains(&field) || field % 4 != 0 { return Err("Number receiver field out of range".into()); }
                Ok(Value::Field(field as u32))
            }
            Value::Constant(a) => Ok(Value::Constant(a.wrapping_add(d))),
            _ => Err("unproved native Number address".into()),
        }
    }
    fn load(&self, i: &Instruction, size: usize) -> Result<Value, String> {
        if i.memory_size().size() != size { return Err("native Number memory width mismatch".into()); }
        let a = self.address(i)?;
        match a {
            Value::Stack(0) if size == 4 => Ok(Value::Saved(8)),
            Value::Stack(4) if size == 4 => Ok(Value::Env),
            Value::Stack(8) if size == 4 => Ok(Value::Argc),
            Value::Stack(12) if size == 4 => Ok(Value::Argv(0)),
            Value::Stack(n) => self.stack.get(&n).filter(|(width, _)| *width == size)
                .map(|(_, v)| v.clone()).ok_or_else(|| "uninitialized/overlapping Number spill".into()),
            Value::Argv(0) if size == 4 => Ok(Value::Receiver),
            Value::Field(n) if size == 8 => {
                self.touch(n)?;
                Ok(Value::Number(self.fields.get(&n).cloned().unwrap_or(Number::Field(n))))
            }
            Value::Argv(n) if size == 8 && self.offsets.contains(&(n as u32)) => Ok(Value::Number(Number::Arg(n as u32))),
            Value::Argv(n) if size == 4 && self.ints.contains(&(n as u32)) => Ok(Value::IntArg(n as u32)),
            Value::Constant(a) if size == 4 && a == self.core + 32 => Ok(Value::Limit),
            Value::Constant(a) if size == 4 && a == self.core + 48 => Ok(self.frame.clone()),
            _ => Err(format!("unknown native Number read {a:?}/{size}")),
        }
    }
    /// Record a receiver field access. Two accesses at different offsets must not overlap (an 8-byte double).
    fn touch(&self, field: u32) -> Result<(), String> {
        let mut accessed = self.accessed.borrow_mut();
        if accessed.iter().any(|&o| o != field && o.abs_diff(field) < 8) { return Err("overlapping Number field accesses".into()); }
        accessed.insert(field);
        Ok(())
    }
    fn source(&self, i: &Instruction, n: u32) -> Result<Value, String> {
        match i.op_kind(n) {
            OpKind::Register => Ok(self.regs[gpr(i.op_register(n))?].clone()),
            OpKind::Memory => self.load(i, 4),
            OpKind::Immediate8 | OpKind::Immediate8to32 | OpKind::Immediate32 => Ok(Value::Constant(i.immediate(n) as u32)),
            _ => Err("unknown native Number scalar operand".into()),
        }
    }
    fn spill(&mut self, n: i32, size: usize, value: Value) -> Result<(), String> {
        if n < self.low || n % 4 != 0 || n.checked_add(size as i32).is_none_or(|end| end > 0) {
            return Err("Number write outside aligned nonescaping frame".into());
        }
        self.stack.retain(|&at, (width, _)| n >= at + *width as i32 || at >= n + size as i32);
        self.stack.insert(n, (size, value)); Ok(())
    }
    fn assign(&mut self, i: &Instruction, value: Value, size: usize) -> Result<(), String> {
        match i.op0_kind() {
            OpKind::Register if size == 4 => { self.regs[gpr(i.op0_register())?] = value; Ok(()) }
            OpKind::Memory if i.memory_size().size() == size => match self.address(i)? {
                Value::Stack(n) => self.spill(n, size, value),
                Value::Constant(a) if size == 4 && a == self.core + 48 => {
                    if let Value::Stack(n) = value {
                        if self.linked || n < self.low || n >= 0 { return Err("invalid Number MethodFrame link".into()); }
                        self.linked = true; self.frame = value;
                    } else if value == Value::Frame && self.linked && !self.restored {
                        self.frame = value; self.restored = true;
                    } else { return Err("unrestored Number MethodFrame write".into()); }
                    Ok(())
                }
                Value::Field(n) if size == 8 => {
                    let Value::Number(v) = value else { return Err("non-Number receiver field store".into()); };
                    self.touch(n)?;
                    self.fields.insert(n, v.clone());
                    self.store_log.push((n, v));
                    Ok(())
                }
                _ => Err("native Number write escapes stack/MethodFrame".into()),
            },
            _ => Err("unsupported Number destination".into()),
        }
    }
}

/// The argv layout the proof read: parameters packed from offset 4 with no alignment padding (an int32 takes
/// 4 bytes, a double 8). A gap before or between read slots is a run of parameters the code never reads;
/// their size is unknown, so they are assumed to be doubles (a gap must be a multiple of 8 bytes). A wrong
/// guess changes argc and the runtime `[SP+8] == argc` guard then keeps the native method running; nothing
/// else depends on it. Returns (argc, bytes of argv up to and including the last read slot, at least 4 for the receiver).
fn argv_layout(params: &[Type], offsets: &[u32]) -> Result<(u32, u32), String> {
    if params.len() != offsets.len() { return Err("Number parameter/offset count differs".into()); }
    // Receiver-field bindings are not argv slots; the receiver word itself (argv[0]) is always inside the layout.
    let mut slots: Vec<(u32, u32)> = offsets.iter().zip(params).filter(|(&o, _)| o & RECEIVER_FIELD == 0)
        .map(|(&o, t)| (o, if *t == Type::I32 { 4 } else { 8 })).collect();
    slots.sort_unstable();
    let (mut next, mut argc) = (4u32, 0u32);
    for &(offset, size) in &slots {
        if offset < next || (offset - next) % 8 != 0 { return Err("Number argv reads overlap or leave a gap that is not whole doubles".into()); }
        argc += (offset - next) / 8 + 1;
        next = offset + size;
    }
    if argc > 8 || next > 4 + 8 * 8 { return Err("Number argv layout exceeds eight parameters".into()); }
    Ok((argc, next))
}

/// Independently decode the complete guarded no-call native path, including dead
/// reads/writes and every callee save. `read_mask` reads the real guest m128 value;
/// it must not invent bytes based on a public source opcode/name.
pub fn verify_native(bytes: &[u8], entry: u32, core: u32, trace: &Trace,
    offsets: &[u32], read_mask: impl FnMut(u32) -> Result<[u8; 16], String>) -> Result<Verified, String> {
    verify_native_with_stores(bytes, entry, core, trace, offsets, &[], read_mask)
}

/// `offsets[i]` binds SSA parameter `i`: an argv byte offset, or `RECEIVER_FIELD | field` for a double read from
/// the receiver object. `stores` are the proposed native receiver-field stores as (field, SSA value id), in native
/// order; the proof requires exactly those stores, with exactly those values.
pub fn verify_native_with_stores(bytes: &[u8], entry: u32, core: u32, trace: &Trace,
    offsets: &[u32], stores: &[(u32, usize)], mut read_mask: impl FnMut(u32) -> Result<[u8; 16], String>) -> Result<Verified, String> {
    if core.checked_add(52).is_none() || bytes.len() < 8 || bytes.len() > 4096
        || entry.checked_add(bytes.len() as u32).is_none() || offsets.is_empty() || offsets.len() > 8 {
        return Err("invalid captured Number bounds".into());
    }
    let (argc, argv_size) = argv_layout(&trace.params, offsets)?;
    let (wanted, shape, effects, store_trees) = expression(trace, offsets, stores)?;
    let argv_bound = |o: &&u32| **o & RECEIVER_FIELD == 0;
    let doubles: Vec<u32> = offsets.iter().zip(&trace.params).filter(|(o, t)| **t == Type::F64 && argv_bound(o)).map(|(o, _)| *o).collect();
    let ints: Vec<u32> = offsets.iter().zip(&trace.params).filter(|(o, t)| **t == Type::I32 && argv_bound(o)).map(|(o, _)| *o).collect();
    let mut m = Machine { core, regs: (0..8).map(|r| if r == ESP { Value::Stack(0) } else { Value::Saved(r) }).collect(),
        xmm: vec![None; 8], stack: BTreeMap::new(), frame: Value::Frame, linked: false, restored: false,
        low: 0, guard: None, cmp: None, offsets: &doubles, ints: &ints,
        fields: BTreeMap::new(), accessed: Default::default(), store_log: Vec::new() };
    let mut offset = 0;
    let mut visited = BTreeSet::new();
    let mut fld = None;
    let mut masks = Vec::new();
    let mut statics: Vec<(u32, u64)> = Vec::new();
    let int_result = matches!(wanted, Number::Trunc(_));
    let mut range_checked = false;
    let mut immediates: Vec<u64> = Vec::new();
    let mut arithmetic = Vec::new();
    for count in 1..=128 {
        if offset >= bytes.len() || !visited.insert(offset) { return Err("Number native path loops/leaves capture".into()); }
        let mut decoder = Decoder::with_ip(32, &bytes[offset..], entry as u64 + offset as u64, DecoderOptions::NONE);
        let i = decoder.decode();
        if i.is_invalid() || i.has_lock_prefix() || i.segment_prefix() != Register::None
            || ((i.has_rep_prefix() || i.has_repne_prefix()) && !matches!(i.mnemonic(), M::Movq | M::Movsd | M::Cvtsi2sd | M::Cvttsd2si | M::Addsd | M::Subsd | M::Mulsd | M::Divsd)) {
            return Err("unproved native Number encoding/prefix".into());
        }
        offset += i.len();
        match i.mnemonic() {
            M::Push if i.op0_kind() == OpKind::Register => {
                let v = m.source(&i, 0)?;
                let Value::Stack(sp) = m.regs[ESP] else { return Err("unproved Number ESP".into()); };
                let sp = sp.checked_sub(4).filter(|&n| n >= -4080).ok_or("Number stack overflow")?;
                m.low = m.low.min(sp); m.regs[ESP] = Value::Stack(sp); m.spill(sp, 4, v)?;
            }
            M::Pop if i.op0_kind() == OpKind::Register && i.op0_register() != Register::ESP => {
                let Value::Stack(sp) = m.regs[ESP] else { return Err("unproved Number pop".into()); };
                let v = m.stack.get(&sp).filter(|(size, _)| *size == 4).ok_or("uninitialized Number pop")?.1.clone();
                m.assign(&i, v, 4)?; m.regs[ESP] = Value::Stack(sp + 4);
            }
            M::Mov => { let v = m.source(&i, 1)?; m.assign(&i, v, 4)?; }
            M::Lea if i.op1_kind() == OpKind::Memory => { let v = m.address(&i)?; m.assign(&i, v, 4)?; }
            M::Add | M::Sub => {
                let (Value::Stack(sp), Value::Constant(n)) = (m.source(&i, 0)?, m.source(&i, 1)?) else {
                    return Err("unproved Number native integer arithmetic".into()); };
                let sp = if i.mnemonic() == M::Add { sp.checked_add(n as i32) } else { sp.checked_sub(n as i32) }
                    .filter(|&n| (-4080..=0).contains(&n)).ok_or("Number frame outside bounded stack")?;
                m.low = m.low.min(sp); m.assign(&i, Value::Stack(sp), 4)?; m.cmp = None;
            }
            M::Cmp => m.cmp = Some((m.source(&i, 0)?, m.source(&i, 1)?)),
            M::Jae if m.guard.is_none() => {
                let Some((Value::Stack(frame), Value::Limit)) = m.cmp.take() else { return Err("not a Number stack-limit guard".into()); };
                if !m.linked || m.restored || m.frame != Value::Stack(frame) || frame < m.low || frame >= 0 {
                    return Err("unproved Number MethodFrame guard".into());
                }
                let target = i.near_branch32().checked_sub(entry).ok_or("Number guard leaves capture")? as usize;
                if target <= offset || target >= bytes.len() { return Err("Number guard is backward/outside".into()); }
                m.guard = Some(frame); offset = target;
            }
            M::Movq => {
                if i.op0_kind() == OpKind::Register {
                    let dst = xmm(i.op0_register())?;
                    let v = if i.op1_kind() == OpKind::Memory { m.load(&i, 8)? } else {
                        Value::Number(m.xmm[xmm(i.op1_register())?].clone().ok_or("unknown Number XMM")?) };
                    let Value::Number(v) = v else { return Err("non-Number movq".into()); };
                    m.xmm[dst] = Some(v);
                } else {
                    let v = m.xmm[xmm(i.op1_register())?].clone().ok_or("unknown Number SIMD spill")?;
                    m.assign(&i, Value::Number(v), 8)?;
                }
            }
            M::Movsd if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Memory => {
                // A NanoJIT double constant: an 8-byte load from a static pool slot. The real guest bytes are
                // read now and rechecked on every call; the SSA `immd` must equal them bit for bit.
                if i.memory_size().size() != 8 { return Err("Number constant width differs".into()); }
                let Value::Constant(address) = m.address(&i)? else { return Err("nonstatic Number constant".into()); };
                if address.checked_add(8).is_none() || address & 4095 > 4096 - 8 { return Err("wrapping/page-crossing Number constant".into()); }
                let bytes = read_mask(address)?;
                let bits = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                statics.push((address, bits));
                m.xmm[xmm(i.op0_register())?] = Some(Number::Const(bits));
            }
            M::Xorpd if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Register && i.op0_register() == i.op1_register() => {
                // The zeroing idiom NanoJIT emits before a scalar int->double convert.
                m.xmm[xmm(i.op0_register())?] = Some(Number::Const(0)); immediates.push(0);
            }
            M::Cvtsi2sd if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Register => {
                // int32 -> double is always exact (no MXCSR flag, rounding mode irrelevant). Only a value the
                // proof knows (a code immediate) is accepted; the converted double is a tracked constant.
                match m.regs[gpr(i.op1_register())?] {
                    Value::Constant(n) => {
                        let bits = (n as i32 as f64).to_bits();
                        m.xmm[xmm(i.op0_register())?] = Some(Number::Const(bits)); immediates.push(bits);
                    }
                    // a proven int32 argv argument: the exact conversion of a value read from argv
                    Value::IntArg(offset) => m.xmm[xmm(i.op0_register())?] = Some(Number::FromInt(offset)),
                    _ => return Err("cvtsi2sd of an unproved value".into()),
                }
            }
            M::Cvttsd2si if int_result && i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Register => {
                let v = m.xmm[xmm(i.op1_register())?].clone().ok_or("unknown Number truncation source")?;
                m.regs[gpr(i.op0_register())?] = Value::Number(Number::Trunc(Box::new(v)));
            }
            M::Jne if int_result && !range_checked => {
                // NanoJIT's int() idiom: `cmp eax,0x80000000; jne done` skips a helper call for out-of-range
                // inputs. The proof follows the taken branch; the adapter refuses whenever |x| >= 2^31, so the
                // skipped call (bound with the native bytes) is only ever run by the native method.
                let Some((Value::Number(Number::Trunc(_)), Value::Constant(0x8000_0000))) = m.cmp.take() else {
                    return Err("not the Number int range check".into()); };
                let target = i.near_branch32().checked_sub(entry).ok_or("Number range check leaves capture")? as usize;
                if target <= offset || target >= bytes.len() { return Err("Number range check is backward/outside".into()); }
                range_checked = true; offset = target;
            }
            M::Movaps if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Register => {
                // Full-register copy: the low double is tracked, the upper half is caller-volatile.
                let v = m.xmm[xmm(i.op1_register())?].clone().ok_or("unknown Number XMM copy source")?;
                m.xmm[xmm(i.op0_register())?] = Some(v);
            }
            M::Xorpd if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Memory => {
                if i.memory_size().size() != 16 { return Err("Number XOR mask width differs".into()); }
                let Value::Constant(address) = m.address(&i)? else { return Err("nonstatic Number XOR mask".into()); };
                if address % 16 != 0 || address.checked_add(16).is_none() { return Err("unaligned/wrapping Number XOR mask".into()); }
                let mask = read_mask(address)?;
                if u64::from_le_bytes(mask[..8].try_into().unwrap()) != 1 << 63 { return Err("Number XOR is not exact sign negation".into()); }
                masks.push((address, mask));
                let dst = xmm(i.op0_register())?;
                let v = m.xmm[dst].take().ok_or("unknown Number XOR input")?;
                m.xmm[dst] = Some(Number::Neg(Box::new(v)));
            }
            M::Addsd | M::Subsd | M::Mulsd | M::Divsd if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Register => {
                let dst = xmm(i.op0_register())?;
                let a = m.xmm[dst].clone().ok_or("unknown Number SSE destination")?;
                let b = m.xmm[xmm(i.op1_register())?].clone().ok_or("unknown Number SSE source")?;
                let kind = match i.mnemonic() { M::Addsd => 2, M::Subsd => 3, M::Mulsd => 4, _ => 5 };
                let v = Number::Binary(kind, Box::new(a), Box::new(b));
                arithmetic.push(v.clone()); m.xmm[dst] = Some(v);
            }
            M::Fst if i.op0_kind() == OpKind::Memory && fld.is_some() => {
                // `fst qword [receiver+f], st(0)`: stores the pushed value without popping. It is also the last x87
                // instruction, so it owns the FPU's last-instruction/data pointers.
                if i.memory_size().size() != 8 { return Err("Number FST width differs".into()); }
                let Value::Field(field) = m.address(&i)? else { return Err("FST outside a receiver field".into()); };
                let (value, _, _): (Number, u32, DataPointer) = fld.take().unwrap();
                m.touch(field)?; m.fields.insert(field, value.clone()); m.store_log.push((field, value.clone()));
                fld = Some((value, i.ip32(), DataPointer::Field(field)));
            }
            M::Fild if i.op0_kind() == OpKind::Memory && fld.is_none() => {
                // FILD of a spilled int32 argument pushes the exact value, identical to FLD of its double.
                let Value::IntArg(offset) = m.load(&i, 4)? else { return Err("FILD of an unproved value".into()); };
                let Value::Stack(n) = m.address(&i)? else { return Err("FILD of a non-stack operand".into()); };
                fld = Some((Number::FromInt(offset), i.ip32(), DataPointer::Stack(n)));
            }
            M::Fld if i.op0_kind() == OpKind::Memory && fld.is_none() => {
                let Value::Number(value) = m.load(&i, 8)? else { return Err("non-Number native FLD".into()); };
                let dp = match m.address(&i)? { Value::Argv(n) => DataPointer::Argv(n as u32), Value::Stack(n) => DataPointer::Stack(n),
                    Value::Field(n) => DataPointer::Field(n),
                    _ => return Err("unknown Number FLD data pointer".into()) };
                fld = Some((value, i.ip32(), dp));
            }
            M::Ret if i.op_count() == 0 => {
                let frame = m.guard.ok_or("Number native lacks stack-limit guard")?;
                if m.regs[ESP] != Value::Stack(0) || !m.restored || m.frame != Value::Frame
                    || [EBX, EBP, ESI, EDI].iter().any(|&r| m.regs[r] != Value::Saved(r)) {
                    return Err("Number native does not restore frame/callee saves".into());
                }
                if m.store_log != store_trees { return Err("Number receiver stores differ from the proposed stores".into()); }
                let fields: Vec<u32> = m.accessed.borrow().iter().copied().collect();
                let (fld_ip, data_pointer) = if int_result {
                    // Std.int: EAX holds the truncation, no x87 value is pushed, and the native range check
                    // (the `jne` over the helper call) was taken.
                    if fld.is_some() || !range_checked || m.regs[EAX] != Value::Number(wanted.clone()) || arithmetic != effects {
                        return Err("Number native/LIR int result or FP effects differ".into());
                    }
                    (0, DataPointer::Stack(-4))
                } else {
                    let (value, fld_ip, data_pointer) = fld.ok_or("Number native has no x87 return")?;
                    // The ordered flag-raising SSE sequence, with exact operand order, must match the SSA.
                    // Operand order decides which NaN payload propagates, and every op sets sticky flags.
                    if value != wanted || arithmetic != effects || range_checked {
                        return Err("Number native/LIR result or FP effects differ".into());
                    }
                    (fld_ip, data_pointer)
                };
                // Omitted MethodFrame writes cannot overlap the native code or mask.
                let overlaps = |a: u32, n: u32, b: u32, s: u32| (a as u64) < b as u64 + s as u64 && (b as u64) < a as u64 + n as u64;
                if overlaps(entry, offset as u32, core + 48, 4) || masks.iter().any(|(a, _)| overlaps(*a, 16, core + 48, 4))
                    || statics.iter().any(|(a, _)| overlaps(*a, 8, core + 48, 4)) {
                    return Err("Number static dependency aliases MethodFrame".into());
                }
                masks.sort_unstable(); masks.dedup(); statics.sort_unstable(); statics.dedup(); immediates.sort_unstable(); immediates.dedup();
                if !constants_bound(&shape, &statics, &immediates) {
                    return Err("Number constants are not bound to native pool slots or code immediates".into());
                }
                return Ok(Verified { len: offset, stack_low: m.low, frame_offset: frame, instructions: count,
                    fld_ip, data_pointer, shape, masks, statics, immediates, argv_size, argc, int_result, fields });
            }
            M::Nop => {}
            other => return Err(format!("unsupported native Number effect {other:?}")),
        }
    }
    Err("native Number instruction limit".into())
}

#[derive(Clone, Debug)]
pub struct Abi { pub entry: u32, pub core: u32, pub guest_size: u32, pub argc: u32 }
pub struct Output { pub wasm: Vec<u8>, pub region: RegionOut }
// wasm local layout: params (cpu, head), then the i64 locals, then the i32 locals. The emitter places
// i64 locals first, so every i32 index is shifted by N_VALUES.
const N_VALUES: u32 = (STEP0 + MAX_STEPS) as u32; // argv inputs, constants, then step results
const SP: u32 = 2 + N_VALUES;
const ARGV: u32 = SP + 1;
const RET: u32 = SP + 2;
const BUDGET: u32 = SP + 3;
const LOW: u32 = SP + 4;
const PAGE: u32 = SP + 5;
const READ_PERMISSION: u32 = SP + 6;
const WRITE_PERMISSION: u32 = SP + 7;
const NEXT: u32 = SP + 8; // physical x87 register the FLD fills
const SHIFT: u32 = SP + 9; // 2 * NEXT: tag-word shift
const RECEIVER: u32 = SP + 10; // argv[0], the receiver object pointer (only when receiver fields are used)
const OBJECT_LOW: u32 = SP + 11;
const OBJECT_END: u32 = SP + 12;
const ADDRESS: u32 = SP + 13;
const N_I32: u32 = 14;
fn value(i: usize) -> u32 { 2 + i as u32 }
/// MXCSR bits that must be: rounding control 0, FTZ 0, DAZ 0, PE (sticky inexact) 1. This is the
/// interpreter's own host fast path (`sse_fp::Env::fast`) and the emulator JIT's inline scalar FP
/// (`jit_fp.rs`), which this adapter mirrors guard for guard.
const MX_MASK: u32 = 0x6000 | 0x8000 | 0x40 | 0x20;
const MX_FAST: u32 = 0x20;
fn reject(c: &mut Code) { c.if_(0x40).u32c(S_RETRY).op(op::RETURN).end(); }
fn cpu(c: &mut Code, o: u32) { c.lget(0).mem(op::I32_LOAD, o); }
fn permission(c: &mut Code, l: &Layout, address: u32, size: u32, write: bool) {
    c.lget(address).u32c(4095).op(op::I32_AND).u32c(4096 - size).op(op::I32_GT_U); reject(c);
    c.lget(address).u32c(12).op(op::I32_SHR_U).lset(PAGE);
    c.lget(PAGE).mem(op::I32_LOAD8_U, l.rtab).lset(READ_PERMISSION);
    if write {
        c.lget(PAGE).mem(op::I32_LOAD8_U, l.wtab).lset(WRITE_PERMISSION);
        c.lget(READ_PERMISSION).lget(WRITE_PERMISSION).op(op::I32_AND);
    } else {
        c.lget(READ_PERMISSION);
    }
    c.u32c(1).op(op::I32_AND).op(op::I32_EQZ); reject(c);
}
fn disjoint_stack(c: &mut Code, address: u32, size: u32) {
    c.lget(LOW).u32c(address + size).op(op::I32_LT_U)
        .lget(SP).u32c(16).op(op::I32_ADD).u32c(address).op(op::I32_GT_U).op(op::I32_AND); reject(c);
}

fn argv_slot(o: u32, size: u32, argv_size: u32) -> bool { o >= 4 && o % 4 == 0 && o.checked_add(size).is_some_and(|end| end <= argv_size) }
/// Every argv offset the adapter will load must be a slot inside the proven argv, and a program must be a
/// well-formed bounded straight-line SSA (operands strictly earlier values).
fn shape_in_bounds(shape: &Shape, argv_size: u32, fields: &[u32]) -> bool {
    match shape {
        Shape::Simple(o) => o.kind <= 5 && argv_slot(o.a, 8, argv_size) && argv_slot(o.b, 8, argv_size),
        Shape::Program(p) => {
            let inputs_ok = !p.inputs.is_empty() && p.inputs.len() <= MAX_INPUTS && p.ints.len() == p.inputs.len()
                && p.inputs.iter().zip(&p.ints).all(|(&o, &int)| if o & RECEIVER_FIELD != 0 {
                    !int && fields.contains(&(o & !RECEIVER_FIELD))
                } else {
                    argv_slot(o, if int { 4 } else { 8 }, argv_size)
                });
            // A value index names an existing argv/field input, an existing constant, or a step before `before`.
            let exists = |v: u32, before: usize| { let v = v as usize;
                v < p.inputs.len() || (CONST0..CONST0 + p.consts.len()).contains(&v) || (STEP0..STEP0 + before).contains(&v) };
            inputs_ok && p.steps.len() <= MAX_STEPS && p.consts.len() <= MAX_CONSTS
                && p.steps.iter().enumerate().all(|(i, s)| (1..=6).contains(&s.kind) && exists(s.a, i) && exists(s.b, i)
                    && (s.kind != 1 || s.a == s.b)
                    && (s.kind != 6 || (s.a == s.b && i + 1 == p.steps.len() && p.result as usize == STEP0 + i)))
                && exists(p.result, p.steps.len())
                && p.stores.len() <= 4
                && p.stores.iter().all(|&(f, v)| f <= 4088 && f % 4 == 0 && fields.contains(&f) && exists(v, p.steps.len()))
        }
    }
}

/// Every program constant is bound: a rechecked static slot or a code immediate. No static slot may be read
/// without being used (dead reads are not skipped), and a shape with no constants reads no slot at all.
fn constants_bound(shape: &Shape, statics: &[(u32, u64)], immediates: &[u64]) -> bool {
    match shape {
        Shape::Simple(_) => statics.is_empty(),
        Shape::Program(p) => p.consts.iter().all(|c| statics.iter().any(|(_, bits)| bits == c) || immediates.contains(c))
            && statics.iter().all(|(_, bits)| p.consts.contains(bits)),
    }
}
/// i32: the bits in local `t` are a normal double, or a zero when `zero_ok`.
fn normal(c: &mut Code, t: u32, zero_ok: bool) {
    c.lget(t).i64c(1).op(op::I64_SHL).i64c(1 << 53).op(op::I64_SUB).i64c(0x7fe_i64 << 53).op(op::I64_LT_U);
    if zero_ok { is_zero(c, t); c.op(op::I32_OR); }
}
/// i32: the bits in local `t` are +0 or -0.
fn is_zero(c: &mut Code, t: u32) { c.lget(t).i64c(1).op(op::I64_SHL).op(op::I64_EQZ); }
/// i32: the result bits have a biased exponent of at least 2 and are finite.
fn result_ok(c: &mut Code, t: u32) {
    c.lget(t).i64c(1).op(op::I64_SHL).i64c(2 << 53).op(op::I64_SUB).i64c(0x7fd_i64 << 53).op(op::I64_LT_U);
}

/// What the adapter evaluates: argv inputs (offset, is-int32), constants, steps and the returned value
/// index. A `Simple` shape is a program of at most one step over double inputs; value indexes follow `Program`.
struct Lowered { inputs: Vec<(u32, bool)>, consts: Vec<u64>, steps: Vec<Step>, result: usize, stores: Vec<(u32, usize)> }
fn lowering(shape: &Shape) -> Lowered {
    let doubles = |v: &[u32]| v.iter().map(|&o| (o, false)).collect::<Vec<_>>();
    match shape {
        Shape::Program(p) => Lowered { inputs: p.inputs.iter().copied().zip(p.ints.iter().copied()).collect(),
            consts: p.consts.clone(), steps: p.steps.clone(), result: p.result as usize,
            stores: p.stores.iter().map(|&(f, v)| (f, v as usize)).collect() },
        Shape::Simple(o) => match o.kind {
            0 => Lowered { inputs: doubles(&[o.a]), consts: vec![], steps: vec![], result: 0, stores: vec![] },
            1 => Lowered { inputs: doubles(&[o.a]), consts: vec![], steps: vec![Step { kind: 1, a: 0, b: 0 }], result: STEP0, stores: vec![] },
            kind if o.a == o.b => Lowered { inputs: doubles(&[o.a]), consts: vec![], steps: vec![Step { kind, a: 0, b: 0 }], result: STEP0, stores: vec![] },
            kind => Lowered { inputs: doubles(&[o.a, o.b]), consts: vec![], steps: vec![Step { kind, a: 0, b: 1 }], result: STEP0, stores: vec![] },
        },
    }
}

/// The interpreter-fast-path FP evaluation. Every guard precedes the first store, so any refusal leaves
/// the whole Cpu untouched and the original native method runs. Anything outside the fast configuration
/// (MXCSR not RNE/no-FTZ/DAZ/PE-sticky, a denormal/NaN/infinite operand or result, an inexact-zero
/// result, a full x87 stack) is refused rather than approximated.
fn emit_fp(c: &mut Code, layout: &Layout, proof: &Verified, lowered: &Lowered) {
    let Lowered { inputs, consts, steps, result, stores } = lowered;
    let fpu = core::mem::offset_of!(Cpu, fpu) as u32;
    let field = |o: usize| fpu + o as u32;
    let (top, tag, sw) = (field(core::mem::offset_of!(Fpu, top)), field(core::mem::offset_of!(Fpu, tag)), field(core::mem::offset_of!(Fpu, sw)));
    let (last_ip, last_dp) = (field(core::mem::offset_of!(Fpu, last_ip)), field(core::mem::offset_of!(Fpu, last_dp)));
    let st = core::mem::offset_of!(Fpu, st);
    let (mant, se) = (field(st + core::mem::offset_of!(F80, mant)), field(st + core::mem::offset_of!(F80, se)));
    const _: () = assert!(core::mem::size_of::<F80>() == 16);

    cpu(c, layout.cpu_mxcsr); c.u32c(MX_MASK).op(op::I32_AND).u32c(MX_FAST).op(op::I32_NE); reject(c);
    if !proof.int_result {
        // FLD pushes into physical register TOP-1, which must be empty (otherwise native would overflow the stack).
        c.lget(0).mem(op::I32_LOAD8_U, top).u32c(7).op(op::I32_GT_U); reject(c);
        c.lget(0).mem(op::I32_LOAD8_U, top).i32c(1).op(op::I32_SUB).u32c(7).op(op::I32_AND).lset(NEXT);
        c.lget(NEXT).i32c(1).op(op::I32_SHL).lset(SHIFT);
        c.lget(0).mem(op::I32_LOAD16_U, tag).lget(SHIFT).op(op::I32_SHR_U).i32c(3).op(op::I32_AND).i32c(3).op(op::I32_NE); reject(c);
    }

    for (slot, &(offset, int)) in inputs.iter().enumerate() {
        if offset & RECEIVER_FIELD != 0 {
            c.lget(RECEIVER).mem(op::I64_LOAD, layout.guest_base + (offset & !RECEIVER_FIELD)).lset(value(slot));
        } else if int {
            // an int32 argument is used only through i2d, which is exact: convert while loading
            c.lget(ARGV).mem(op::I32_LOAD, layout.guest_base + offset).op(op::F64_CONVERT_I32_S).op(op::I64_REINTERPRET_F64).lset(value(slot));
        } else {
            c.lget(ARGV).mem(op::I64_LOAD, layout.guest_base + offset).lset(value(slot));
        }
    }
    // Constants were proven equal to their static pool slots (and the slots are rechecked above), so
    // the immediate is the value.
    for (slot, &bits) in consts.iter().enumerate() { c.i64c(bits as i64).lset(value(CONST0 + slot)); }
    for (i, s) in steps.iter().enumerate() {
        let (a, b, r) = (value(s.a as usize), value(s.b as usize), value(STEP0 + i));
        if s.kind == 1 { c.lget(a).i64c(i64::MIN).op(op::I64_XOR).lset(r); continue; }
        if s.kind == 6 {
            // cvttsd2si of a normal/zero double with |x| < 2^31: truncation toward zero cannot overflow, so the
            // result is never the 0x80000000 "indefinite" that sends native code to its helper, and PE (already
            // sticky under the fast MXCSR) is the only flag a fractional input could raise.
            normal(c, a, true); c.op(op::I32_EQZ); reject(c);
            c.lget(a).i64c(1).op(op::I64_SHL).i64c(((1023u64 + 31) << 53) as i64).op(op::I64_LT_U).op(op::I32_EQZ); reject(c);
            c.lget(a).op(op::F64_REINTERPRET_I64).op(op::I32_TRUNC_F64_S).op(op::I64_EXTEND_I32_S).lset(r);
            continue;
        }
        normal(c, a, true); c.op(op::I32_EQZ); reject(c);
        normal(c, b, s.kind != 5); c.op(op::I32_EQZ); reject(c);
        c.lget(a).op(op::F64_REINTERPRET_I64).lget(b).op(op::F64_REINTERPRET_I64);
        c.op(match s.kind { 2 => op::F64_ADD, 3 => op::F64_SUB, 4 => op::F64_MUL, _ => op::F64_DIV });
        c.op(op::I64_REINTERPRET_F64).lset(r);
        // finite with exponent >= 2, or an exactly zero result that cannot have been an underflow
        result_ok(c, r); is_zero(c, r);
        match s.kind {
            2 | 3 => {}
            4 => { is_zero(c, a); is_zero(c, b); c.op(op::I32_OR).op(op::I32_AND); }
            _ => { is_zero(c, a); c.op(op::I32_AND); }
        }
        c.op(op::I32_OR).op(op::I32_EQZ); reject(c);
    }
    let last = value(*result);
    // Every stored double must be normal or zero (FST/FLD of a denormal, NaN or infinity changes x87 flags and
    // quiets NaNs): checked for all stores before the first one is written.
    for &(_, v) in stores { normal(c, value(v), true); c.op(op::I32_EQZ); reject(c); }
    if !proof.int_result { normal(c, last, true); c.op(op::I32_EQZ); reject(c); } // FLD of a denormal/NaN/inf sets flags: native only
    for &(field, v) in stores {
        c.lget(RECEIVER).lget(value(v)).mem(op::I64_STORE, layout.guest_base + field);
    }
    if proof.int_result {
        // Commit the int32: EAX only. No x87 state is touched.
        c.lget(0).lget(last).op(op::I32_WRAP_I64).mem(op::I32_STORE, layout.cpu_r + EAX as u32 * 4);
        return;
    }

    // ---- commit: the architectural effect of one FLD m64 of a normal or zero double ----
    c.lget(0).lget(0).mem(op::I32_LOAD16_U, sw).i32c(!0x0200).op(op::I32_AND).mem(op::I32_STORE16, sw);
    c.lget(0).u32c(proof.fld_ip).mem(op::I32_STORE, last_ip);
    c.lget(0);
    match proof.data_pointer {
        DataPointer::Argv(n) => { c.lget(ARGV).u32c(n).op(op::I32_ADD); }
        DataPointer::Stack(n) => { c.lget(SP).i32c(n).op(op::I32_ADD); }
        DataPointer::Field(n) => { c.lget(RECEIVER).u32c(n).op(op::I32_ADD); }
    }
    c.mem(op::I32_STORE, last_dp);
    // register image: mantissa keeps the explicit integer bit unless the value is zero
    c.lget(0).lget(NEXT).i32c(4).op(op::I32_SHL).op(op::I32_ADD).lset(PAGE);
    c.lget(PAGE)
        .lget(last).i64c(0xf_ffff_ffff_ffff).op(op::I64_AND).i64c(11).op(op::I64_SHL)
        .lget(last).i64c(1).op(op::I64_SHL).i64c(0).op(op::I64_NE).op(op::I64_EXTEND_I32_U).i64c(63).op(op::I64_SHL)
        .op(op::I64_OR).mem(op::I64_STORE, mant);
    // sign/exponent: rebias 1023 -> 16383 for a normal value, plain sign for zero
    c.lget(PAGE)
        .lget(last).i64c(63).op(op::I64_SHR_U).op(op::I32_WRAP_I64).i32c(15).op(op::I32_SHL)
        .lget(last).i64c(52).op(op::I64_SHR_U).i64c(0x7ff).op(op::I64_AND).op(op::I32_WRAP_I64).u32c(16383 - 1023).op(op::I32_ADD)
        .i32c(0).lget(last).i64c(1).op(op::I64_SHL).i64c(0).op(op::I64_NE).op(op::I32_SUB).op(op::I32_AND)
        .op(op::I32_OR).mem(op::I32_STORE16, se);
    // tag: 01 (zero) or 00 (valid) in this register's two bits
    c.lget(0).lget(0).mem(op::I32_LOAD16_U, tag).i32c(3).lget(SHIFT).op(op::I32_SHL).i32c(-1).op(op::I32_XOR).op(op::I32_AND);
    is_zero(c, last); c.lget(SHIFT).op(op::I32_SHL).op(op::I32_OR).mem(op::I32_STORE16, tag);
    c.lget(0).lget(NEXT).mem(op::I32_STORE8, top);
}

/// Only a Verified value returned by the independent effect proof is accepted.
/// Native dependencies must additionally be bound by install_precompiled.
pub fn compile(layout: &Layout, abi: &Abi, proof: &Verified) -> Result<Output, Error> {
    let bad = |s| error(0, s);
    // argc is the proven argv read set (the contiguous parameter layout from offset 4), never a capture-supplied
    // number. A real call with a different argc (unread trailing parameters, optional arguments) fails the
    // runtime `[SP+8] == argc` guard and stays native.
    if proof.argc != abi.argc { return Err(bad("Number argc differs from the proven argv read set")); }
    if (abi.argc == 0 && proof.fields.is_empty()) || abi.argc > 8 || abi.guest_size < 8192 || proof.instructions == 0 || proof.instructions > 128
        || !(-4080..=-4).contains(&proof.stack_low) || !(proof.stack_low..=-4).contains(&proof.frame_offset)
        || proof.len == 0 || proof.len > 4096
        || proof.argv_size < 4 + 4 * abi.argc || proof.argv_size > 4 + 8 * abi.argc
        || (!proof.int_result && !(abi.entry..abi.entry.saturating_add(proof.len as u32)).contains(&proof.fld_ip))
        || (!proof.int_result && match proof.data_pointer { DataPointer::Argv(n) => n.checked_add(8).is_none_or(|end| end > proof.argv_size),
            DataPointer::Field(n) => !proof.fields.contains(&n),
            DataPointer::Stack(n) => n < proof.stack_low || n.checked_add(8).is_none_or(|end| end > 0) })
        || proof.int_result != lowering(&proof.shape).steps.last().is_some_and(|s| s.kind == 6)
        || !shape_in_bounds(&proof.shape, proof.argv_size, &proof.fields)
        || proof.fields.len() > 16 || proof.fields.iter().any(|&f| f > 4088 || f % 4 != 0)
        || abi.entry.checked_add(proof.len as u32).is_none_or(|n| n > abi.guest_size)
        || abi.core.checked_add(52).is_none_or(|n| n > abi.guest_size)
        || proof.masks.len() > 32 || proof.masks.iter().any(|(a, _)| a % 16 != 0 || a.checked_add(16).is_none_or(|n| n > abi.guest_size))
        || proof.statics.len() > 2 * MAX_CONSTS
        || proof.statics.iter().any(|(a, _)| a & 4095 > 4096 - 8 || a.checked_add(8).is_none_or(|n| n > abi.guest_size))
        || !constants_bound(&proof.shape, &proof.statics, &proof.immediates) {
        return Err(bad("invalid verified Number adapter bounds"));
    }
    let native = Layout::for_cpu(layout.guest_base, layout.rtab, layout.wtab, layout.cache);
    if [layout.cpu_r, layout.cpu_eip, layout.cpu_budget, layout.cpu_mxcsr] != [native.cpu_r, native.cpu_eip, native.cpu_budget, native.cpu_mxcsr] {
        return Err(bad("Number adapter requires native Cpu layout"));
    }
    let pages = (abi.guest_size as u64).div_ceil(4096);
    let mut allocations = vec![(layout.guest_base as u64, abi.guest_size as u64), (layout.rtab as u64, pages), (layout.wtab as u64, pages)];
    // With a live chain cache the successful return tail-calls the compiled caller region instead of
    // bouncing through the Rust dispatcher; the cache is part of the trusted extents.
    if layout.cache != 0 { allocations.push((layout.cache as u64, CACHE_ENTRIES as u64 * 16)); }
    let mut end = 0;
    for (i, &(a, n)) in allocations.iter().enumerate() {
        if a + n > 1 << 32 { return Err(bad("Number allocation exceeds wasm32 memory")); }
        for &(b, s) in &allocations[..i] { if a < b + s && b < a + n { return Err(bad("Number allocations overlap")); } }
        end = end.max(a + n);
    }
    let mut module = ModuleBuilder::new();
    module.import_memory("env", "memory", end.div_ceil(65536) as u32);
    if layout.cache != 0 { module.import_table("env", "table", 0); }
    let signature = module.ty(&[I32, I32], &[I32]);
    let mut c = Code::new();
    cpu(&mut c, layout.cpu_eip); c.u32c(abi.entry).op(op::I32_NE).lget(1).op(op::I32_OR); reject(&mut c);
    cpu(&mut c, layout.cpu_budget); c.ltee(BUDGET).u32c(proof.instructions).op(op::I32_LT_S); reject(&mut c);
    cpu(&mut c, layout.cpu_r + ESP as u32 * 4);
    let stack_size = proof.stack_low.unsigned_abs() + 16;
    c.ltee(SP).u32c(proof.stack_low.unsigned_abs()).op(op::I32_SUB).ltee(LOW)
        .u32c(abi.guest_size - stack_size).op(op::I32_GT_U); reject(&mut c);
    c.lget(LOW).u32c(abi.core + 52).op(op::I32_LT_U); reject(&mut c);
    disjoint_stack(&mut c, abi.entry, proof.len as u32);
    // The stack spans a bounded number of aligned pages. Check each page;
    // a single base-page check would miss the upper return/frame slots.
    let page_count = (proof.stack_low.unsigned_abs() / 4096) + 2;
    for page in 0..page_count {
        c.lget(LOW).u32c(page * 4096).op(op::I32_ADD).u32c(12).op(op::I32_SHR_U).lset(PAGE);
        c.lget(PAGE).mem(op::I32_LOAD8_U, layout.rtab).lset(READ_PERMISSION);
        c.lget(PAGE).mem(op::I32_LOAD8_U, layout.wtab).lset(WRITE_PERMISSION);
        c.lget(READ_PERMISSION).lget(WRITE_PERMISSION).op(op::I32_AND).u32c(1).op(op::I32_AND).op(op::I32_EQZ);
        reject(&mut c);
    }
    let core = abi.core + 32;
    if core & 4095 > 4096 - 20 { return Err(bad("Number core fields cross page")); }
    c.u32c(core >> 12).mem(op::I32_LOAD8_U, layout.rtab)
        .u32c(core >> 12).mem(op::I32_LOAD8_U, layout.wtab).op(op::I32_AND).u32c(1).op(op::I32_AND).op(op::I32_EQZ); reject(&mut c);
    c.lget(SP).u32c(proof.frame_offset.unsigned_abs()).op(op::I32_SUB)
        .u32c(core).mem(op::I32_LOAD, layout.guest_base).op(op::I32_LT_U); reject(&mut c);
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base + 8).u32c(abi.argc).op(op::I32_NE); reject(&mut c);
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base + 12).ltee(ARGV)
        .lget(SP).u32c(4).op(op::I32_ADD).op(op::I32_LT_U); reject(&mut c);
    let argv_size = proof.argv_size;
    c.lget(ARGV).u32c(abi.guest_size - argv_size).op(op::I32_GT_U); reject(&mut c);
    permission(&mut c, layout, ARGV, argv_size, false);
    for (address, bytes) in &proof.masks {
        disjoint_stack(&mut c, *address, 16);
        // Mask is 16-byte aligned and hence cannot straddle a page.
        c.u32c(*address >> 12).mem(op::I32_LOAD8_U, layout.rtab).u32c(1).op(op::I32_AND).op(op::I32_EQZ); reject(&mut c);
        for at in [0, 8] {
            c.u32c(*address).mem(op::I64_LOAD, layout.guest_base + at)
                .i64c(u64::from_le_bytes(bytes[at as usize..at as usize + 8].try_into().unwrap()) as i64).op(op::I64_NE); reject(&mut c);
        }
    }
    for (address, bits) in &proof.statics {
        // A constant-pool slot never aliases the omitted frame writes, is readable now, and still holds
        // exactly the bits the proof read. An 8-byte slot cannot straddle a page (checked in compile()).
        disjoint_stack(&mut c, *address, 8);
        c.u32c(*address >> 12).mem(op::I32_LOAD8_U, layout.rtab).u32c(1).op(op::I32_AND).op(op::I32_EQZ); reject(&mut c);
        c.u32c(*address).mem(op::I64_LOAD, layout.guest_base).i64c(*bits as i64).op(op::I64_NE); reject(&mut c);
    }
    if let (Some(&min), Some(&max)) = (proof.fields.first(), proof.fields.last()) {
        // The receiver is an untrusted guest word: prove every receiver+field span in bounds and non-wrapping, then
        // keep the whole accessed object interval apart from everything the replacement omits or reads elsewhere
        // (native transient frame writes, MethodFrame, argv; and, if anything is stored, static slots and masks).
        let written: Vec<u32> = lowering(&proof.shape).stores.iter().map(|&(f, _)| f).collect();
        let end = max + 8;
        c.lget(ARGV).mem(op::I32_LOAD, layout.guest_base).lset(RECEIVER);
        c.lget(RECEIVER).u32c(abi.guest_size - end).op(op::I32_GT_U); reject(&mut c);
        c.lget(RECEIVER).u32c(min).op(op::I32_ADD).lset(OBJECT_LOW);
        c.lget(RECEIVER).u32c(end).op(op::I32_ADD).lset(OBJECT_END);
        // frame [LOW, SP+16)
        c.lget(OBJECT_LOW).lget(SP).u32c(16).op(op::I32_ADD).op(op::I32_LT_U).lget(LOW).lget(OBJECT_END).op(op::I32_LT_U).op(op::I32_AND); reject(&mut c);
        // core fields [core+32, core+52)
        c.lget(OBJECT_LOW).u32c(abi.core + 52).op(op::I32_LT_U).u32c(abi.core + 32).lget(OBJECT_END).op(op::I32_LT_U).op(op::I32_AND); reject(&mut c);
        // argv [ARGV, ARGV+argv_size)
        c.lget(OBJECT_LOW).lget(ARGV).u32c(argv_size).op(op::I32_ADD).op(op::I32_LT_U).lget(ARGV).lget(OBJECT_END).op(op::I32_LT_U).op(op::I32_AND); reject(&mut c);
        if !written.is_empty() {
            for (address, size) in proof.masks.iter().map(|(a, _)| (*a, 16u32)).chain(proof.statics.iter().map(|(a, _)| (*a, 8u32))) {
                c.lget(OBJECT_LOW).u32c(address + size).op(op::I32_LT_U).u32c(address).lget(OBJECT_END).op(op::I32_LT_U).op(op::I32_AND); reject(&mut c);
            }
        }
        for &field in &proof.fields {
            c.lget(RECEIVER).u32c(field).op(op::I32_ADD).lset(ADDRESS);
            permission(&mut c, layout, ADDRESS, 8, written.contains(&field));
        }
    }
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base).lset(RET);
    emit_fp(&mut c, layout, proof, &lowering(&proof.shape));
    c.lget(0).lget(SP).u32c(4).op(op::I32_ADD).mem(op::I32_STORE, layout.cpu_r + ESP as u32 * 4);
    c.lget(0).lget(RET).mem(op::I32_STORE, layout.cpu_eip);
    c.lget(BUDGET).u32c(proof.instructions).op(op::I32_SUB).lset(BUDGET);
    c.lget(0).lget(BUDGET).mem(op::I32_STORE, layout.cpu_budget);
    let completed = core::mem::offset_of!(Cpu, jit_lir_completed) as u32;
    c.lget(0).lget(0).mem(op::I64_LOAD, completed).i64c(1).op(op::I64_ADD).mem(op::I64_STORE, completed);
    if layout.cache != 0 {
        // Every observable effect, including the completed-call counter, is committed before control can
        // leave this module. Same direct-mapped lookup and tail call as a native `ret` exit region.
        c.lget(BUDGET).i32c(0).op(op::I32_GT_S).if_(0x40);
        c.lget(RET).lget(RET).u32c(12).op(op::I32_SHR_U).op(op::I32_XOR)
            .u32c(CACHE_ENTRIES - 1).op(op::I32_AND).u32c(4).op(op::I32_SHL).lset(PAGE);
        c.lget(PAGE).mem(op::I32_LOAD, layout.cache).lget(RET).op(op::I32_EQ)
            .lget(PAGE).mem(op::I32_LOAD, layout.cache + 4).ltee(READ_PERMISSION).i32c(0).op(op::I32_NE).op(op::I32_AND).if_(0x40);
        c.lget(0).lget(PAGE).mem(op::I32_LOAD, layout.cache + 8)
            .lget(READ_PERMISSION).i32c(1).op(op::I32_SUB).return_call_indirect(signature, 0);
        c.end().end();
    }
    c.u32c(S_NEXT);
    let function = module.add_func(signature, N_I32, N_VALUES, 0, c.b);
    module.export_func("r0", function); module.name_func(function, "captured_number_leaf");
    Ok(Output { wasm: module.finish(), region: RegionOut { heads: vec![(abi.entry, 0)], n_ins: proof.instructions,
        chains: vec![(abi.entry, proof.instructions)], ..RegionOut::default() } })
}

/// The SSA text may carry receiver-field stores as `st <field offset> <value name>` lines (in native order). They
/// are not Trace instructions: strip them and resolve each name to its SSA value id (values are numbered in the
/// order their defining lines appear).
fn split_stores(source: &str) -> Result<(String, Vec<(u32, usize)>), String> {
    let mut text = Vec::new();
    let mut names = std::collections::HashMap::new();
    let mut stores = Vec::new();
    let mut defined = 0usize;
    for line in source.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["st", field, name] => {
                let field: u32 = field.parse().map_err(|_| "invalid Number store field")?;
                let id = *names.get(*name).ok_or("Number store of an undefined value")?;
                stores.push((field, id));
            }
            [name, "=", ..] => { names.insert(name.to_string(), defined); defined += 1; text.push(line); }
            _ => text.push(line),
        }
    }
    Ok((text.join("\n"), stores))
}

pub fn install(mem: &mut crate::mem::Memory, jit: &mut crate::jit::JitState,
    entry: u32, core: u32, native: &[u8], source: &str, offsets: &[u32], argc: u32) -> Result<usize, String> {
    if source.len() > 8192 { return Err("captured Number source too large".into()); }
    let (text, stores) = split_stores(source)?;
    let trace = Trace::parse(&text).map_err(|e| e.to_string())?;
    if trace.values.len() != text.lines().filter(|l| l.contains(" = ")).count() {
        return Err("Number SSA value numbering differs from its text".into());
    }
    let proof = verify_native_with_stores(native, entry, core, &trace, offsets, &stores,
        |a| mem.r128(a).map_err(|_| "Number native SIMD mask unreadable".into()))?;
    let layout = jit.lir_layout(mem);
    let output = compile(&layout, &Abi { entry, core, guest_size: u32::try_from(mem.size()).map_err(|_| "guest too large")?, argc }, &proof)
        .map_err(|e| e.to_string())?;
    let count = jit.install_precompiled(mem, &layout, &output.wasm,
        &[(output.region, vec![(entry, native[..proof.len].to_vec())])])?;
    jit.mark_lir_entry(entry); Ok(count)
}

#[cfg(test)]
#[path = "number_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "number_expr_data.rs"]
mod expr_data;
#[cfg(test)]
#[path = "number_expr_tests.rs"]
mod expr_tests;
