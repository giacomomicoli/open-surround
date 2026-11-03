# Regressione offline dell'host contro le registrazioni di Windows già salvate (doc sez. 12.10).
# Uso:  powershell -File docs\reference\regress-host.ps1 [-Filter gaming*] [-Baseline <os-dap-host.exe>] [-SkipLong]
# Per ogni cattura in target\verify\<profilo>[\<prova>] (cartella con loopback.wav, info.txt e i blob
# Dolby) rilegge da info.txt l'ingresso, il volume dell'endpoint e se lo stream era WASAPI, riesegue
# os-dap-host con gli stessi blob e lo confronta con la loopback.wav di Windows (wavcmp).
# Non serve Dolby Access né cambiare profilo: è la verifica di verify-profile.ps1 senza la cattura.
# Con -Baseline esegue anche un altro os-dap-host (es. quello di una fase precedente) e controlla che
# le due uscite siano identiche byte per byte. -SkipLong salta gli ingressi oltre 50 MB (audio reale).
param(
    [string]$Filter = "*",
    [string]$Baseline,
    [switch]$SkipLong,
    [string]$HostExe
)
$ErrorActionPreference = "Continue"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$bin = Join-Path $root "target\debug"
if (-not $HostExe) { $HostExe = Join-Path $bin "os-dap-host.exe" }
$verify = Join-Path $root "target\verify"
$out = Join-Path $root "target\regress"
New-Item -ItemType Directory -Force $out | Out-Null
$inv = [Globalization.CultureInfo]::InvariantCulture

$cases = Get-ChildItem $verify -Recurse -Filter info.txt |
    Where-Object { Test-Path (Join-Path $_.DirectoryName "loopback.wav") } |
    ForEach-Object { $_.Directory } |
    Where-Object { $_.FullName.Substring($verify.Length + 1).Split('\')[0] -like $Filter } |
    Sort-Object FullName

$summary = @()
foreach ($dir in $cases) {
    $name = $dir.FullName.Substring($verify.Length + 1) -replace '\\', '_'
    $info = Get-Content (Join-Path $dir.FullName "info.txt") -Raw
    if ($info -notmatch 'volume (-?[0-9.]+) dB') { Write-Warning "$name`: volume non trovato"; continue }
    $volume = [double]::Parse($Matches[1], $inv)
    if ($info -notmatch '(?m)^ingresso: (.+) \((\d+) canali') { Write-Warning "$name`: ingresso non trovato"; continue }
    $wav = $Matches[1]
    if (-not [IO.Path]::IsPathRooted($wav)) { $wav = Join-Path $root $wav }
    if (-not (Test-Path $wav)) { Write-Warning "$name`: ingresso $wav mancante"; continue }
    if ($SkipLong -and (Get-Item $wav).Length -gt 50MB) { continue }
    $endpoint = (Get-ChildItem $dir.FullName -Filter "*-2.bin" | Select-Object -First 1).Name.Split('-')[0]
    $hostArgs = @()
    if ($info -match 'stream WASAPI') { $hostArgs += "--stereo-bypass" }
    $hostArgs += @("--volume-db", $volume.ToString($inv), "--capture", $dir.FullName, "--endpoint", $endpoint, $wav)

    $new = Join-Path $out "$name-host.wav"
    & $HostExe @hostArgs $new 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $new)) { throw "os-dap-host fallito per $name (bloccato da Smart App Control? ricompilare)" }
    $same = ""
    if ($Baseline) {
        $base = Join-Path $out "$name-base.wav"
        & $Baseline @hostArgs $base 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "baseline fallita per $name" }
        $same = if ((Get-FileHash $new).Hash -eq (Get-FileHash $base).Hash) { "si" } else { "NO" }
        Remove-Item $base
    }
    $cmp = & "$bin\wavcmp.exe" --segments 1 (Join-Path $dir.FullName "loopback.wav") $new
    $cmp | Set-Content (Join-Path $out "$name-wavcmp.txt")
    $res = @($cmp | Select-String 'guadagno stimato ([-+0-9.]+) dB, residuo ([-0-9.]+) dB' |
        ForEach-Object { [double]::Parse($_.Matches[0].Groups[2].Value, $inv) })
    $gain = @($cmp | Select-String 'guadagno stimato ([-+0-9.]+) dB' |
        ForEach-Object { [math]::Abs([double]::Parse($_.Matches[0].Groups[1].Value, $inv)) })
    $row = [ordered]@{
        caso = $name
        "residuo peggiore (dB)" = if ($res) { ($res | Measure-Object -Maximum).Maximum } else { "?" }
        "|guadagno| max (dB)" = if ($gain) { ($gain | Measure-Object -Maximum).Maximum } else { "?" }
    }
    if ($Baseline) { $row["= baseline"] = $same }
    $summary += [pscustomobject]$row
    Remove-Item $new
}
$summary | Format-Table -AutoSize
$worst = ($summary | ForEach-Object { $_."residuo peggiore (dB)" } | Where-Object { $_ -is [double] } | Measure-Object -Maximum).Maximum
"casi: $($summary.Count)  residuo peggiore: $worst dB"
if ($Baseline) { "diversi dalla baseline: " + @($summary | Where-Object { $_."= baseline" -ne "si" }).Count }
