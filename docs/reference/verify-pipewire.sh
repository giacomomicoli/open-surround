#!/usr/bin/env bash
# verify-pipewire.sh — verifica della fase 4 (doc sez. 12.13): per ogni cattura salvata in
# target/verify avvia il nodo os-pipewire con gli stessi blob e lo stesso volume dell'endpoint,
# riproduce il segnale di prova nel sink giusto con pw-play (giochi: sink 7.1; stereo delle
# applicazioni: sink stereo), registra l'uscita del nodo dal monitor di un null sink con
# pw-record e la confronta con la loopback.wav di Windows tramite wavcmp. Stesso criterio di
# regress-linux.sh: residuo per canale con guadagno stimato <= −130 dB.
#
# Il nodo usa nomi propri (os-verify-*) e suona solo sul null sink os-verify-null: le uscite
# audio reali e i sink di open-surround in uso non vengono toccati.
#
# Uso:
#   docs/reference/verify-pipewire.sh [DLL] [filtro-profilo]
#     DLL            DolbyAudioProcessing.dll (default ~/Documenti/dolby-bins/…)
#     filtro-profilo elabora solo le cartelle il cui nome contiene la stringa
#   LATENCY=<n> …    chiede al grafo un quantum di n frame (node.latency, es. 480 o 256)

set -u
cd "$(dirname "$0")/../.."

DLL="${1:-$HOME/Documenti/dolby-bins/DolbyAudioProcessing.dll}"
FILTER="${2:-}"
LATENCY="${LATENCY:-}"
VERIFY=target/verify
OUT=target/verify-pipewire
NODE=target/debug/os-pipewire
WAVCMP=target/debug/wavcmp
PREFIX=os-verify
NULL=os-verify-null

for tool in pw-play pw-record pw-cli pw-dump wpctl ffmpeg ffprobe python3; do
    command -v "$tool" > /dev/null || { echo "manca $tool"; exit 1; }
done
[ -x "$NODE" ] && [ -x "$WAVCMP" ] || { echo "prima: cargo build"; exit 1; }
mkdir -p "$OUT/signals"

# id del nodo PipeWire con node.name = $1 (vuoto se non c'è)
node_id() {
    pw-dump 2>/dev/null | python3 -c '
import json, sys
for o in json.load(sys.stdin):
    if o.get("type", "").endswith(":Node") and o["info"]["props"].get("node.name") == sys.argv[1]:
        print(o["id"]); break' "$1"
}

wait_node() {
    for _ in $(seq 50); do
        id=$(node_id "$1")
        [ -n "$id" ] && { echo "$id"; return 0; }
        sleep 0.2
    done
    return 1
}

# SIGINT al processo $1 e attesa, con SIGKILL dopo 10 s.
stop() {
    kill -INT "$1" 2> /dev/null || return 0
    for _ in $(seq 100); do
        kill -0 "$1" 2> /dev/null || { wait "$1" 2> /dev/null; return 0; }
        sleep 0.1
    done
    echo "  $1 non si ferma con SIGINT: SIGKILL"
    kill -KILL "$1" 2> /dev/null; wait "$1" 2> /dev/null
}

node_pid=""
cleanup() {
    [ -n "$node_pid" ] && stop "$node_pid"
    id=$(node_id "$NULL")
    [ -n "$id" ] && pw-cli destroy "$id" > /dev/null 2>&1
}
trap cleanup EXIT

if [ -z "$(node_id "$NULL")" ]; then
    pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$NULL \
        node.description=\"open-surround verifica (null)\" media.class=Audio/Sink \
        audio.position=[FL FR] audio.rate=48000 object.linger=true }" > /dev/null
fi
null_id=$(wait_node "$NULL") || { echo "null sink non creato"; exit 1; }
wpctl set-volume "$null_id" 1.0

# Segnali in float32: pw-play non converte formati (stessi valori di os_dap_core::offline::read_wav:
# le conversioni da intero di ffmpeg scalano per potenze di 2, esatte). Con 2 s di silenzio in
# coda: ref-capture tiene aperto il flusso mentre registra la coda del modulo, che quindi vede
# oggetti attivi a zero; pw-play invece chiude a fine file e il sink resterebbe vuoto (oggetti
# inattivi), e da lì la coda di leveler e limiter sarebbe diversa.
# Un 5.1 (i brani di audio reale) diventa un 7.1 con i posteriori a zero: stessi bed di
# layout::channel_map(6) (laterali SL/SR, come Windows), senza dipendere dalla maschera dei canali
# letta da pw-play; i bed in più, attivi a zero, non cambiano l'uscita (verificato, 12.13).
prepare() {
    local src="$1" dst="$2" ch
    [ -f "$dst" ] && return 0
    ch=$(ffprobe -v error -show_entries stream=channels -of csv=p=0 "$src")
    case "$ch" in
        6) ffmpeg -loglevel error -y -i "$src" -af "pan=7.1|FL=c0|FR=c1|FC=c2|LFE=c3|SL=c4|SR=c5,apad=pad_dur=2" -c:a pcm_f32le "$dst" ;;
        2 | 8) ffmpeg -loglevel error -y -i "$src" -af apad=pad_dur=2 -c:a pcm_f32le "$dst" ;;
        *) echo "  $src: $ch canali non gestiti (2, 6, 8)"; return 1 ;;
    esac
}

worstdb() {
    a="$1"; b="$2"
    if [ -z "$a" ]; then echo "$b"; return; fi
    if [ -z "$b" ]; then echo "$a"; return; fi
    if awk -v a="$a" -v b="$b" 'BEGIN{exit (a+0 > b+0) ? 0 : 1}'; then echo "$a"; else echo "$b"; fi
}

