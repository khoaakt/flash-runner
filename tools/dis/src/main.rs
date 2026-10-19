use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter, Instruction};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).unwrap();
    let ip: u64 = u64::from_str_radix(a.get(2).map(|s| s.trim_start_matches("0x")).unwrap_or("0"), 16).unwrap();
    let limit: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(80);
    let mut d = Decoder::with_ip(32, &bytes, ip, DecoderOptions::NONE);
    let mut f = IntelFormatter::new();
    let mut i = Instruction::default();
    let mut n = 0;
    while d.can_decode() && n < limit {
        d.decode_out(&mut i);
        let mut s = String::new(); f.format(&i, &mut s);
        let off = (i.ip() - ip) as usize;
        let hex: String = bytes[off..off + i.len()].iter().map(|b| format!("{b:02x}")).collect();
        println!("{:08x}  {:<24} {}", i.ip(), hex, s);
        n += 1;
        if i.mnemonic() == iced_x86::Mnemonic::Ret { break; }
    }
}
