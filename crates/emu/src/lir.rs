//! Portable normalized NanoJIT LIR subset -> standalone WebAssembly.
//!
//! The normalized backend is separate from the exact-build raw capture frontend;
//! `pure` verifies native effects/result equivalence before real Flash installation.
//! Names/semantics follow adobe/avmplus NanoJIT LIRopcode.tbl (858d034a).
//! Only straight-line, side-effect-free i32/f64 SSA and explicit guards are accepted.
//! No loads/stores, calls, labels, loops, saved registers, or snapshot reconstruction.
//!
//! Text format (one instruction per line, `#` comments):
//! ```text
//! params i32 f64
//! a = parami 0
//! b = argd 1
//! c = d2i b
//! ok = eqi a c
//! xf ok 7
//! reti c
//! ```
//! `params` is required, even with no arguments. `parami N` denotes a normalized
//! argument index, NOT a NanoJIT register/stack parameter kind. `argd N` is an
//! explicitly synthetic normalized double argument operation (there is no paramd).
//! `immi` takes signed decimal i32 or hexadecimal u32 bits; `immd` takes a Rust
//! f64 literal or `bits:0123456789abcdef` for exact IEEE bits. SSA identifiers use
//! ASCII letters/digits/underscore, starting with a letter/underscore.
//!
//! Export `trace` has declared scalar arguments and MULTIVALUE result `(i32,i64)`:
//! status 0 = normal return, payload = sign-extended i32 or IEEE f64 bits according
//! to `reti`/`retd`; positive status = taken guard ID, payload 0. `xt c ID` exits
//! when c != 0, `xf` when c == 0, `x ID` unconditionally. IDs are 1..=i32::MAX.
//! Guards expose no live-value snapshot; the host must retain inputs and restart
//! its interpreter at a host-owned safe point. No native guard pointer is used.
//! Modules have no imports, memory, or effects, making that restart safe.
//!
//! Integer arithmetic wraps and shift counts use low five bits. d2i models IA32
//! SSE cvttsd2si: truncate toward zero, return i32::MIN for NaN/infinity/overflow,
//! never trap. Floating arithmetic follows wasm IEEE semantics, not mutable x87/
//! MXCSR modes; arithmetic NaN payloads are unspecified (constants preserve bits).
//! `compile_region` supplies a transactional general scalar-leaf ABI bridge;
//! `fast_region` is the trusted-layout integer adapter used by the verified Flash
//! frontend. The worker automatically hooks only the explicitly supported build.

use crate::wasm_emit::{op, Code, ModuleBuilder, I32, I64};
use std::collections::HashMap;
use std::fmt;

mod region;
pub use region::{Abi, AccessCheck, Argument, CodeBinding, RegionContract, ScalarRegion, WordSource};

pub mod flash_leaf;
pub mod fast_region;
pub mod memory_region;
pub mod pure;
pub mod counted;
pub mod number;

const F64: u8 = 0x7c;
const MAX_VALUES: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    I32,
    F64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub message: String,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for Error {}
fn error(line: usize, message: impl Into<String>) -> Error {
    Error {
        line,
        message: message.into(),
    }
}

/// A validated typed SSA graph. Fields are private so invalid graphs cannot be
/// constructed through the public API; `parse` is the normalized capture boundary.
#[derive(Clone, Debug)]
pub struct Trace {
    params: Vec<Type>,
    values: Vec<Value>,
    instructions: Vec<Instruction>,
    result: Type,
}
#[derive(Clone, Debug)]
struct Value {
    ty: Type,
    expr: Expr,
}
#[derive(Clone, Debug)]
enum Expr {
    Int(i32),
    Double(u64),
    Param(usize),
    Numeric {
        opcode: u8,
        args: Vec<usize>,
    },
    D2i(usize),
    Negi(usize),
    Noti(usize),
    Select {
        condition: usize,
        yes: usize,
        no: usize,
    },
}
#[derive(Clone, Debug)]
enum Instruction {
    Value(usize),
    Guard {
        condition: Option<usize>,
        on_true: bool,
        id: i32,
    },
    Return(usize),
}

impl Trace {
    /// Integer-only SSA emitter for integrated leaf adapters: real i32 locals, no
    /// i64 bit boxing per arithmetic operation. Validated guard-free traces only.
    fn emit_integer_function(&self, module: &mut ModuleBuilder) -> Result<u32, Error> {
        self.emit_integer_function_outputs(module, &[])
    }

