# Verifica che os-dap-host suoni identico a Windows per il profilo Dolby Access attivo (doc sez. 12.9).
# Uso:  powershell -File docs\reference\verify-profile.ps1 -Name movie-balanced [-Endpoint 32ed16b6]
# Prima selezionare il profilo in Dolby Access sull'endpoint scelto (deve avere Dolby Atmos attivo e,
# meglio, non essere collegato: i segnali di prova vengono riprodotti davvero).
# Per ogni segnale di prova: ref-capture (Windows: ISpatialAudioClient + loopback + blob Dolby del
# momento), os-dap-host con gli stessi blob e il guadagno che Windows applica prima di Dolby
# (-3 dB prima, volume dell'endpoint dopo), wavcmp. Risultati in target\verify\<Name>\, riepilogo a schermo.
param(
    [Parameter(Mandatory = $true)][string]$Name,
    [string]$Endpoint = "32ed16b6"
)
# Le eccezioni vengono dagli exit code: con "Stop" PowerShell 5.1 tratta lo stderr dei programmi come errore.
$ErrorActionPreference = "Continue"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$bin = Join-Path $root "target\debug"
$sig = Join-Path $root "target\verify\signals"
$out = Join-Path $root "target\verify\$Name"
New-Item -ItemType Directory -Force $sig, $out | Out-Null

# Segnali di prova (generati una volta, 48 kHz float, 1 s di silenzio iniziale per evitare il
# transitorio di avvio del modulo).
$signals = [ordered]@{
    # un canale 7.1 alla volta, rumore rosa a -10 dBFS
    "canali71" = "-f lavfi -i anoisesrc=c=pink:r=48000:a=0.3:d=8:seed=1 -f lavfi -i aevalsrc='between(t,0,0.8)|between(t,1,1.8)|between(t,2,2.8)|between(t,3,3.8)|between(t,4,4.8)|between(t,5,5.8)|between(t,6,6.8)|between(t,7,7.8)':s=48000:d=8:c=7.1 -filter_complex [0]aformat=channel_layouts=mono,asplit=8[a0][a1][a2][a3][a4][a5][a6][a7];[a0][a1][a2][a3][a4][a5][a6][a7]join=inputs=8:channel_layout=7.1[n];[n][1]amultiply,adelay=1000:all=1"
    # tutti i canali 7.1 insieme, rumori indipendenti, gradini di livello -40/-20/-6/0 dBFS:
    # fa lavorare leveler, maximizer (volmax) e limiter
    "dinamica71" = "-f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=11 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=12 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=13 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=14 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=15 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=16 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=17 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=8:seed=18 -filter_complex [0][1][2][3][4][5][6][7]join=inputs=8:channel_layout=7.1,volume='if(lt(t,2),0.01,if(lt(t,4),0.1,if(lt(t,6),0.5,1)))':eval=frame,alimiter=limit=1:level=0,adelay=1000:all=1"
    # stereo con L e R indipendenti, -12 dBFS poi -3 dBFS
    "stereo" = "-f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=6:seed=21 -f lavfi -i anoisesrc=c=pink:r=48000:a=1:d=6:seed=22 -filter_complex [0][1]join=inputs=2:channel_layout=stereo,volume='if(lt(t,3),0.25,0.7)':eval=frame,adelay=1000:all=1"
}
foreach ($s in $signals.Keys) {
    $wav = Join-Path $sig "$s.wav"
    if (-not (Test-Path $wav)) {
        $fa = $signals[$s] -split " "
        & ffmpeg -hide_banner -loglevel error -y @fa -c:a pcm_f32le $wav
        if ($LASTEXITCODE -ne 0) { throw "ffmpeg fallito per $s" }
    }
}

# Prove: segnale, opzioni di ref-capture, opzioni di os-dap-host. "stereo-app" riproduce lo stereo
# come un'applicazione normale (stream WASAPI): Windows lo passa agli oggetti bypass 17-18 senza -3 dB.
$tests = @(
    @{ name = "canali71"; signal = "canali71"; ref = @(); host = @() },
    @{ name = "dinamica71"; signal = "dinamica71"; ref = @(); host = @() },
    @{ name = "stereo"; signal = "stereo"; ref = @(); host = @() },
    @{ name = "stereo-app"; signal = "stereo"; ref = @("--wasapi"); host = @("--stereo-bypass") }
)
$summary = @()
foreach ($t in $tests) {
    $s = $t.name
    $wav = Join-Path $sig "$($t.signal).wav"
    $ref = Join-Path $out $s
    $refArgs = $t.ref
    & "$bin\ref-capture.exe" @refArgs --endpoint $Endpoint $wav $ref 2>&1 | Out-Null
    # Se Smart App Control blocca l'eseguibile, $LASTEXITCODE resta quello precedente: controllo anche il file.
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $ref "loopback.wav"))) { throw "ref-capture fallito per $s" }
    $info = Get-Content (Join-Path $ref "info.txt") -Raw
    if ($info -notmatch 'volume (-?[0-9.]+) dB') { throw "volume non trovato in info.txt" }
    $volume = [double]$Matches[1]
    $host_wav = Join-Path $out "$s-host.wav"
    $hostArgs = $t.host
    & "$bin\os-dap-host.exe" @hostArgs --volume-db $volume.ToString([Globalization.CultureInfo]::InvariantCulture) --capture $ref --endpoint $Endpoint $wav $host_wav 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $host_wav)) { throw "os-dap-host fallito per $s (bloccato da Smart App Control? ricompilare)" }
    $cmp = & "$bin\wavcmp.exe" --segments 1 (Join-Path $ref "loopback.wav") $host_wav
    $cmp | Set-Content (Join-Path $out "$s-wavcmp.txt")
    foreach ($line in $cmp | Select-String '^canale') {
        if ($line -match 'canale (\d): .*residuo g=1 ([-0-9.]+) dB.*guadagno stimato ([-+0-9.]+) dB, residuo ([-0-9.]+) dB') {
            $summary += [pscustomobject]@{
                segnale = $s; canale = $Matches[1]; "residuo g=1 (dB)" = $Matches[2]
                "guadagno stimato (dB)" = $Matches[3]; "residuo (dB)" = $Matches[4]
            }
        }
    }
}
"profilo: $Name  endpoint: $Endpoint  catena: -3 dB (0 dB per stereo-app), Dolby, volume endpoint $volume dB"
$summary | Format-Table -AutoSize
# Blob runtime cuffie del momento, per riconoscere il profilo
Get-FileHash (Join-Path $out "canali71\$Endpoint-2.bin") -Algorithm SHA256 |
    ForEach-Object { "blob ,2 sha256: " + $_.Hash.Substring(0, 12) }
