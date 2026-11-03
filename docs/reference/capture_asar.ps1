# Cattura i blob parametri DAP-VR ("ASAR") scritti da Dolby Access nel property store
# degli endpoint audio e li salva in docs/reference/captures/<nome>/.
# Uso:  powershell -File capture_asar.ps1 -Name music-warm
# Salva, per ogni endpoint che ha il contesto Dolby, tutti i valori del contesto: {1b4dab55-...},N come
# <endpoint>-N.bin (cuffie: init ,4 + runtime ,5; speaker: init ,1 + runtime ,2), gli altri come
# <endpoint>-<primi 8 hex del fmtid>-N.bin
# e stampa un riepilogo con hash, cosi' si vede quali endpoint hanno ricevuto l'ultima scrittura.
param([Parameter(Mandatory = $true)][string]$Name)

$out = Join-Path $PSScriptRoot "captures\$Name"
New-Item -ItemType Directory -Force $out | Out-Null
$root = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render"
$ctx = "{45da5c30-2837-4ac4-b1e2-50acc3865974}"
$pset = "{1b4dab55-b1fb-4d8c-8317-f2d4a96efbb8}"
$sha256 = [System.Security.Cryptography.SHA256]::Create()

foreach ($ep in Get-ChildItem $root) {
    $key = Get-Item (Join-Path $ep.PSPath "FxProperties\$ctx\User") -ErrorAction SilentlyContinue
    if (-not $key) { continue }
    $friendly = ""
    try { $friendly = (Get-ItemProperty (Join-Path $ep.PSPath "Properties") -ErrorAction Stop).'{a45c254e-df1c-4efd-8020-67d146a850e0},2' } catch {}
    $short = $ep.PSChildName.Substring(1, 8)
    foreach ($vname in $key.GetValueNames()) {
        if ($vname -notmatch '^\{([0-9a-f]{8})-[0-9a-f-]+\},(\d+)$') { continue }
        $idx = if ($vname.StartsWith($pset)) { $Matches[2] } else { "$($Matches[1])-$($Matches[2])" }
        $val = $key.GetValue($vname)
        if ($val -isnot [byte[]]) { continue }
        $hash = ([System.BitConverter]::ToString($sha256.ComputeHash($val))).Replace('-', '').Substring(0, 10)
        [System.IO.File]::WriteAllBytes((Join-Path $out "$short-$idx.bin"), $val)
        "{0,-28} {1}  idx{2}  {3,4} B  sha {4}" -f $friendly, $short, $idx, $val.Length, $hash
    }
}

# Ultime scritture registrate dall'app, per sapere quali endpoint sono aggiornati.
$log = Get-ChildItem "$env:LOCALAPPDATA\Packages\DolbyLaboratories.DolbyAccess_rz1tebttyb220\LocalCache\DolbyAccess_*.log" |
    Sort-Object LastWriteTime | Select-Object -Last 1
if ($log) {
    "--- ultime righe di $($log.Name) ---"
    Get-Content $log.FullName -Tail 300 | Select-String "Selected profile|set or updated|written to Property Store" |
        Select-Object -Last 8 | ForEach-Object {
            ($_ -replace '\]\[[A-Z]+\]\[[^\]]*\]::[0-9a-f]+::', ' ') -replace '\\\\\?\\SWD#MMDEVAPI#\{0\.0\.0\.00000000\}\.\{([0-9a-f]{8})[^}]*\}#\{[^}]*\}', 'EP{$1}'
        }
}
