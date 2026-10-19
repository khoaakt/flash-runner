//! Diagnostics sink: stderr natively, a host import on wasm.

#[cfg(target_arch = "wasm32")]
pub fn log(s: &str) {
    #[link(wasm_import_module = "env")]
    extern "C" {
        fn host_log(ptr: *const u8, len: usize);
    }
    unsafe { host_log(s.as_ptr(), s.len()) }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn log(s: &str) {
    eprintln!("{}", s);
}