    fn emit_integer_function_outputs(&self, module: &mut ModuleBuilder, outputs: &[usize]) -> Result<u32, Error> {
        if outputs.len() > 8 || outputs.iter().any(|&id| id >= self.values.len()) { return Err(error(0, "invalid integer effect outputs")); }
        if self.result != Type::I32 || self.params.iter().any(|t| *t != Type::I32) || self.values.iter().any(|v| v.ty != Type::I32)
            || self.instructions.iter().any(|i| matches!(i, Instruction::Guard { .. })) { return Err(error(0, "integer emitter requires pure i32 SSA")); }
        let base = self.params.len() as u32;
        let mut code = Code::new();
        for instruction in &self.instructions {
            match instruction {
                Instruction::Value(id) => {
                    match &self.values[*id].expr {
                        Expr::Int(n) => { code.i32c(*n); }
                        Expr::Param(n) => { code.lget(*n as u32); }
                        Expr::Numeric { opcode, args } => { for &a in args { code.lget(base + a as u32); } code.op(*opcode); }
                        Expr::Negi(a) => { code.i32c(0).lget(base + *a as u32).op(op::I32_SUB); }
                        Expr::Noti(a) => { code.lget(base + *a as u32).i32c(-1).op(op::I32_XOR); }
                        Expr::Select { condition, yes, no } => { code.lget(base + *yes as u32).lget(base + *no as u32).lget(base + *condition as u32).select(); }
                        _ => return Err(error(0, "unsupported integer-only expression")),
                    }
                    code.lset(base + *id as u32);
                }
                Instruction::Return(id) => {
                    code.lget(base + *id as u32);
                    for &output in outputs { code.lget(base + output as u32); }
                    code.op(op::RETURN);
                }
                _ => return Err(error(0, "unexpected integer guard")),
            }
        }
        code.unreachable();
        let signature = module.ty(&vec![I32; self.params.len()], &vec![I32; 1 + outputs.len()]);
        let function = module.add_func(signature, self.values.len() as u32, 0, 0, code.b);
        module.name_func(function, "captured_integer_lir");
        Ok(function)
    }

    pub fn params(&self) -> &[Type] {
        &self.params
    }
    pub fn result(&self) -> Type {
        self.result
    }

