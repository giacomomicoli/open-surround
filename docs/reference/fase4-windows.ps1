# Fase 4, parte conclusiva su Windows (doc sez. 12.13, passi W6-W8): traccia ETW del modulo Dolby
# dentro audiodg e riferimenti delle sessioni separate da una pausa e del cambio di profilo a caldo.
# Va lanciato da PowerShell COME AMMINISTRATORE (logman), dalla radice del repository, dopo cargo build:
#   powershell -ExecutionPolicy Bypass -File docs\reference\fase4-windows.ps1 -Step controllo
#   powershell -ExecutionPolicy Bypass -File docs\reference\fase4-windows.ps1 -Step sessioni
#   powershell -ExecutionPolicy Bypass -File docs\reference\fase4-windows.ps1 -Step cambio
# controllo = W6, prima prova (la traccia raccoglie il log del modulo?); sessioni = W6 (senza
# loopback) e W7 (con loopback), Film Equilibrato, pause 500/2000/5000/15000 ms; cambio = W8, tre cambi
# di profilo fatti a mano in Dolby Access al messaggio "CAMBIA PROFILO ADESSO".
# Prima di ogni prova lo script controlla che il profilo attivo sia quello giusto confrontando i blob
# Dolby del registro con quelli già catturati in target\verify\<profilo>\canali71 (e se no aspetta
# che lo si selezioni). Tutto finisce in target\verify-fase4\ (catture, .etl, .csv, .xml, log).
# Le stringhe sono in ASCII: PowerShell 5.1 legge i .ps1 senza BOM come ANSI.
param(
    [ValidateSet("controllo", "sessioni", "cambio")][Parameter(Mandatory = $true)][string]$Step,
    [string]$Endpoint = "32ed16b6",
    [string]$Provider = "{ebfc27e1-9503-4ad5-bf27-8c6bc695c59a}",
    [int[]]$Pauses = @(500, 2000, 5000, 15000)
)
$ErrorActionPreference = "Continue"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $root
$rc = Join-Path $root "target\debug\ref-capture.exe"
$sig = Join-Path $root "target\verify\signals"
$ref = Join-Path $root "target\verify"
$out = Join-Path $root "target\verify-fase4"
New-Item -ItemType Directory -Force $out | Out-Null

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { throw "serve PowerShell come amministratore (logman)" }
if (-not (Test-Path $rc)) { throw "manca $rc : cargo build" }

Start-Transcript (Join-Path $out "fase4-$Step.log") -Append | Out-Null

# --- profilo attivo -------------------------------------------------------------------------------
$base = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render"
$guid = (Get-ChildItem $base | Where-Object { $_.PSChildName -like "{$Endpoint*" }).PSChildName
if (-not $guid) { throw "endpoint $Endpoint non trovato" }
$dolbyKey = "$base\$guid\FxProperties\{45da5c30-2837-4ac4-b1e2-50acc3865974}\User"

# Blob attuali, con i nomi dei file di ref-capture / capture_asar.ps1.
function Get-CurrentBlobs {
    $k = Get-Item $dolbyKey
    $cur = @{}
    foreach ($n in $k.GetValueNames()) {
        $v = $k.GetValue($n)
        if ($v -isnot [byte[]]) { continue }
        $ln = $n.ToLower()
        $i = $ln.LastIndexOf(',')
        $fmt = $ln.Substring(0, $i); $p = $ln.Substring($i + 1)
        if ($fmt -eq "{1b4dab55-b1fb-4d8c-8317-f2d4a96efbb8}") { $f = "$Endpoint-$p.bin" }
        else { $f = "$Endpoint-$($fmt.TrimStart('{').Substring(0, 8))-$p.bin" }
        $cur[$f] = [Convert]::ToBase64String($v)
    }
    $cur
}

# Numero di blob diversi fra quelli attuali e quelli catturati per il profilo.
function Get-ProfileDiff([string]$ProfileDir) {
    $cur = Get-CurrentBlobs
    $files = Get-ChildItem (Join-Path $ref "$ProfileDir\canali71") -Filter *.bin
    @($files | Where-Object {
            -not $cur.ContainsKey($_.Name) -or $cur[$_.Name] -ne [Convert]::ToBase64String([IO.File]::ReadAllBytes($_.FullName))
        }).Count
}

function Wait-Profile([string]$ProfileDir, [string]$Label) {
    while (($d = Get-ProfileDiff $ProfileDir) -gt 0) {
        Read-Host "Profilo attivo diverso da '$Label' ($d blob diversi da target\verify\$ProfileDir). Selezionalo in Dolby Access e premi Invio"
    }
    "  profilo attivo: $Label (blob identici a target\verify\$ProfileDir\canali71)"
}

# --- traccia ETW ----------------------------------------------------------------------------------
function Start-Trace([string]$Name) {
    $etl = Join-Path $out "etw-$Name.etl"
    logman stop dolby-apo -ets 2>&1 | Out-Null
    Remove-Item $etl, ($etl -replace '\.etl$', '.csv'), ($etl -replace '\.etl$', '.xml') -ErrorAction SilentlyContinue
    logman start dolby-apo -p $Provider 0xffffffffffffffff 5 -o $etl -ets | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "logman start fallito ($LASTEXITCODE)" }
    $etl
}

