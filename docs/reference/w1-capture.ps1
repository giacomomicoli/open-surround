# Fase 5, passo W1 (doc 12.14): una cattura mirata in un colpo solo.
# Uso:  powershell -File docs\reference\w1-capture.ps1 -Name w1-geq-a
# 1. capture_asar.ps1 -Name <nome> (blob del contesto Dolby di ogni endpoint);
# 2. settings.json di Dolby Access ridotto alle sole sezioni dei profili (Common.DapProfile e
#    UserSettingsService._profileParameters): niente licenze, prezzi o identificativi;
# 3. os-blob import sull'endpoint delle cuffie, salvato anche in import.txt nella cartella.
param(
    [Parameter(Mandatory = $true)][string]$Name,
    [string]$Endpoint = "32ed16b6"
)
$ErrorActionPreference = "Stop"
$repo = Split-Path (Split-Path $PSScriptRoot)
$out = Join-Path $PSScriptRoot "captures\$Name"

& powershell -NoProfile -File (Join-Path $PSScriptRoot "capture_asar.ps1") -Name $Name |
    Where-Object { $_ -match $Endpoint -or $_ -match 'Selected profile|^---' }

# Dolby Access scrive settings.json solo quando si chiude (o va in sospensione), non a ogni cambio:
# la copia ha senso solo se il file e' piu' recente dell'ultima scrittura nel registro (log).
$src = "$env:LOCALAPPDATA\Packages\DolbyLaboratories.DolbyAccess_rz1tebttyb220\LocalCache\settings.json"
$log = Get-ChildItem (Join-Path (Split-Path $src) "DolbyAccess_*.log") | Sort-Object LastWriteTime | Select-Object -Last 1
if ((Get-Item $src).LastWriteTime -lt $log.LastWriteTime.AddSeconds(-30)) {
    "settings.json non aggiornato (scritto $((Get-Item $src).LastWriteTime)): non copiato; chiudere l'app per salvarlo"
} else {
    $s = Get-Content $src -Raw -Encoding UTF8 | ConvertFrom-Json
    $ridotto = [ordered]@{
        DapProfile         = $s.Common.DapProfile
        _profileParameters = $s.UserSettingsService._profileParameters
    }
    $json = $ridotto | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText((Join-Path $out "settings.json"), $json + "`n", (New-Object System.Text.UTF8Encoding $false))
    $tipo = $s.Common.DapProfile.Type
    "settings.json copiato; profilo attivo nell'app: $tipo"
    $p = $s.UserSettingsService._profileParameters.$tipo
    if ($p -and $p.CustomEqualizerSettings) {
        "  _user10Gains: " + ($p.CustomEqualizerSettings._user10Gains -join ' ')
        "  _dap20Gains:  " + ($p.CustomEqualizerSettings._dap20Gains -join ' ')
    }
}

$ErrorActionPreference = "Continue"   # un import fallito va salvato, non interrompe
$imp = & (Join-Path $repo "target\debug\os-blob.exe") import $out $Endpoint 2>&1 | ForEach-Object { "$_" }
[System.IO.File]::WriteAllLines((Join-Path $out "import.txt"), [string[]]$imp)
"os-blob import:"
$imp | ForEach-Object { "  $_" }
