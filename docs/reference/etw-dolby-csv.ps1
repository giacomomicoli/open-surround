# Estrae il log del modulo Dolby da una traccia ETW convertita in XML da tracerpt (doc sez. 12.13, W6).
# Il modulo scrive con EventWriteString, senza manifest: Get-WinEvent non formatta il messaggio,
# tracerpt invece lo mette in <Data> direttamente sotto <Event>. Scrive un CSV (ora locale, pid, tid,
# messaggio) accanto all'XML. Uso:
#   powershell -ExecutionPolicy Bypass -File docs\reference\etw-dolby-csv.ps1 target\verify-fase4\etw-x.xml
param(
    [Parameter(Mandatory = $true)][string]$Xml,
    [string]$Provider = "{ebfc27e1-9503-4ad5-bf27-8c6bc695c59a}"
)
$doc = New-Object System.Xml.XmlDocument
$doc.Load((Resolve-Path $Xml).Path)
$rows = foreach ($e in $doc.DocumentElement.ChildNodes) {
    if ($e.LocalName -ne "Event") { continue }
    $sys = $e.Item("System")
    if ($sys.Item("Provider").GetAttribute("Guid") -ne $Provider) { continue }
    $t = $sys.Item("TimeCreated").GetAttribute("SystemTime")   # 2026-11-05T21:01:45.385785100+00:59, ora locale
    $ex = $sys.Item("Execution")
    $data = $e.Item("Data")
    [pscustomobject]@{
        ora       = $t.Substring(11, 12)
        pid       = $ex.GetAttribute("ProcessID")
        tid       = $ex.GetAttribute("ThreadID")
        messaggio = if ($data) { $data.InnerText } else { "" }
    }
}
$csv = $Xml -replace '\.xml$', '.csv'
$rows | Export-Csv $csv -NoTypeInformation -Encoding UTF8
"$csv : $(@($rows).Count) righe del modulo"