    /// Validate all instructions, including instructions following conditional
    /// guards. Definitions must precede uses; typed returns must end the trace.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut trace = Self {
            params: Vec::new(),
            values: Vec::new(),
            instructions: Vec::new(),
            result: Type::I32,
        };
        let mut names = HashMap::<String, usize>::new();
        let mut header = false;
        let mut terminated = false;
        let mut returned = false;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let words: Vec<_> = raw.split('#').next().unwrap().split_whitespace().collect();
            if words.is_empty() {
                continue;
            }
            if terminated {
                return Err(error(line, "instruction after terminator"));
            }
            if !header {
                if words[0] != "params" {
                    return Err(error(line, "expected params header"));
                }
                for ty in &words[1..] {
                    trace.params.push(match *ty {
                        "i32" => Type::I32,
                        "f64" => Type::F64,
                        _ => return Err(error(line, "unsupported parameter type")),
                    });
                }
                if trace.params.len() > MAX_VALUES {
                    return Err(error(line, "too many parameters"));
                }
                header = true;
                continue;
            }
            let operand = |name: &str, ty: Type| -> Result<usize, Error> {
                let id = *names
                    .get(name)
                    .ok_or_else(|| error(line, format!("undefined operand {name}")))?;
                if trace.values[id].ty != ty {
                    return Err(error(
                        line,
                        format!("type mismatch for {name}: expected {ty:?}"),
                    ));
                }
                Ok(id)
            };
            match words[0] {
                "reti" | "retd" => {
                    if words.len() != 2 {
                        return Err(error(line, "return expects one operand"));
                    }
                    trace.result = if words[0] == "reti" {
                        Type::I32
                    } else {
                        Type::F64
                    };
                    let id = operand(words[1], trace.result)?;
                    trace.instructions.push(Instruction::Return(id));
                    terminated = true;
                    returned = true;
                    continue;
                }
                "x" | "xt" | "xf" => {
                    let unconditional = words[0] == "x";
                    if words.len() != if unconditional { 2 } else { 3 } {
                        return Err(error(line, "invalid guard arity"));
                    }
                    let id: i32 = words[words.len() - 1]
                        .parse()
                        .map_err(|_| error(line, "invalid guard ID"))?;
                    if id <= 0 {
                        return Err(error(line, "guard ID must be positive"));
                    }
                    let condition = if unconditional {
                        None
                    } else {
                        Some(operand(words[1], Type::I32)?)
                    };
                    trace.instructions.push(Instruction::Guard {
                        condition,
                        on_true: words[0] != "xf",
                        id,
                    });
                    // Require an eventual typed return even after x, to declare
                    // the normal-result ABI. x itself always returns at runtime.
                    continue;
                }
                _ => {}
            }
            if words.len() < 4 || words[1] != "=" {
                return Err(error(line, "expected name = opcode operands"));
            }
            let name = words[0];
            if !identifier(name) || names.contains_key(name) {
                return Err(error(line, "invalid or duplicate SSA identifier"));
            }
            if trace.values.len() >= MAX_VALUES {
                return Err(error(line, "too many SSA values"));
            }
            let opcode = words[2];
            let args = &words[3..];
            let arity = |n| {
                if args.len() == n {
                    Ok(())
                } else {
                    Err(error(line, format!("{opcode} expects {n} operands")))
                }
            };
            let (ty, expr) = match opcode {
                "immi" => {
                    arity(1)?;
                    let value = if let Some(hex) = args[0].strip_prefix("0x") {
                        u32::from_str_radix(hex, 16).map(|x| x as i32)
                    } else {
                        args[0].parse::<i32>()
                    }
                    .map_err(|_| error(line, "invalid i32 immediate"))?;
                    (Type::I32, Expr::Int(value))
                }
                "immd" => {
                    arity(1)?;
                    let bits = if let Some(hex) = args[0].strip_prefix("bits:") {
                        if hex.len() != 16 {
                            return Err(error(line, "immd bits requires 16 hex digits"));
                        }
                        u64::from_str_radix(hex, 16).map_err(|_| error(line, "invalid f64 bits"))?
                    } else {
                        args[0]
                            .parse::<f64>()
                            .map_err(|_| error(line, "invalid f64 immediate"))?
                            .to_bits()
                    };
                    (Type::F64, Expr::Double(bits))
                }
                "parami" | "argd" => {
                    arity(1)?;
                    let index: usize = args[0]
                        .parse()
                        .map_err(|_| error(line, "invalid parameter index"))?;
                    let ty = if opcode == "parami" {
                        Type::I32
                    } else {
                        Type::F64
                    };
                    if trace.params.get(index) != Some(&ty) {
                        return Err(error(line, "parameter index/type mismatch"));
                    }
                    (ty, Expr::Param(index))
                }
                "d2i" | "negi" | "noti" => {
                    arity(1)?;
                    let id = operand(
                        args[0],
                        if opcode == "d2i" {
                            Type::F64
                        } else {
                            Type::I32
                        },
                    )?;
                    (
                        Type::I32,
                        match opcode {
                            "d2i" => Expr::D2i(id),
                            "negi" => Expr::Negi(id),
                            _ => Expr::Noti(id),
                        },
                    )
                }
                "cmovi" | "cmovd" => {
                    arity(3)?;
                    let ty = if opcode == "cmovi" {
                        Type::I32
                    } else {
                        Type::F64
                    };
                    (
                        ty,
                        Expr::Select {
                            condition: operand(args[0], Type::I32)?,
                            yes: operand(args[1], ty)?,
                            no: operand(args[2], ty)?,
                        },
                    )
                }
                _ => {
                    let (input, output, wasm, count) = numeric(opcode)
                        .ok_or_else(|| error(line, format!("unsupported opcode {opcode}")))?;
                    arity(count)?;
                    let ids = args
                        .iter()
                        .map(|arg| operand(arg, input))
                        .collect::<Result<Vec<_>, _>>()?;
                    (
                        output,
                        Expr::Numeric {
                            opcode: wasm,
                            args: ids,
                        },
                    )
                }
            };
            let id = trace.values.len();
            trace.values.push(Value { ty, expr });
            trace.instructions.push(Instruction::Value(id));
            names.insert(name.to_owned(), id);
        }
        if !header || !returned {
            return Err(error(
                text.lines().count().max(1),
                "missing params header or typed return",
            ));
        }
        Ok(trace)
    }

    /// Generate an import-free module with one `trace` export. All SSA locals
    /// contain i64 bits; scalar reinterpretation avoids modifying shared emitters.
    pub fn compile(&self) -> Vec<u8> {
        let mut module = ModuleBuilder::new();
        let function = self.emit_function(&mut module);
        module.export_func("trace", function);
        module.finish()
    }

    fn emit_function(&self, module: &mut ModuleBuilder) -> u32 {
        let base = self.params.len() as u32;
        let mut code = Code::new();
        for instruction in &self.instructions {
            match instruction {
                Instruction::Value(id) => {
                    let value = &self.values[*id];
                    match &value.expr {
                        Expr::Int(n) => {
                            code.i32c(*n);
                        }
                        Expr::Double(bits) => {
                            code.i64c(*bits as i64).op(op::F64_REINTERPRET_I64);
                        }
                        Expr::Param(n) => {
                            code.lget(*n as u32);
                        }
                        Expr::Numeric { opcode, args } => {
                            for arg in args {
                                self.get(&mut code, base, *arg);
                            }
                            code.op(*opcode);
                        }
                        Expr::Negi(arg) => {
                            code.i32c(0);
                            self.get(&mut code, base, *arg);
                            code.op(op::I32_SUB);
                        }
                        Expr::Noti(arg) => {
                            self.get(&mut code, base, *arg);
                            code.i32c(-1).op(op::I32_XOR);
                        }
                        Expr::Select { condition, yes, no } => {
                            self.get(&mut code, base, *yes);
                            self.get(&mut code, base, *no);
                            self.get(&mut code, base, *condition);
                            code.select();
                        }
                        Expr::D2i(arg) => {
                            // Test the TRUNCATED value: -2147483648.9 is valid
                            // and becomes MIN, not an overflow. Ordered tests
                            // reject NaN before the otherwise trapping conversion.
                            self.get(&mut code, base, *arg);
                            code.op(0x9d).f64c(-2147483648.0).op(op::F64_GE);
                            self.get(&mut code, base, *arg);
                            code.op(0x9d)
                                .f64c(2147483648.0)
                                .op(op::F64_LT)
                                .op(op::I32_AND)
                                .if_(I32);
                            self.get(&mut code, base, *arg);
                            code.op(op::I32_TRUNC_F64_S).else_().i32c(i32::MIN).end();
                        }
                    }
                    code.op(match value.ty {
                        Type::I32 => op::I64_EXTEND_I32_S,
                        Type::F64 => op::I64_REINTERPRET_F64,
                    });
                    code.lset(base + *id as u32);
                }
                Instruction::Guard {
                    condition,
                    on_true,
                    id,
                } => {
                    if let Some(arg) = condition {
                        self.get(&mut code, base, *arg);
                        if !on_true {
                            code.op(op::I32_EQZ);
                        }
                        code.if_(0x40);
                    }
                    code.i32c(*id).i64c(0).op(op::RETURN);
                    if condition.is_some() {
                        code.end();
                    }
                }
                Instruction::Return(id) => {
                    code.i32c(0).lget(base + *id as u32).op(op::RETURN);
                }
            }
        }
        // All paths return; mark the syntactic end unreachable for wasm typing.
        code.unreachable();
        let params: Vec<_> = self
            .params
            .iter()
            .map(|ty| match ty {
                Type::I32 => I32,
                Type::F64 => F64,
            })
            .collect();
        let signature = module.ty(&params, &[I32, I64]);
        let function = module.add_func(signature, 0, self.values.len() as u32, 0, code.b);
        module.name_func(function, "normalized_lir_trace");
        function
    }

    fn get(&self, code: &mut Code, base: u32, id: usize) {
        code.lget(base + id as u32).op(match self.values[id].ty {
            Type::I32 => op::I32_WRAP_I64,
            Type::F64 => op::F64_REINTERPRET_I64,
        });
    }
}

