# Reads the running Memories Rescue window through UI Automation (WebView2
# exposes the page as an accessibility tree), optionally opens a page by
# pressing a named button, checks for expected text, and saves a screenshot.
# Used for the installed-app smoke test of the release checklist (SRS 10.5);
# needs no debugging port and no change to the app.
#
#   powershell -File tools/release/verify_window.ps1 -OutPng window.png `
#       -Press "About" -Expect "Memories Rescue","Third-party notices" -Reject "mock"
#
# Exit code 1 when the window is not found, an expected string is missing or
# a rejected string is present.
param(
    [string]$OutPng = "window.png",
    [string]$Press = "",
    [string[]]$Expect = @(),
    [string[]]$Reject = @(),
    [int]$WaitSeconds = 30
)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
$sig = @'
using System; using System.Runtime.InteropServices;
public static class Win32 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
}
'@
Add-Type -TypeDefinition $sig

function Get-Texts($root) {
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $texts = New-Object System.Collections.Generic.List[string]
    foreach ($el in $all) {
        $n = $el.Current.Name
        if ($n -and $n.Trim().Length -gt 0) { $texts.Add($n.Trim()) }
    }
    return $texts | Select-Object -Unique
}

$deadline = (Get-Date).AddSeconds($WaitSeconds)
$proc = $null
while ((Get-Date) -lt $deadline) {
    $proc = Get-Process memories-rescue -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    if ($proc) { break }
    Start-Sleep -Milliseconds 500
}
if (-not $proc) { Write-Error "app window not found"; exit 1 }
$hwnd = $proc.MainWindowHandle
"window: '$($proc.MainWindowTitle)' pid $($proc.Id)"
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)

# Wait until the page has something beyond the title.
while ((Get-Date) -lt $deadline) {
    if ((Get-Texts $root).Count -gt 3) { break }
    Start-Sleep -Milliseconds 500
}

if ($Press) {
    $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Press)
    $btn = $null
    while ((Get-Date) -lt $deadline) {
        $btn = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
        if ($btn) { break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $btn) { Write-Error "no element named '$Press'"; exit 1 }
    $pattern = $btn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $pattern.Invoke()
    Start-Sleep -Milliseconds 1200
    "pressed: $Press"
}

$texts = Get-Texts $root
"---- window text ----"
$texts
"---- checks ----"
$failed = $false
foreach ($e in $Expect) {
    $hit = $texts | Where-Object { $_ -like "*$e*" } | Select-Object -First 1
    if ($hit) { "  ok      contains '$e'" } else { "  MISSING '$e'"; $failed = $true }
}
foreach ($r in $Reject) {
    $hit = $texts | Where-Object { $_ -like "*$r*" } | Select-Object -First 1
    if ($hit) { "  PRESENT '$r' (rejected): $hit"; $failed = $true } else { "  ok      no '$r'" }
}

[void][Win32]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 600
$r = New-Object Win32+RECT
[void][Win32]::GetWindowRect($hwnd, [ref]$r)
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $w, $h))
$bmp.Save($OutPng, [System.Drawing.Imaging.ImageFormat]::Png)
"saved $OutPng ($w x $h)"
if ($failed) { exit 1 }