pass=0
fail=0
worst_overall=""
for dir in "$VERIFY"/*/ "$VERIFY"/*/*/; do
    [ -f "$dir/info.txt" ] && [ -f "$dir/loopback.wav" ] || continue
    case "$dir" in
        "$VERIFY/signals/" | "$VERIFY/x/") continue ;;
    esac
    name=$(basename "$(dirname "$dir")")/$(basename "$dir")
    if [ -n "$FILTER" ] && ! grep -q "$FILTER" <<<"$name"; then continue; fi

    info=$(cat "$dir/info.txt")
    volume=$(grep -oP 'volume \K-?[0-9.]+(?= dB)' <<<"$info" | head -1)
    # Ingresso come in regress-linux.sh: segnali di prova, poi la cartella della cattura.
    ingresso=$(grep -oP '^ingresso: \K.+?(?= \([0-9]+ canali)' <<<"$info" | head -1)
    ingresso="${ingresso##*[/\\]}"
    input=""
    for cand in "$VERIFY/signals/$ingresso" "$dir/$ingresso"; do
        [ -n "$ingresso" ] && [ -f "$cand" ] && { input="$cand"; break; }
    done
    if [ -n "$input" ]; then
        prepared="$OUT/signals/$(basename "$(dirname "$input")")-$ingresso"
        [ "$(dirname "$input")" = "$VERIFY/signals" ] && prepared="$OUT/signals/$ingresso"
        prepare "$input" "$prepared" || input=""
        input="${input:+$prepared}"
    fi
    if [ -z "$volume" ] || [ -z "$input" ]; then
        echo "SALTA $name (volume o ingresso assenti)"
        continue
    fi
    sink="$PREFIX-7.1"
    if grep -q "stream WASAPI" <<<"$info" || [[ "$dir" == *stereo-app* || "$dir" == *stereo-wasapi* ]]; then
        sink="$PREFIX-stereo"
    fi

    out="$OUT/$name"
    mkdir -p "$out"
    rm -f "$out/node.wav"
    extra=()
    [ -n "$LATENCY" ] && extra+=(--latency "$LATENCY")
    "$NODE" --capture "$dir" --dll "$DLL" --name "$PREFIX" --target "$NULL" --volume-db "$volume" \
        "${extra[@]}" -v > "$out/node.log" 2>&1 &
    node_pid=$!
    ok=1
    for n in "$PREFIX-7.1" "$PREFIX-stereo" "$PREFIX-output"; do
        id=$(wait_node "$n") || { ok=0; break; }
        # Volume dei sink al 100%: resta solo --volume-db dopo il modulo, come la catena di Windows.
        [ "$n" != "$PREFIX-output" ] && wpctl set-volume "$id" 1.0
    done
    if [ "$ok" -ne 1 ]; then
        echo "ERRORE $name: il nodo non è partito (vedi $out/node.log)"
        tail -5 "$out/node.log" | sed 's/^/    /'
        fail=$((fail + 1))
        stop "$node_pid"; node_pid=""
        continue
    fi
    # Il registratore tiene in moto il null sink, e quindi il nodo: il modulo elabora silenzio
    # prima del segnale, come l'offline (che parte con il secondo di silenzio del segnale).
    pw-record --target "$NULL" -P '{ stream.capture.sink = true }' --format f32 --rate 48000 \
        --channels 2 "$out/node.wav" > "$out/record.log" 2>&1 &
    rec_pid=$!
    sleep 1
    # Upmix disattivato: uno stereo nel sink 7.1 deve restare FL/FR (bed, come un gioco su Windows).
    pw-play --target "$sink" -P '{ channelmix.upmix = false }' --volume 1.0 "$input" > "$out/play.log" 2>&1
    sleep 1
    stop "$rec_pid"
    stop "$node_pid"; node_pid=""

    "$WAVCMP" --onset --segments 3 "$dir/loopback.wav" "$out/node.wav" > "$out/wavcmp.txt" 2>&1
    gain=$(grep -oP 'guadagno stimato \K[-+]?[0-9.]+(?= dB)' "$out/wavcmp.txt" | tail -1)
    worst=""
    while read -r r; do worst=$(worstdb "$worst" "$r"); done < <(grep -E '^canale' "$out/wavcmp.txt" | grep -oP 'guadagno stimato [-+]?[0-9.]+ dB, residuo \K-?[0-9.]+(?= dB rel\.)')
    worst_overall=$(worstdb "$worst_overall" "$worst")
    lat=$(grep -oP 'latenza dell.adattatore: \K[0-9]+' "$out/node.log" | tail -1)
    stats=$(grep '^\[nodo\]' "$out/node.log" | tail -1 | grep -oP 'errori \K[0-9]+')
    if awk -v w="${worst:-0}" 'BEGIN{exit (w+0 <= -130) ? 0 : 1}' && [ "${stats:-1}" = "0" ]; then
        echo "OK    $name ($sink): residuo $worst dB (guadagno $gain, latenza adattatore ${lat:-?} frame)"
        pass=$((pass + 1))
    else
        echo "FAIL  $name ($sink): residuo ${worst:-?} dB (guadagno ${gain:-?}, errori ${stats:-?}) — vedi $out/"
        fail=$((fail + 1))
    fi
done

echo
echo "passati: $pass   falliti: $fail   peggiore: ${worst_overall:-?} dB"
[ "$fail" -eq 0 ] && [ "$pass" -gt 0 ]
