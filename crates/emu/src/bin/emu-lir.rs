//! Compile portable normalized LIR, not a proprietary LIns dump.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        eprintln!("Usage: emu-lir INPUT.lir OUTPUT.wasm\nSee emu::lir module docs for normalized syntax.\nExport: trace(declared scalar params) -> (status:i32, payload:i64).\nStatus 0: reti sign-extended i32/retd IEEE bits; positive: guard ID, payload 0.\nStandalone backend; not integrated with proprietary Flash capture/call ABI.");
        std::process::exit(2);
    }
    let text = std::fs::read_to_string(&args[0])?;
    let trace = emu::lir::Trace::parse(&text)?;
    let wasm = trace.compile();
    std::fs::write(&args[1], &wasm)?;
    eprintln!(
        "{} bytes; trace({:?}) -> (status:i32, {:?} bits:i64)",
        wasm.len(),
        trace.params(),
        trace.result()
    );
    Ok(())
}
