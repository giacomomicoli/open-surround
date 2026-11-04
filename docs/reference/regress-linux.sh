#!/usr/bin/env bash
# regress-linux.sh — verifica della fase 3 su Linux: riesegue con os-dap-linux (loader PE
# nativo) tutte le catture salvate in target/verify (prodotte su Windows da verify-profile.ps1
# con ref-capture) e confronta l'uscita con la loopback.wav di Windows tramite wavcmp.
# Equivalente di regress-host.ps1 (doc sez. 12.10) con il backend Linux invece di quello Windows.
#
# Uso:
#   docs/reference/regress-linux.sh [DLL] [filtro-profilo]
#     DLL            DolbyAudioProcessing.dll (default ~/Documenti/dolby-bins/…)
#     filtro-profilo elabora solo le cartelle il cui nome contiene la stringa
#   QUANTUM=<n> docs/reference/regress-linux.sh …
#     fase 4: fa passare ogni caso dall'adattatore del tempo reale a cicli di n frame
#     (--quantum, come il nodo PipeWire). Le uscite, tolta la latenza dell'adattatore,
#     devono restare identiche byte per byte alla baseline (doc sez. 12.13).

set -u
cd "$(dirname "$0")/../.."

DLL="${1:-$HOME/Documenti/dolby-bins/DolbyAudioProcessing.dll}"
FILTER="${2:-}"
QUANTUM="${QUANTUM:-}"
VERIFY=target/verify
OUT=target/regress-linux
HOST=target/debug/os-dap-linux
WAVCMP=target/debug/wavcmp

mkdir -p "$OUT"

worst_overall=""
pass=0
fail=0

worstdb() {
    # minimo (più profondo) residuo è migliore: restituisce il più alto tra i due
    a="$1"; b="$2"
    if [ -z "$a" ]; then echo "$b"; return; fi
    if [ -z "$b" ]; then echo "$a"; return; fi
    if awk -v a="$a" -v b="$b" 'BEGIN{exit (a+0 > b+0) ? 0 : 1}'; then echo "$a"; else echo "$b"; fi
}

for dir in "$VERIFY"/*/ "$VERIFY"/*/*/; do
    [ -f "$dir/info.txt" ] && [ -f "$dir/loopback.wav" ] || continue
    # a livello profilo (senza segnale) non c'è loopback; le catture con loopback sono
    # le cartelle di segnale e la cattura singola stereo-wasapi-gaming.
    # "x" è una cattura di prova senza profilo noto: si salta.
    case "$dir" in
        "$VERIFY/signals/") continue ;;
        "$VERIFY/x/") continue ;;
    esac

    # filtro opzionale
    name=$(basename "$(dirname "$dir")")/$(basename "$dir")
    if [ -n "$FILTER" ] && ! grep -q "$FILTER" <<<"$name"; then continue; fi

    info=$(cat "$dir/info.txt")
    volume=$(grep -oP 'volume \K-?[0-9.]+(?= dB)' <<<"$info" | head -1)
    # Ingresso: il nome del file di info.txt (percorso di Windows o relativo), cercato fra i
    # segnali di prova e poi nella cartella della cattura (lì vanno i brani di audio reale).
    ingresso=$(grep -oP '^ingresso: \K.+?(?= \([0-9]+ canali)' <<<"$info" | head -1)
    ingresso="${ingresso##*[/\\]}"
    input=""
    for cand in "$VERIFY/signals/$ingresso" "$dir/$ingresso"; do
        [ -n "$ingresso" ] && [ -f "$cand" ] && { input="$cand"; break; }
    done
    if [ -z "$volume" ] || [ -z "$input" ]; then
        echo "SALTA $name (volume o ingresso assenti)"
        continue
    fi

    extra=()
    if [ -n "$QUANTUM" ]; then
        extra+=(--quantum "$QUANTUM")
    fi
    if grep -q "stream WASAPI" <<<"$info" || [[ "$dir" == *stereo-app* || "$dir" == *stereo-wasapi* ]]; then
        extra+=(--stereo-bypass)
    fi

    tag="$name"
    out="$OUT/$tag"
    mkdir -p "$out"
    outwav="$OUT/$tag/linux.wav"

    if ! "$HOST" --capture "$dir" --dll "$DLL" "${extra[@]}" --volume-db "$volume" "$input" "$outwav" > "$out/host.log" 2>&1; then
        echo "ERRORE $name: os-dap-linux è fallito (vedi $out/host.log)"
        tail -5 "$out/host.log" | sed 's/^/    /'
        fail=$((fail + 1))
        continue
    fi
    "$WAVCMP" --segments 3 "$dir/loopback.wav" "$outwav" > "$out/wavcmp.txt" 2>&1
    # Criterio: il residuo per canale con guadagno stimato (righe "canale N: …"), non i
    # segmenti: la coda di silenzio del segnale ha un riferimento a −107 dBFS e un residuo
    # relativo privo di senso (lo stesso −97 dB che mostra l'host Windows).
    gain=$(grep -oP 'guadagno stimato \K[-+]?[0-9.]+(?= dB)' "$out/wavcmp.txt" | tail -1)
    worst=""
    while read -r r; do worst=$(worstdb "$worst" "$r"); done < <(grep -E '^canale' "$out/wavcmp.txt" | grep -oP 'guadagno stimato [-+]?[0-9.]+ dB, residuo \K-?[0-9.]+(?= dB rel\.)')
    worst_overall=$(worstdb "$worst_overall" "$worst")
    resid="$worst"

    # criterio della fase 3 (doc 12.5): residuo <= -130 dB (arrotondamento float) su ogni caso
    if awk -v w="${worst:-0}" 'BEGIN{exit (w+0 <= -130) ? 0 : 1}'; then
        echo "OK    $name: residuo $worst dB (guadagno $gain)"
        pass=$((pass + 1))
    else
        echo "FAIL  $name: residuo $worst dB (guadagno $gain) — vedi $out/"
        fail=$((fail + 1))
    fi
done

echo
echo "passati: $pass   falliti: $fail   peggiore: ${worst_overall:-?} dB"
[ "$fail" -eq 0 ] && [ "$pass" -gt 0 ]
