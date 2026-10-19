//! Offline ELF compiler. Plugin-complete preserves exact existing HLE frontiers;
//! all other valid discovered instructions are emitted offline. See emu::aot.
use emu::{
    aot::{self, Options},
    jit_x86::Layout,
};

fn number(s: &str) -> Result<u32, String> {
    if let Some(h) = s.strip_prefix("0x") {
        u32::from_str_radix(h, 16)
    } else {
        s.parse()
    }
    .map_err(|_| format!("invalid u32: {s}"))
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or(concat!(
        "usage: emu-aot INPUT.elf OUTPUT_PREFIX [--portable | --partial | --flash-leaves | --plugin-functions | --plugin-complete] --entry ELF_VADDR [--entry ...] [--guest-base N --rtab N --wtab N] [--load-bias N] [--allow START:END | --profile FILE] [--max-instructions N] [--max-blocks N]\n",
        "--portable emits symbolic pointer/load-bias relocations; --partial also leaves unsupported instructions and existing HLE functions to the dispatcher. ",
        "--plugin-functions discovers function entries from ELF symbols/unwind/initializer metadata and emits PREFIX-NNN.{aot,wasm,json} plus PREFIX.index.json with measured static coverage. ",
        "--plugin-complete emits v4 for all valid discovered non-HLE instructions, including generic interpreter calls and exact fallback instruction bytes, preserving known exact HLE functions as explicit dispatcher frontiers before fusion. Defaults: 5000000 instructions/1048576 blocks. It does not infer indirect targets or guarantee native FP execution/speedup. ",
        "--plugin-startup is a compatibility alias for --plugin-functions. --flash-leaves explicitly selects known HLE leaf bytes instead (not recommended for normal playback). ",
        "Profile: one half-open ELF virtual-address range START:END per line; # comments allowed. Outputs PREFIX.aot, PREFIX.wasm and PREFIX.json. CPU ABI is checked at installation. ",
        "Portable bundles require bind(actual_layout,actual_load_bias); v4 fallback additionally requires allocated runtime tags via bind_with_tags. No runtime x86 translation is performed by binding."
    ))?;
    let output = args.next().ok_or("missing output prefix")?;
    let mut entries = Vec::new();
    let mut allowlist = Vec::new();
    let mut bias = 0;
    let mut portable = false;
    let mut flash_leaves = false;
    let mut partial = false;
    let mut plugin_startup = false;
    let mut plugin_complete = false;
    let (mut guest, mut rtab, mut wtab) = (None, None, None);
    let (mut max_instructions, mut max_blocks) = (100_000, 1024);
    let (mut explicit_instruction_limit, mut explicit_block_limit) = (false, false);
    fn range(s: &str) -> Result<(u32, u32), String> {
        let (a, b) = s.split_once(':').ok_or("range must be START:END")?;
        Ok((number(a.trim())?, number(b.trim())?))
    }
    while let Some(flag) = args.next() {
        if flag == "--portable" { portable = true; continue; }
        if flag == "--flash-leaves" { flash_leaves = true; portable = true; continue; }
        if flag == "--partial" { partial = true; portable = true; continue; }
        if matches!(flag.as_str(), "--plugin-startup" | "--plugin-functions") { plugin_startup = true; portable = true; continue; }
        if flag == "--plugin-complete" { plugin_startup = true; plugin_complete = true; portable = true; continue; }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--entry" => entries.push(number(&value)?),
            "--allow" => allowlist.push(range(&value)?),
            "--profile" => {
                let text = std::fs::read_to_string(&value).map_err(|e| e.to_string())?;
                for line in text
                    .lines()
                    .map(|l| l.split('#').next().unwrap().trim())
                    .filter(|l| !l.is_empty())
                {
                    allowlist.push(range(line)?);
                }
            }
            "--load-bias" => bias = number(&value)?,
            "--guest-base" => guest = Some(number(&value)?),
            "--rtab" => rtab = Some(number(&value)?),
            "--wtab" => wtab = Some(number(&value)?),
            "--max-instructions" => { max_instructions = number(&value)? as usize; explicit_instruction_limit = true; }
            "--max-blocks" => { max_blocks = number(&value)? as usize; explicit_block_limit = true; }
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let mut layout = Layout::for_cpu(
        guest.or(portable.then_some(0)).ok_or("--guest-base required (or use --portable)")?,
        rtab.or(portable.then_some(1)).ok_or("--rtab required (or use --portable)")?,
        wtab.or(portable.then_some(2)).ok_or("--wtab required (or use --portable)")?,
        0,
    );
    layout.simd = false;
    let image = std::fs::read(input).map_err(|e| e.to_string())?;
    if plugin_startup {
        if !entries.is_empty() || !allowlist.is_empty() || bias != 0 || guest.is_some() || rtab.is_some() || wtab.is_some() || flash_leaves || partial {
            return Err("--plugin-functions cannot be combined with explicit layout/entry/profile/compiler modes".into());
        }
        let compiler = if plugin_complete { aot::compile_plugin_complete_bundles_with_limits } else { aot::compile_plugin_bundles_with_limits };
        let (artifacts, report) = compiler(&image, if explicit_instruction_limit { max_instructions } else if plugin_complete { 5_000_000 } else { 1_000_000 }, if explicit_block_limit { max_blocks } else if plugin_complete { 1_048_576 } else { 65_536 })?;
        let mut index = format!("{{\"format\":\"flash-runner-aot-index-v1\",\"coverage\":{},\"artifacts\":[", report.json());
        for (i, artifact) in artifacts.iter().enumerate() {
            let prefix = format!("{output}-{i:03}");
            write_artifact(&prefix, artifact)?;
            if i != 0 { index.push(','); }
            let name = std::path::Path::new(&prefix).file_name().and_then(|v| v.to_str()).ok_or("invalid output filename")?;
            // Filename escaping is kept deliberately strict for the packaged manifest.
            if name.contains(['\"', '\\']) || name.chars().any(char::is_control) { return Err("unsupported output filename".into()); }
            index.push_str(&format!("{{\"file\":\"{name}.aot\",\"regions\":{},\"wasm_bytes\":{}}}", artifact.regions.len(), artifact.wasm.len()));
        }
        index.push_str("]}\n");
        std::fs::write(format!("{output}.index.json"), index).map_err(|e| e.to_string())?;
        eprintln!("compiled {} static blocks into {} fused regions / {} exported heads / {} internal static edges; {} instructions / {} compiled guest bytes / {} guarded bytes in {} offline modules; {} decoded instructions, {} HLE frontiers, {} budget frontiers, {} invalid frontiers; {} instructions / {} bytes use generic interpreter calls emitted offline; no runtime x86 translation", report.compiled_blocks, report.fused_regions, report.exported_heads, report.internal_static_edges, report.compiled_instructions, report.compiled_bytes, report.guarded_bytes, artifacts.len(), report.decoded_instructions, report.hle_frontiers, report.budget_frontiers, report.invalid_frontiers, report.fallback_instructions, report.fallback_bytes);
        return Ok(());
    }
    let artifact = if flash_leaves { aot::compile_flash_leaves(&image)? } else { (if partial { aot::compile_portable_partial } else if portable { aot::compile_portable } else { aot::compile })(
        &image,
        &Options {
            load_bias: bias,
            entries,
            allowlist,
            max_instructions,
            max_blocks,
            layout,
        },
    )? };
    write_artifact(&output, &artifact)?;
    eprintln!(
        "compiled {} blocks ({} wasm bytes); {} artifact; install through the guarded runtime loader",
        artifact.regions.len(),
        artifact.wasm.len(),
        if portable { "portable" } else { "fixed-layout" }
    );
    Ok(())
}

fn write_artifact(output: &str, artifact: &aot::Artifact) -> Result<(), String> {
    let bundle = artifact.encode_bundle();
    aot::Artifact::decode(&bundle).map_err(|e| format!("output bundle validation failed: {e}"))?;
    std::fs::write(format!("{output}.aot"), bundle).map_err(|e| e.to_string())?;
    std::fs::write(format!("{output}.wasm"), &artifact.wasm).map_err(|e| e.to_string())?;
    std::fs::write(format!("{output}.json"), artifact.manifest_json()).map_err(|e| e.to_string())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("emu-aot: {error}");
        std::process::exit(1);
    }
}
