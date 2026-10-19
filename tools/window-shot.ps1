# Windows only: save a screenshot of one whole application window (title bar, tabs, address bar and all), not just its page.
#   powershell -NoProfile -File tools/window-shot.ps1 -Title "Google Chrome" -Out shot.png -ProcId <pid>[,<pid>...] [-Wait 20]
# -Title is a regular expression matched against window titles; the largest visible match is raised to the front and captured as it is on screen.
# -ProcId limits the match to the windows of those processes (Firefox starts through a launcher, so give every process of the test browser): pass the browser the test started, so a browser of your own with other tabs open is never captured.
# -Wait keeps looking for the window for that many seconds. Used for the README screenshots.
param(
  [Parameter(Mandatory = $true)][string]$Title,
  [Parameter(Mandatory = $true)][string]$Out,
  [Parameter(Mandatory = $true)][string]$ProcId,
  [int]$Wait = 20
)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Win {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static List<IntPtr> Visible() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => { if (IsWindowVisible(h)) list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }
  public static string TitleOf(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
  public static uint PidOf(IntPtr h) { uint p; GetWindowThreadProcessId(h, out p); return p; }
}
'@
[void][Win]::SetProcessDPIAware()   # real pixels, not scaled ones
$ids = @($ProcId -split '[,\s]+' | Where-Object { $_ } | ForEach-Object { [int]$_ })

$deadline = (Get-Date).AddSeconds($Wait)
$best = $null; $bestArea = 0
do {
  foreach ($h in [Win]::Visible()) {
    $t = [Win]::TitleOf($h)
    if (-not $t -or $t -notmatch $Title) { continue }
    if ($ids -notcontains [int][Win]::PidOf($h)) { continue }
    $r = New-Object Win+RECT
    [void][Win]::DwmGetWindowAttribute($h, 9, [ref]$r, [Runtime.InteropServices.Marshal]::SizeOf($r))   # DWMWA_EXTENDED_FRAME_BOUNDS: the frame without the shadow
    $area = ($r.Right - $r.Left) * ($r.Bottom - $r.Top)
    if ($area -gt $bestArea) { $best = $h; $bestArea = $area }
  }
  if (-not $best) { Start-Sleep -Milliseconds 500 }
} while (-not $best -and (Get-Date) -lt $deadline)
if (-not $best) { Write-Error "no visible window matching '$Title'"; exit 1 }

if ([Win]::IsIconic($best)) { [void][Win]::ShowWindow($best, 9) }
[void][Win]::SetWindowPos($best, [IntPtr](-1), 0, 0, 0, 0, 0x0003)   # topmost for the moment of the capture (no move, no resize)
[void][Win]::SetForegroundWindow($best)
Start-Sleep -Milliseconds 700
$rect = New-Object Win+RECT
[void][Win]::DwmGetWindowAttribute($best, 9, [ref]$rect, [Runtime.InteropServices.Marshal]::SizeOf($rect))
$full = New-Object Win+RECT
[void][Win]::GetWindowRect($best, [ref]$full)
$fw = $full.Right - $full.Left; $fh = $full.Bottom - $full.Top
$whole = New-Object System.Drawing.Bitmap $fw, $fh
$gw = [System.Drawing.Graphics]::FromImage($whole)
$hdc = $gw.GetHdc()
$ok = [Win]::PrintWindow($best, $hdc, 2)   # PW_RENDERFULLCONTENT
$gw.ReleaseHdc($hdc)
$w = $rect.Right - $rect.Left; $h2 = $rect.Bottom - $rect.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h2
$g = [System.Drawing.Graphics]::FromImage($bmp)
if ($ok) { $g.DrawImage($whole, [System.Drawing.Rectangle]::new(0, 0, $w, $h2), [System.Drawing.Rectangle]::new($rect.Left - $full.Left, $rect.Top - $full.Top, $w, $h2), [System.Drawing.GraphicsUnit]::Pixel) }
else { $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size) }
[void][Win]::SetWindowPos($best, [IntPtr](-2), 0, 0, 0, 0, 0x0003)   # not topmost any more
New-Item -ItemType Directory -Force -Path (Split-Path -Parent ([IO.Path]::GetFullPath($Out))) | Out-Null
$bmp.Save([IO.Path]::GetFullPath($Out), [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output ("{0}x{1}  '{2}'  -> {3}" -f $w, $h2, [Win]::TitleOf($best), $Out)