fn identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn numeric(name: &str) -> Option<(Type, Type, u8, usize)> {
    use Type::{F64 as D, I32 as I};
    let integer = match name {
        "addi" => op::I32_ADD,
        "subi" => op::I32_SUB,
        "muli" => op::I32_MUL,
        "andi" => op::I32_AND,
        "ori" => op::I32_OR,
        "xori" => op::I32_XOR,
        "lshi" => op::I32_SHL,
        "rshi" => op::I32_SHR_S,
        "rshui" => op::I32_SHR_U,
        "eqi" => op::I32_EQ,
        "lti" => op::I32_LT_S,
        "gti" => op::I32_GT_S,
        "lei" => op::I32_LE_S,
        "gei" => op::I32_GE_S,
        "ltui" => op::I32_LT_U,
        "gtui" => op::I32_GT_U,
        "leui" => op::I32_LE_U,
        "geui" => op::I32_GE_U,
        _ => 0,
    };
    if integer != 0 {
        return Some((I, I, integer, 2));
    }
    Some(match name {
        "addd" => (D, D, op::F64_ADD, 2),
        "subd" => (D, D, op::F64_SUB, 2),
        "muld" => (D, D, op::F64_MUL, 2),
        "divd" => (D, D, op::F64_DIV, 2),
        "negd" => (D, D, 0x9a, 1),
        "absd" => (D, D, 0x99, 1),
        "sqrtd" => (D, D, op::F64_SQRT, 1),
        "eqd" => (D, I, op::F64_EQ, 2),
        "ltd" => (D, I, op::F64_LT, 2),
        "gtd" => (D, I, op::F64_GT, 2),
        "led" => (D, I, op::F64_LE, 2),
        "ged" => (D, I, op::F64_GE, 2),
        "i2d" => (I, D, op::F64_CONVERT_I32_S, 1),
        "ui2d" => (I, D, 0xb8, 1),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmi::{Engine, Linker, Module, Store, TypedFunc, WasmParams};

    fn executable<P: WasmParams>(text: &str) -> (Store<()>, TypedFunc<P, (i32, i64)>) {
        let trace = Trace::parse(text).unwrap();
        let bytes = trace.compile();
        let engine = Engine::default();
        let module = Module::new(&engine, &bytes[..]).expect("generated wasm validates");
        let mut store = Store::new(&engine, ());
        let instance = Linker::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .unwrap();
        let function = instance
            .get_typed_func::<P, (i32, i64)>(&store, "trace")
            .unwrap();
        (store, function)
    }
    fn random(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn integer_ops_differential_randomized() {
        let operations = [
            "addi", "subi", "muli", "andi", "ori", "xori", "lshi", "rshi", "rshui", "eqi", "lti",
            "gti", "lei", "gei", "ltui", "gtui", "leui", "geui",
        ];
        let edges = [i32::MIN, i32::MAX, -1, 0, 1, 31, 32, 33, -32, -33];
        let mut seed = 0x9182a7b3c4d5;
        for operation in operations {
            let source =
                format!("params i32 i32\na = parami 0\nb = parami 1\nc = {operation} a b\nreti c");
            let (mut store, function) = executable::<(i32, i32)>(&source);
            let oracle = |a: i32, b: i32| -> i32 {
                match operation {
                    "addi" => a.wrapping_add(b),
                    "subi" => a.wrapping_sub(b),
                    "muli" => a.wrapping_mul(b),
                    "andi" => a & b,
                    "ori" => a | b,
                    "xori" => a ^ b,
                    "lshi" => a.wrapping_shl(b as u32),
                    "rshi" => a.wrapping_shr(b as u32),
                    "rshui" => (a as u32).wrapping_shr(b as u32) as i32,
                    "eqi" => (a == b) as i32,
                    "lti" => (a < b) as i32,
                    "gti" => (a > b) as i32,
                    "lei" => (a <= b) as i32,
                    "gei" => (a >= b) as i32,
                    "ltui" => ((a as u32) < b as u32) as i32,
                    "gtui" => ((a as u32) > b as u32) as i32,
                    "leui" => ((a as u32) <= b as u32) as i32,
                    "geui" => ((a as u32) >= b as u32) as i32,
                    _ => unreachable!(),
                }
            };
            for (a, b) in edges
                .iter()
                .flat_map(|a| edges.iter().map(move |b| (*a, *b)))
                .chain((0..512).map(|_| (random(&mut seed) as i32, random(&mut seed) as i32)))
            {
                assert_eq!(
                    function.call(&mut store, (a, b)).unwrap(),
                    (0, oracle(a, b) as i64),
                    "{operation} {a} {b}"
                );
            }
        }
    }

    #[test]
    fn randomized_ssa_graph() {
        let mut seed = 0x7f9021042222;
        let mut source = String::from("params i32 i32\nv0 = parami 0\nv1 = parami 1\n");
        let mut nodes = Vec::new();
        for id in 2..130 {
            let left = random(&mut seed) as usize % id;
            let right = random(&mut seed) as usize % id;
            let operation = random(&mut seed) % 4;
            source.push_str(&format!(
                "v{id} = {} v{left} v{right}\n",
                ["addi", "muli", "xori", "rshui"][operation as usize]
            ));
            nodes.push((left, right, operation));
        }
        source.push_str("reti v129\n");
        let (mut store, function) = executable::<(i32, i32)>(&source);
        for _ in 0..1024 {
            let a = random(&mut seed) as i32;
            let b = random(&mut seed) as i32;
            let mut values = vec![a, b];
            for &(left, right, operation) in &nodes {
                let (x, y) = (values[left], values[right]);
                values.push(match operation {
                    0 => x.wrapping_add(y),
                    1 => x.wrapping_mul(y),
                    2 => x ^ y,
                    _ => (x as u32).wrapping_shr(y as u32) as i32,
                });
            }
            assert_eq!(
                function.call(&mut store, (a, b)).unwrap(),
                (0, values[129] as i64)
            );
        }
    }

    #[test]
    fn sse_d2i_nontrapping_boundaries_and_random_bits() {
        let (mut store, function) = executable::<f64>("params f64\na = argd 0\nb = d2i a\nreti b");
        let mut seed = 0x42fedcba9876;
        let boundaries = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -0.0,
            1.9,
            -1.9,
            2147483647.9,
            2147483648.0,
            -2147483648.9,
            -2147483649.0,
            f64::MAX,
            f64::MIN_POSITIVE,
        ];
        for value in boundaries
            .into_iter()
            .chain((0..4096).map(|_| f64::from_bits(random(&mut seed))))
        {
            let truncated = value.trunc();
            let expected = if truncated.is_finite()
                && truncated >= i32::MIN as f64
                && truncated < 2147483648.0
            {
                truncated as i32
            } else {
                i32::MIN
            };
            assert_eq!(
                function.call(&mut store, value).unwrap(),
                (0, expected as i64),
                "d2i {value:?}"
            );
        }
    }

    fn assert_float(actual: i64, expected: f64) {
        let got = f64::from_bits(actual as u64);
        if expected.is_nan() {
            assert!(got.is_nan());
        } else {
            assert_eq!(
                got.to_bits(),
                expected.to_bits(),
                "got {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn floating_arithmetic_comparisons_and_signed_zero() {
        let edges = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
        ];
        let mut seed = 0x991287fa;
        for operation in [
            "addd", "subd", "muld", "divd", "eqd", "ltd", "gtd", "led", "ged",
        ] {
            let comparison = matches!(operation, "eqd" | "ltd" | "gtd" | "led" | "ged");
            let source = format!(
                "params f64 f64\na = argd 0\nb = argd 1\nc = {operation} a b\n{} c",
                if comparison { "reti" } else { "retd" }
            );
            let (mut store, function) = executable::<(f64, f64)>(&source);
            for (a, b) in edges
                .iter()
                .flat_map(|a| edges.iter().map(move |b| (*a, *b)))
                .chain((0..512).map(|_| {
                    (
                        f64::from_bits(random(&mut seed)),
                        f64::from_bits(random(&mut seed)),
                    )
                }))
            {
                let (status, payload) = function.call(&mut store, (a, b)).unwrap();
                assert_eq!(status, 0);
                if comparison {
                    let expected = match operation {
                        "eqd" => a == b,
                        "ltd" => a < b,
                        "gtd" => a > b,
                        "led" => a <= b,
                        _ => a >= b,
                    };
                    assert_eq!(payload, expected as i64);
                } else {
                    assert_float(
                        payload,
                        match operation {
                            "addd" => a + b,
                            "subd" => a - b,
                            "muld" => a * b,
                            _ => a / b,
                        },
                    );
                }
            }
        }
        for operation in ["negd", "absd", "sqrtd"] {
            let (mut store, function) = executable::<f64>(&format!(
                "params f64\na = argd 0\nb = {operation} a\nretd b"
            ));
            for a in edges {
                let (_, payload) = function.call(&mut store, a).unwrap();
                assert_float(
                    payload,
                    match operation {
                        "negd" => -a,
                        "absd" => a.abs(),
                        _ => a.sqrt(),
                    },
                );
            }
        }
    }

    #[test]
    fn guards_select_and_conversions() {
        let (mut store, function) = executable::<i32>(
            "params i32\na = parami 0\nb = immi 5\nc = cmovi a a b\nxt a 19\nreti c",
        );
        assert_eq!(function.call(&mut store, 0).unwrap(), (0, 5));
        for a in [-1, 1, i32::MIN] {
            assert_eq!(function.call(&mut store, a).unwrap(), (19, 0));
        }
        let (mut store, function) =
            executable::<i32>("params i32\na = parami 0\nxf a 2147483647\nreti a");
        assert_eq!(function.call(&mut store, 0).unwrap(), (i32::MAX, 0));
        assert_eq!(function.call(&mut store, -8).unwrap(), (0, -8));
        let (mut store, function) = executable::<()>("params\nx 7\na = immi 1\nreti a");
        assert_eq!(function.call(&mut store, ()).unwrap(), (7, 0));
        for operation in ["i2d", "ui2d", "negi", "noti"] {
            let double = operation.ends_with('d');
            let (mut store, function) = executable::<i32>(&format!(
                "params i32\na = parami 0\nb = {operation} a\n{} b",
                if double { "retd" } else { "reti" }
            ));
            for a in [i32::MIN, i32::MAX, -1, 0, 1] {
                let (_, payload) = function.call(&mut store, a).unwrap();
                match operation {
                    "i2d" => assert_float(payload, a as f64),
                    "ui2d" => assert_float(payload, a as u32 as f64),
                    "negi" => assert_eq!(payload, a.wrapping_neg() as i64),
                    _ => assert_eq!(payload, (!a) as i64),
                }
            }
        }
        let (mut store,function) = executable::<i32>("params i32\na = parami 0\nb = immd bits:8000000000000000\nc = immd bits:7ff8000000000042\nd = cmovd a b c\nretd d");
        assert_eq!(function.call(&mut store, 1).unwrap(), (0, i64::MIN));
        assert_eq!(
            function.call(&mut store, 0).unwrap(),
            (0, 0x7ff8000000000042)
        );
        let (mut store, function) =
            executable::<()>("params\na = immi 0xffffffff\nb = immd -0.0\nreti a");
        assert_eq!(function.call(&mut store, ()).unwrap(), (0, -1));
    }

    #[test]
    fn malformed_or_unsupported_traces_rejected() {
        for source in [
            "",
            "a = immi 0",
            "params",
            "params i64\na = immi 0\nreti a",
            "params\na = addi b b\nreti a",
            "params\na = immi 0\na = immi 1\nreti a",
            "params\na = immd 0\nreti a",
            "params\na = immi 0\nretd a",
            "params\na = immi 2147483648\nreti a",
            "params\na = immi 0x100000000\nreti a",
            "params\na = immd bits:ff\nretd a",
            "params\na = parami 0\nreti a",
            "params i32\na = argd 0\nretd a",
            "params\na = immi 0 1\nreti a",
            "params\n1a = immi 0\nreti 1a",
            "params\na = immi 1\nxt a 0\nreti a",
            "params\na = immi 1\nxf a -1\nreti a",
            "params\na = immd 1\nxt a 1\nretd a",
            "params\na = immi 0\nreti a\na = immi 1",
            "params\nlabel foo",
            "params\na = ldi foo\nreti a",
            "params\na = divi b c\nreti a",
            "params\na = paramd 0\nretd a",
            "params\na = immi 0\nreti a extra",
        ] {
            assert!(
                Trace::parse(source).is_err(),
                "accepted malformed trace: {source}"
            );
        }
        let err = Trace::parse("params\na = addi missing missing\nreti a").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.message.contains("undefined"));
    }
}