function Stop-Trace([string]$Etl) {
    logman stop dolby-apo -ets | Out-Null
    $ev = @(Get-WinEvent -Path $Etl -Oldest -ErrorAction SilentlyContinue)
    $ev | Select-Object @{n = 'ora'; e = { $_.TimeCreated.ToString('HH:mm:ss.fff') } }, ProcessId,
    @{n = 'Message'; e = { if ($_.Message) { $_.Message } else { ($_.Properties | ForEach-Object { $_.Value }) -join ' ' } } } |
        Export-Csv ($Etl -replace '\.etl$', '.csv') -NoTypeInformation -Encoding UTF8
    tracerpt $Etl -o ($Etl -replace '\.etl$', '.xml') -of XML -y 2>&1 | Out-Null
    $adg = @(Get-Process audiodg -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    $inAdg = @($ev | Where-Object { $adg -contains $_.ProcessId }).Count
    Write-Host "  traccia $(Split-Path $Etl -Leaf): $($ev.Count) eventi, $inAdg da audiodg (pid $($adg -join ','))"
    $ev.Count
}

function Invoke-RefCapture([string[]]$RcArgs, [string]$Dir, [bool]$NeedLoopback) {
    # Uscita di ref-capture diretta sulla console (niente pipe: "CAMBIA PROFILO ADESSO" deve
    # comparire subito); i dettagli restano in info.txt.
    & $rc @RcArgs
    if ($LASTEXITCODE -ne 0) { throw "ref-capture fallito ($LASTEXITCODE)" }
    if (-not (Test-Path (Join-Path $Dir "info.txt"))) { throw "ref-capture non ha scritto info.txt (Smart App Control?)" }
    if ($NeedLoopback -and -not (Test-Path (Join-Path $Dir "loopback.wav"))) { throw "manca loopback.wav in $Dir" }
}

$canali71 = Join-Path $sig "canali71.wav"
$cambio71 = Join-Path $sig "cambio71.wav"

switch ($Step) {
    "controllo" {
        "== W6, prova di controllo: traccia del modulo con canali71 senza loopback"
        $dir = Join-Path $out "controllo"
        $etl = Start-Trace "controllo"
        Invoke-RefCapture @("--no-loopback", "--endpoint", $Endpoint, $canali71, $dir) $dir $false
        $n = Stop-Trace $etl
        if ($n -eq 0) {
            "  NESSUN evento dal provider $Provider. Provider attivi in audiodg:"
            $p = (Get-Process audiodg).Id
            logman query providers -pid $p | Tee-Object (Join-Path $out "provider-audiodg.txt")
        } else {
            "  prime righe:"
            Import-Csv ($etl -replace '\.etl$', '.csv') | Select-Object -First 15 | Format-Table -AutoSize | Out-String -Width 250
        }
    }
    "sessioni" {
        Wait-Profile "film-balanced" "Film Equilibrato"
        "== W6: sessioni senza loopback"
        foreach ($p in $Pauses) {
            $dir = Join-Path $out "sessioni-noloop-p$p"
            "-- pausa $p ms"
            $etl = Start-Trace "sessioni-p$p"
            Invoke-RefCapture @("--no-loopback", "--sessions", "2", "--pause-ms", "$p", "--endpoint", $Endpoint, $canali71, $dir) $dir $false
            $null = Stop-Trace $etl
        }
        "== W7: sessioni con loopback"
        foreach ($p in $Pauses) {
            $dir = Join-Path $out "sessioni-film-balanced-p$p"
            "-- pausa $p ms"
            $etl = Start-Trace "sessioni-loop-p$p"
            Invoke-RefCapture @("--sessions", "2", "--pause-ms", "$p", "--endpoint", $Endpoint, $canali71, $dir) $dir $true
            $null = Stop-Trace $etl
        }
        Wait-Profile "film-balanced" "Film Equilibrato"
    }
    "cambio" {
        if (-not (Test-Path $cambio71)) { throw "manca $cambio71 (vedi W8 nel doc)" }
        $cases = @(
            @{ dir = "cambio-film-balanced-a-custom1-virt-off"; from = "film-balanced"; fromL = "Film Equilibrato"; to = "custom1-virt-off"; toL = "Personalizzato 1, virtualizzatore spento"; etw = "cambio-film-a-custom1" },
            @{ dir = "cambio-gaming-performance-a-film-balanced"; from = "gaming-performance"; fromL = "Gaming, modalita' performance"; to = "film-balanced"; toL = "Film Equilibrato"; etw = "cambio-gaming-a-film" },
            @{ dir = "cambio-film-balanced-a-film-warm"; from = "film-balanced"; fromL = "Film Equilibrato"; to = "film-warm"; toL = "Film Caldo"; etw = "cambio-film-a-warm" }
        )
        foreach ($c in $cases) {
            "== W8: $($c.fromL) -> $($c.toL)"
            Wait-Profile $c.from $c.fromL
            Read-Host "Al messaggio CAMBIA PROFILO ADESSO (secondo 12) seleziona '$($c.toL)' e poi non toccare altro. Invio per partire"
            $dir = Join-Path $out $c.dir
            $etl = Start-Trace $c.etw
            Invoke-RefCapture @("--watch-profile", "--prompt-at", "12", "--endpoint", $Endpoint, $cambio71, $dir) $dir $true
            $null = Stop-Trace $etl
            $last = Get-ChildItem $dir -Directory -Filter "cambio-*" | Sort-Object { [int]($_.Name -replace '\D', '') } | Select-Object -Last 1
            if (-not $last) { "  ATTENZIONE: nessuna notifica del registro durante la riproduzione"; continue }
            $bad = 0
            foreach ($f in Get-ChildItem (Join-Path $ref "$($c.to)\canali71") -Filter *.bin) {
                fc.exe /b $f.FullName (Join-Path $last.FullName $f.Name) | Out-Null
                if ($LASTEXITCODE -ne 0) { $bad++; "  fc /b diverso: $($f.Name)" }
            }
            "  blob d'arrivo ($($last.Name)) contro target\verify\$($c.to)\canali71: " + $(if ($bad -eq 0) { "identici" } else { "$bad diversi" })
        }
    }
}
Stop-Transcript | Out-Null
