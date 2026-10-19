//! Execute receiver-memory LIR through the real dispatcher against native x86.
use super::*;

fn bytes(hex: &str) -> Vec<u8> { hex.as_bytes().chunks_exact(2).map(|v|u8::from_str_radix(std::str::from_utf8(v).unwrap(),16).unwrap()).collect() }
fn native(core: u32, body: &str) -> Vec<u8> {
    let mut code=bytes("558bec83ec18895dfc8b4d088b45108d55f08b1d00000000894df4895df08915000000008b1d000000003bd38b5dfc7308e8000000008b4510");
    for (at,value) in [(20,core+48),(32,core+48),(38,core+32)] {code[at..at+4].copy_from_slice(&value.to_le_bytes());}
    code.extend(bytes(body));let at=code.len();code.extend(bytes("8b4df0890d000000008be55dc3"));
    code[at+5..at+9].copy_from_slice(&(core+48).to_le_bytes());code
}

#[test]
fn receiver_lir_read_write_and_update_match_native_with_fault_alias_smc_fallback() {
    let core=DATA+0x200;
    for (body,source,bindings,fields,store,argc) in [
        ("8b088b4158","params i32\na = parami 0\nreti a",vec![0x80000058],vec![88],None,0),
        ("8b108b42588b4a5403c1","params i32 i32\na = parami 0\nb = parami 1\nr = addi a b\nreti r",vec![0x80000058,0x80000054],vec![88,84],None,0),
        ("8b088b50048951588b4158","params i32\na = parami 0\nreti a",vec![4],vec![88],Some(88),1),
        ("8b088b50048b415803c28941588b4158","params i32 i32\na = parami 0\nb = parami 1\nr = addi a b\nreti r",vec![0x80000058,4],vec![88],Some(88),1),
    ] {
        let code=native(core,body);
        for case in 0..13 {
            install_backend();let mut mem=build_mem(101,&code);let mut actual=initial_cpu(&mut Rng::new(91));let sp=actual.r[ESP];
            let mut object=DATA+0x1000;
            if case==1 {object=PG_NONE;} // no guest read permission
            if case==2 {object=sp+64-88;} // object field aliases argv
            if case==3 {object=core+48-88;} // field aliases transient MethodFrame link
            if case==4 {object=sp-88;} // native transient frame alias
            if case==5 {object=u32::MAX-40;} // overflowing field range
            if case==6 {object=CODE+0x300-88;mem.set_prot(CODE,4096,P_MAPPED|P_R|P_W|P_X);} // active code page
            if case==12 {object=PG_WONLY;} // store may succeed before a later native read faults
            mem.write_bytes(CODE+0x200,&[0xcd,0x82]).unwrap();
            for (address,value) in [(sp,CODE+0x200),(sp+4,0x1234),(sp+8,argc),(sp+12,sp+64),(sp+64,object),
                (sp+68,if case>=8{[u32::MAX,0,0x80000000,0x7fffffff][case%4]}else{0x80000001}),(core+32,sp-4096),(core+48,0x12345678)] {mem.w32(address,value).unwrap();}
            if object>=DATA && object<DATA+0x10000 && mem.page_prot(object+88)&P_W!=0 {mem.w32(object+88,if case>=8{[0,0x80000000,u32::MAX,0x7fffffff][case%4]}else{0x7fffffff}).unwrap();mem.w32(object+84,77).unwrap();}
            if case==7 {mem.w32(sp+8,argc+1).unwrap();}
            let mut cache=BlockCache::new();cache.jit.threshold=100;cache.jit.layout_override=Some((GUEST,RTAB,WTAB,0));
            crate::lir::pure::install_memory(&mut mem,&mut cache.jit,CODE,core,&code,source,&bindings,&fields,store,argc).unwrap();
            let mut oracle=build_mem(101,&code);oracle.raw_mut().copy_from_slice(mem.raw());
            if case==6 {oracle.set_prot(CODE,4096,P_MAPPED|P_R|P_W|P_X);}
            let mut expected=actual.clone();let exit=expected.run(&mut oracle,&mut BlockCache::new(),1000);
            assert_eq!(actual.run(&mut mem,&mut cache,1000),exit,"{body} case{case}");
            for reg in [EAX,ESP,EBX,EBP,ESI,EDI] {assert_eq!(actual.r[reg],expected.r[reg],"{body} case{case} reg{reg}");}
            if case==0 || (8..=11).contains(&case) {
                assert_eq!(actual.jit_lir_completed,1);assert_eq!(mem.r32(core+48).unwrap(),0x12345678);
                assert_eq!(mem.r32(object+88).unwrap(),oracle.r32(object+88).unwrap());
            } else if case!=6 || store.is_some() {assert_eq!(actual.jit_lir_completed,0,"unsafe {body} case{case} unexpectedly committed");}
            if case==12 {assert_eq!(mem.read_bytes(object+88,4).unwrap(),oracle.read_bytes(object+88,4).unwrap(),"native partial-store fault effects must survive fallback");}
        }
    }
}
