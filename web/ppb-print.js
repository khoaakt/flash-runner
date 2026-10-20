// Printing for flash.net.PrintJob. Flash does not render pages into the Pepper calls it makes: PrintJob.start() calls PPB_Flash_Print::InvokePrinting
// and asks PPB_Printing for the page geometry, and the pages themselves are pulled by the *browser* through the plugin's PPP_Printing(Dev)
// interface (QuerySupportedFormats, Begin, PrintPages, End). Pepper Flash offers one output format, PDF (PrintPages returns a PPB_Buffer holding
// the PDF). The player frame has no print machinery, so this plays the browser's part: on InvokePrinting it runs that PPP sequence and hands the
// PDF bytes to the frame's 'print-pdf' service, which shows them to the browser's own PDF viewer and prints them.
const FORMAT_PDF = 2;
const PAGE = { w: 612, h: 792 };            // US Letter in points; the same geometry PPB_Printing.GetDefaultPrintSettings reports (72 dpi)
const MAX_PDF_BYTES = 64 * 1024 * 1024;

export function printSettings(host, format) {
  const p = host.alloc(60);
  for (const base of [0, 16]) { host.wi32(p + base, 0); host.wi32(p + base + 4, 0); host.wi32(p + base + 8, PAGE.w); host.wi32(p + base + 12, PAGE.h); }   // printable_area, content_area
  host.wi32(p + 32, PAGE.w); host.wi32(p + 36, PAGE.h);
  host.wi32(p + 40, 72); host.wi32(p + 44, 0); host.wi32(p + 48, 0); host.wi32(p + 52, 0);   // dpi, orientation, scaling, colour
  host.wi32(p + 56, format);
  return p;
}

/** Run the browser's side of a print: returns the PDF as a Uint8Array, or throws with the step that failed. */
export async function collectPdf(host, instance) {
  let iface = 0;
  for (const name of ['PPP_Printing(Dev);0.6', 'PPP_Printing(Dev);0.7']) {
    const r = await host.call(host.getPPP, [host.allocCStr(name)]);
    if (r.lo) { iface = r.lo; break; }
  }
  if (!iface) throw new Error('the plugin has no PPP_Printing interface');
  const fn = (i) => host.u32(iface + i * 4);           // QuerySupportedFormats, Begin, PrintPages, End, IsScalingDisabled
  const formats = (await host.call(fn(0), [instance])).lo >>> 0;
  host.log(`[print] supported formats 0x${formats.toString(16)}`);
  if (!(formats & FORMAT_PDF)) throw new Error('the plugin cannot print PDF');
  const count = (await host.call(fn(1), [instance, printSettings(host, FORMAT_PDF)])).lo | 0;
  host.log(`[print] Begin -> ${count} page(s)`);
  if (count <= 0) { await host.call(fn(3), [instance]); throw new Error('the movie has nothing to print'); }
  try {
    const range = host.alloc(8); host.w32(range, 0); host.w32(range + 4, count - 1);
    const res = (await host.call(fn(2), [instance, range, 1])).lo;
    const buf = res && host.getRes(res, 'buffer');
    host.log(`[print] PrintPages(0..${count - 1}) -> resource ${res}${buf ? ` ${buf.size} bytes` : ''}`);
    if (!buf) throw new Error('PrintPages returned no buffer');
    if (buf.size > MAX_PDF_BYTES) throw new Error('the PDF is too large');
    const pdf = host.ro(buf.ptr, buf.size).slice();
    host.releaseRes(res);
    return pdf;
  } finally { await host.call(fn(3), [instance]); }
}
