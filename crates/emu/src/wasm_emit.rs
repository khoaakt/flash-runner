//! Minimal WebAssembly binary assembler used by the JIT. Target independent (pure byte building) so the
//! translator can be unit-tested natively. Only what the translator needs: function types, imports
//! (functions, one memory, one table), defined functions with i32/i64 locals, and exports.

pub const I32: u8 = 0x7f;
pub const I64: u8 = 0x7e;
pub const V128: u8 = 0x7b;
/// Empty block type.
pub const BT_EMPTY: u8 = 0x40;

pub fn leb_u32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

pub fn leb_i32(out: &mut Vec<u8>, v: i32) {
    leb_i64(out, v as i64)
}

pub fn leb_i64(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
        if done {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Numeric / control opcodes (single byte) the translator uses.
pub mod op {
    pub const UNREACHABLE: u8 = 0x00;
    pub const NOP: u8 = 0x01;
    pub const BLOCK: u8 = 0x02;
    pub const LOOP: u8 = 0x03;
    pub const IF: u8 = 0x04;
    pub const ELSE: u8 = 0x05;
    pub const END: u8 = 0x0b;
    pub const BR: u8 = 0x0c;
    pub const BR_IF: u8 = 0x0d;
    pub const BR_TABLE: u8 = 0x0e;
    pub const RETURN: u8 = 0x0f;
    pub const CALL: u8 = 0x10;
    pub const CALL_INDIRECT: u8 = 0x11;
    pub const RETURN_CALL: u8 = 0x12;
    pub const RETURN_CALL_INDIRECT: u8 = 0x13;
    pub const DROP: u8 = 0x1a;
    pub const SELECT: u8 = 0x1b;
    pub const LOCAL_GET: u8 = 0x20;
    pub const LOCAL_SET: u8 = 0x21;
    pub const LOCAL_TEE: u8 = 0x22;

    pub const I32_LOAD: u8 = 0x28;
    pub const I64_LOAD: u8 = 0x29;
    pub const I32_LOAD8_S: u8 = 0x2c;
    pub const I32_LOAD8_U: u8 = 0x2d;
    pub const I32_LOAD16_S: u8 = 0x2e;
    pub const I32_LOAD16_U: u8 = 0x2f;
    pub const I32_STORE: u8 = 0x36;
    pub const I64_STORE: u8 = 0x37;
    pub const I32_STORE8: u8 = 0x3a;
    pub const I32_STORE16: u8 = 0x3b;
    pub const I32_CONST: u8 = 0x41;
    pub const I64_CONST: u8 = 0x42;

    pub const I32_EQZ: u8 = 0x45;
    pub const I32_EQ: u8 = 0x46;
    pub const I32_NE: u8 = 0x47;
    pub const I32_LT_S: u8 = 0x48;
    pub const I32_LT_U: u8 = 0x49;
    pub const I32_GT_S: u8 = 0x4a;
    pub const I32_GT_U: u8 = 0x4b;
    pub const I32_LE_S: u8 = 0x4c;
    pub const I32_LE_U: u8 = 0x4d;
    pub const I32_GE_S: u8 = 0x4e;
    pub const I32_GE_U: u8 = 0x4f;
    pub const I64_EQZ: u8 = 0x50;
    pub const I64_EQ: u8 = 0x51;
    pub const I64_NE: u8 = 0x52;
    pub const I64_LT_S: u8 = 0x53;
    pub const I64_LT_U: u8 = 0x54;
    pub const I64_GT_S: u8 = 0x55;
    pub const I64_GT_U: u8 = 0x56;
    pub const I64_LE_S: u8 = 0x57;
    pub const I64_LE_U: u8 = 0x58;
    pub const I64_GE_S: u8 = 0x59;
    pub const I64_GE_U: u8 = 0x5a;

    pub const I32_CLZ: u8 = 0x67;
    pub const I32_CTZ: u8 = 0x68;
    pub const I32_POPCNT: u8 = 0x69;
    pub const I32_ADD: u8 = 0x6a;
    pub const I32_SUB: u8 = 0x6b;
    pub const I32_MUL: u8 = 0x6c;
    pub const I32_DIV_S: u8 = 0x6d;
    pub const I32_DIV_U: u8 = 0x6e;
    pub const I32_REM_S: u8 = 0x6f;
    pub const I32_REM_U: u8 = 0x70;
    pub const I32_AND: u8 = 0x71;
    pub const I32_OR: u8 = 0x72;
    pub const I32_XOR: u8 = 0x73;
    pub const I32_SHL: u8 = 0x74;
    pub const I32_SHR_S: u8 = 0x75;
    pub const I32_SHR_U: u8 = 0x76;
    pub const I32_ROTL: u8 = 0x77;
    pub const I32_ROTR: u8 = 0x78;

    pub const I64_CLZ: u8 = 0x79;
    pub const I64_ADD: u8 = 0x7c;
    pub const I64_SUB: u8 = 0x7d;
    pub const I64_MUL: u8 = 0x7e;
    pub const I64_DIV_S: u8 = 0x7f;
    pub const I64_DIV_U: u8 = 0x80;
    pub const I64_REM_S: u8 = 0x81;
    pub const I64_REM_U: u8 = 0x82;
    pub const I64_AND: u8 = 0x83;
    pub const I64_OR: u8 = 0x84;
    pub const I64_XOR: u8 = 0x85;
    pub const I64_SHL: u8 = 0x86;
    pub const I64_SHR_S: u8 = 0x87;
    pub const I64_SHR_U: u8 = 0x88;

    pub const F32_EQ: u8 = 0x5b;
    pub const F32_NE: u8 = 0x5c;
    pub const F32_LT: u8 = 0x5d;
    pub const F32_GT: u8 = 0x5e;
    pub const F32_LE: u8 = 0x5f;
    pub const F32_GE: u8 = 0x60;
    pub const F64_EQ: u8 = 0x61;
    pub const F64_NE: u8 = 0x62;
    pub const F64_LT: u8 = 0x63;
    pub const F64_GT: u8 = 0x64;
    pub const F64_LE: u8 = 0x65;
    pub const F64_GE: u8 = 0x66;
    pub const F32_SQRT: u8 = 0x91;
    pub const F32_ADD: u8 = 0x92;
    pub const F32_SUB: u8 = 0x93;
    pub const F32_MUL: u8 = 0x94;
    pub const F32_DIV: u8 = 0x95;
    pub const F64_SQRT: u8 = 0x9f;
    pub const F64_ADD: u8 = 0xa0;
    pub const F64_SUB: u8 = 0xa1;
    pub const F64_MUL: u8 = 0xa2;
    pub const F64_DIV: u8 = 0xa3;
    pub const I32_TRUNC_F32_S: u8 = 0xa8;
    pub const I32_TRUNC_F64_S: u8 = 0xaa;
    pub const F32_CONVERT_I32_S: u8 = 0xb2;
    pub const F32_DEMOTE_F64: u8 = 0xb6;
    pub const F64_CONVERT_I32_S: u8 = 0xb7;
    pub const F64_PROMOTE_F32: u8 = 0xbb;
    pub const I32_REINTERPRET_F32: u8 = 0xbc;
    pub const I64_REINTERPRET_F64: u8 = 0xbd;
    pub const F32_REINTERPRET_I32: u8 = 0xbe;
    pub const F64_REINTERPRET_I64: u8 = 0xbf;

    pub const I32_WRAP_I64: u8 = 0xa7;
    pub const I64_EXTEND_I32_S: u8 = 0xac;
    pub const I64_EXTEND_I32_U: u8 = 0xad;
    pub const I32_EXTEND8_S: u8 = 0xc0;
    pub const I32_EXTEND16_S: u8 = 0xc1;
}

/// Byte buffer with typed emit helpers. No validation is done here: the module is validated by the
/// engine (and by wasmi in the native tests).
#[derive(Default, Clone)]
pub struct Code {
    pub b: Vec<u8>,
    pub relocations: Vec<Relocation>,
    pub record_relocations: bool,
}

/// Fixed-width LEB operand relocation, explicitly emitted at semantic address sites.
/// kind: 0 = guest instruction address, 1 = guest RAM, 2 = read table, 3 = write table,
/// 4 = live region-cache base plus an explicit 0/4/8-byte field offset,
/// 5 = module-local fallback instruction index (exec_one's tag operand only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub offset: u32,
    pub kind: u32,
    pub value: u32,
}

pub fn fixed_leb(value: u32, signed: bool) -> [u8; 5] {
    let mut b = [0u8; 5];
    for (i, byte) in b.iter_mut().enumerate() {
        *byte = ((value >> (7 * i)) & 0x7f) as u8 | if i < 4 { 0x80 } else { 0 };
    }
    if signed && value & 0x8000_0000 != 0 { b[4] |= 0x70; }
    b
}

impl Code {
    pub fn new() -> Code {
        Code { b: Vec::with_capacity(1024), relocations: Vec::new(), record_relocations: false }
    }
    pub fn relocatable() -> Code { Code { record_relocations: true, ..Code::new() } }
    pub fn len(&self) -> usize {
        self.b.len()
    }
    pub fn append(&mut self, other: &Code) {
        let base = self.b.len() as u32;
        self.relocations.extend(other.relocations.iter().cloned().map(|mut r| { r.offset += base; r }));
        self.b.extend_from_slice(&other.b);
    }
    fn reloc_operand(&mut self, kind: u32, value: u32) -> &mut Self {
        self.relocations.push(Relocation { offset: self.b.len() as u32, kind, value });
        self.b.extend_from_slice(&fixed_leb(value, matches!(kind, 0 | 5)));
        self
    }
    pub fn ip(&mut self, value: u32) -> &mut Self {
        if !self.record_relocations { return self.u32c(value); }
        self.op(op::I32_CONST).reloc_operand(0, value)
    }
    pub fn fallback_tag(&mut self, value: u32) -> &mut Self {
        if !self.record_relocations { return self.u32c(value); }
        // i32.const takes a signed LEB, including tags with bit 31 set.
        self.op(op::I32_CONST).reloc_operand(5, value)
    }
    pub fn reloc_mem(&mut self, opc: u8, value: u32, kind: u32) -> &mut Self {
        if !self.record_relocations { return self.mem(opc, value); }
        self.op(opc);
        leb_u32(&mut self.b, 0);
        self.reloc_operand(kind, value)
    }
    #[inline]
    pub fn op(&mut self, o: u8) -> &mut Self {
        self.b.push(o);
        self
    }
    pub fn block(&mut self, bt: u8) -> &mut Self {
        self.op(op::BLOCK).op(bt)
    }
    pub fn loop_(&mut self, bt: u8) -> &mut Self {
        self.op(op::LOOP).op(bt)
    }
    pub fn if_(&mut self, bt: u8) -> &mut Self {
        self.op(op::IF).op(bt)
    }
    pub fn else_(&mut self) -> &mut Self {
        self.op(op::ELSE)
    }
    pub fn end(&mut self) -> &mut Self {
        self.op(op::END)
    }
    pub fn br(&mut self, depth: u32) -> &mut Self {
        self.op(op::BR);
        leb_u32(&mut self.b, depth);
        self
    }
    pub fn br_if(&mut self, depth: u32) -> &mut Self {
        self.op(op::BR_IF);
        leb_u32(&mut self.b, depth);
        self
    }
    pub fn br_table(&mut self, targets: &[u32], default: u32) -> &mut Self {
        self.op(op::BR_TABLE);
        leb_u32(&mut self.b, targets.len() as u32);
        for &t in targets {
            leb_u32(&mut self.b, t);
        }
        leb_u32(&mut self.b, default);
        self
    }
    pub fn unreachable(&mut self) -> &mut Self {
        self.op(op::UNREACHABLE)
    }
    pub fn call(&mut self, f: u32) -> &mut Self {
        self.op(op::CALL);
        leb_u32(&mut self.b, f);
        self
    }
    pub fn call_indirect(&mut self, ty: u32, table: u32) -> &mut Self {
        self.op(op::CALL_INDIRECT);
        leb_u32(&mut self.b, ty);
        leb_u32(&mut self.b, table);
        self
    }
    pub fn return_call_indirect(&mut self, ty: u32, table: u32) -> &mut Self {
        self.op(op::RETURN_CALL_INDIRECT);
        leb_u32(&mut self.b, ty);
        leb_u32(&mut self.b, table);
        self
    }
    pub fn drop_(&mut self) -> &mut Self {
        self.op(op::DROP)
    }
    pub fn select(&mut self) -> &mut Self {
        self.op(op::SELECT)
    }
    pub fn lget(&mut self, i: u32) -> &mut Self {
        self.op(op::LOCAL_GET);
        leb_u32(&mut self.b, i);
        self
    }
    pub fn lset(&mut self, i: u32) -> &mut Self {
        self.op(op::LOCAL_SET);
        leb_u32(&mut self.b, i);
        self
    }
    pub fn ltee(&mut self, i: u32) -> &mut Self {
        self.op(op::LOCAL_TEE);
        leb_u32(&mut self.b, i);
        self
    }
    pub fn i32c(&mut self, v: i32) -> &mut Self {
        self.op(op::I32_CONST);
        leb_i32(&mut self.b, v);
        self
    }
    pub fn u32c(&mut self, v: u32) -> &mut Self {
        self.i32c(v as i32)
    }
    /// memory.copy (dest, src, len) on memory 0 (bulk memory operations)
    pub fn memory_copy(&mut self) -> &mut Self {
        self.b.extend_from_slice(&[0xfc, 0x0a, 0x00, 0x00]);
        self
    }
    /// memory.fill (dest, byte, len) on memory 0
    pub fn memory_fill(&mut self) -> &mut Self {
        self.b.extend_from_slice(&[0xfc, 0x0b, 0x00]);
        self
    }
    pub fn f32c(&mut self, v: f32) -> &mut Self {
        self.op(0x43);
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn f64c(&mut self, v: f64) -> &mut Self {
        self.op(0x44);
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i64c(&mut self, v: i64) -> &mut Self {
        self.op(op::I64_CONST);
        leb_i64(&mut self.b, v);
        self
    }
    /// Prefixed SIMD instruction (0xfd + LEB128 opcode).
    pub fn simd(&mut self, opc: u32) -> &mut Self {
        self.op(0xfd);
        leb_u32(&mut self.b, opc);
        self
    }
    /// SIMD memory instruction with alignment hint 0 and static offset.
    pub fn simd_mem(&mut self, opc: u32, offset: u32) -> &mut Self {
        self.simd(opc);
        leb_u32(&mut self.b, 0);
        leb_u32(&mut self.b, offset);
        self
    }
    /// SIMD lane-memory instruction (load/store lane): memarg + lane index.
    pub fn simd_mem_lane(&mut self, opc: u32, offset: u32, lane: u8) -> &mut Self {
        self.simd_mem(opc, offset);
        self.b.push(lane);
        self
    }
    /// SIMD lane instruction (extract/replace lane).
    pub fn simd_lane(&mut self, opc: u32, lane: u8) -> &mut Self {
        self.simd(opc);
        self.b.push(lane);
        self
    }
    pub fn v128_const(&mut self, bytes: [u8; 16]) -> &mut Self {
        self.simd(0x0c);
        self.b.extend_from_slice(&bytes);
        self
    }
    pub fn shuffle(&mut self, lanes: [u8; 16]) -> &mut Self {
        self.simd(0x0d);
        self.b.extend_from_slice(&lanes);
        self
    }

    /// Memory access with the given opcode, alignment hint 0 (byte aligned: always valid) and static offset.
    pub fn mem(&mut self, opc: u8, offset: u32) -> &mut Self {
        self.op(opc);
        leb_u32(&mut self.b, 0);
        leb_u32(&mut self.b, offset);
        self
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<u8>,
    pub results: Vec<u8>,
}

enum ImportKind {
    Func(u32),
    Memory(u32),
    Table(u32),
}

struct Import {
    module: String,
    name: String,
    kind: ImportKind,
}

struct Func {
    ty: u32,
    n_i32: u32,
    n_i64: u32,
    n_v128: u32,
    body: Code,
}

#[derive(Default)]
pub struct ModuleBuilder {
    types: Vec<FuncType>,
    imports: Vec<Import>,
    n_func_imports: u32,
    funcs: Vec<Func>,
    exports: Vec<(String, u32)>,
    func_names: Vec<(u32, String)>,
}

impl ModuleBuilder {
    pub fn new() -> ModuleBuilder {
        ModuleBuilder::default()
    }

    pub fn ty(&mut self, params: &[u8], results: &[u8]) -> u32 {
        let t = FuncType { params: params.to_vec(), results: results.to_vec() };
        if let Some(i) = self.types.iter().position(|x| *x == t) {
            return i as u32;
        }
        self.types.push(t);
        (self.types.len() - 1) as u32
    }

    /// Imported functions must all be declared before the first `add_func`. Returns the function index.
    pub fn import_func(&mut self, module: &str, name: &str, ty: u32) -> u32 {
        assert!(self.funcs.is_empty());
        self.imports.push(Import { module: module.into(), name: name.into(), kind: ImportKind::Func(ty) });
        self.n_func_imports += 1;
        self.n_func_imports - 1
    }
    pub fn import_memory(&mut self, module: &str, name: &str, min_pages: u32) {
        self.imports.push(Import { module: module.into(), name: name.into(), kind: ImportKind::Memory(min_pages) });
    }
    pub fn import_table(&mut self, module: &str, name: &str, min: u32) {
        self.imports.push(Import { module: module.into(), name: name.into(), kind: ImportKind::Table(min) });
    }

    /// Define a function; `n_i64` i64 locals, then `n_v128` v128 locals, then `n_i32` i32 locals are declared
    /// after the parameters. Returns the function index (imports come first).
    pub fn add_func(&mut self, ty: u32, n_i32: u32, n_i64: u32, n_v128: u32, body: Vec<u8>) -> u32 {
        self.add_code_func(ty, n_i32, n_i64, n_v128, Code { b: body, relocations: Vec::new(), record_relocations: false })
    }
    pub fn add_code_func(&mut self, ty: u32, n_i32: u32, n_i64: u32, n_v128: u32, body: Code) -> u32 {
        self.funcs.push(Func { ty, n_i32, n_i64, n_v128, body });
        self.n_func_imports + self.funcs.len() as u32 - 1
    }

    pub fn export_func(&mut self, name: &str, idx: u32) {
        self.exports.push((name.into(), idx));
    }

    /// Name of a defined function for the "name" custom section (shown by profilers and stack traces).
    pub fn name_func(&mut self, idx: u32, name: &str) {
        self.func_names.push((idx, name.into()));
    }

    fn section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
        out.push(id);
        leb_u32(out, payload.len() as u32);
        out.extend_from_slice(payload);
    }
    fn name(out: &mut Vec<u8>, s: &str) {
        leb_u32(out, s.len() as u32);
        out.extend_from_slice(s.as_bytes());
    }

    pub fn finish(&self) -> Vec<u8> {
        self.finish_relocatable().0
    }

    pub fn finish_relocatable(&self) -> (Vec<u8>, Vec<Relocation>) {
        let mut out = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        // types
        let mut p = Vec::new();
        leb_u32(&mut p, self.types.len() as u32);
        for t in &self.types {
            p.push(0x60);
            leb_u32(&mut p, t.params.len() as u32);
            p.extend_from_slice(&t.params);
            leb_u32(&mut p, t.results.len() as u32);
            p.extend_from_slice(&t.results);
        }
        Self::section(&mut out, 1, &p);
        // imports
        let mut p = Vec::new();
        leb_u32(&mut p, self.imports.len() as u32);
        for im in &self.imports {
            Self::name(&mut p, &im.module);
            Self::name(&mut p, &im.name);
            match im.kind {
                ImportKind::Func(t) => {
                    p.push(0x00);
                    leb_u32(&mut p, t);
                }
                ImportKind::Table(min) => {
                    p.push(0x01);
                    p.push(0x70); // funcref
                    p.push(0x00); // no max
                    leb_u32(&mut p, min);
                }
                ImportKind::Memory(min) => {
                    p.push(0x02);
                    p.push(0x00); // no max
                    leb_u32(&mut p, min);
                }
            }
        }
        Self::section(&mut out, 2, &p);
        // functions
        let mut p = Vec::new();
        leb_u32(&mut p, self.funcs.len() as u32);
        for f in &self.funcs {
            leb_u32(&mut p, f.ty);
        }
        Self::section(&mut out, 3, &p);
        // exports
        let mut p = Vec::new();
        leb_u32(&mut p, self.exports.len() as u32);
        for (n, idx) in &self.exports {
            Self::name(&mut p, n);
            p.push(0x00);
            leb_u32(&mut p, *idx);
        }
        Self::section(&mut out, 7, &p);
        // code
        let mut p = Vec::new();
        let mut relocations = Vec::new();
        leb_u32(&mut p, self.funcs.len() as u32);
        for f in &self.funcs {
            let mut body = Vec::new();
            let groups = (f.n_i32 > 0) as u32 + (f.n_i64 > 0) as u32 + (f.n_v128 > 0) as u32;
            leb_u32(&mut body, groups);
            // i64 and v128 locals come first (fixed indices right after the parameters), then the i32 ones
            if f.n_i64 > 0 {
                leb_u32(&mut body, f.n_i64);
                body.push(I64);
            }
            if f.n_v128 > 0 {
                leb_u32(&mut body, f.n_v128);
                body.push(V128);
            }
            if f.n_i32 > 0 {
                leb_u32(&mut body, f.n_i32);
                body.push(I32);
            }
            let prefix = body.len() as u32;
            body.extend_from_slice(&f.body.b);
            body.push(op::END);
            leb_u32(&mut p, body.len() as u32);
            let base = p.len() as u32 + prefix;
            relocations.extend(f.body.relocations.iter().cloned().map(|mut r| { r.offset += base; r }));
            p.extend_from_slice(&body);
        }
        Self::section(&mut out, 10, &p);
        let code_base = (out.len() - p.len()) as u32;
        for r in &mut relocations { r.offset += code_base; }
        if !self.func_names.is_empty() {
            // custom section "name", subsection 1 (function names): profilers and stack traces show them
            let mut names = self.func_names.clone();
            names.sort();
            let mut sub = Vec::new();
            leb_u32(&mut sub, names.len() as u32);
            for (idx, n) in &names {
                leb_u32(&mut sub, *idx);
                Self::name(&mut sub, n);
            }
            let mut p = Vec::new();
            Self::name(&mut p, "name");
            p.push(1);
            leb_u32(&mut p, sub.len() as u32);
            p.extend_from_slice(&sub);
            Self::section(&mut out, 0, &p);
        }
        (out, relocations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb() {
        let mut v = Vec::new();
        leb_i32(&mut v, -1);
        assert_eq!(v, [0x7f]);
        v.clear();
        leb_i32(&mut v, 63);
        assert_eq!(v, [0x3f]);
        v.clear();
        leb_i32(&mut v, 64);
        assert_eq!(v, [0xc0, 0x00]);
        v.clear();
        leb_i32(&mut v, -65);
        assert_eq!(v, [0xbf, 0x7f]);
        v.clear();
        leb_u32(&mut v, 624485);
        assert_eq!(v, [0xe5, 0x8e, 0x26]);
        v.clear();
        leb_i64(&mut v, i64::MIN);
        assert_eq!(v.len(), 10);
    }
}
