#!/usr/bin/env bash
# verify-sessioni-cambi.sh — verifica sul nodo vero dei passi L4 e L5 della fase 4 (doc sez.
# 12.13): sessioni separate da una pausa e cambio di profilo a caldo, attraverso PipeWire.
#
#   sessioni  Film Equilibrato, due riproduzioni di canali71 separate da una pausa (500 ms e
#             3 s): a fine flusso il nodo riconfigura il modulo, come Windows senza loopback
#             (W6), quindi ognuna deve coincidere con il riferimento da modulo nuovo,
#             target/verify/film-balanced/canali71/loopback.wav (residuo <= −130 dB su entrambe)
#   cambi     i tre casi di W8 (target/verify-fase4/cambio-*): il nodo parte con i blob di
#             partenza, riceve dal FIFO di controllo i cambi al frame trovato offline e la sua
#             uscita si confronta con la loopback.wav di Windows (intera registrazione) e con il
#             render offline con gli stessi cambi (identico byte per byte, a meno del ritardo)
#
# Il nodo usa nomi propri (os-verify-*) e suona solo sul null sink os-verify-null: le uscite
# audio reali non vengono toccate. Serve target/verify-fase4 (verify-fase4.tar.gz, W9).
#
# Uso:
#   docs/reference/verify-sessioni-cambi.sh [sessioni|cambi|tutto] [DLL]
#   PROFILE=1 …  fase 5 (doc 12.14, L3): il nodo parte con --profile e riceve i cambi come
#                `profilo <stato>` nel FIFO, con gli stati ricavati dalle catture da os-blob
#                import; il render offline di confronto resta quello dalle catture, quindi
#                "bit-identico" vale anche come verifica dei blob generati. Uscite in
#                target/verify-sessioni-cambi-profilo
#   SOCKET=1 …   fase 6 (doc 12.15, L2): i cambi passano dal socket di controllo con os-ctl,
#                il canale dell'interfaccia, invece che dal FIFO; ogni cambio deve avere la
#                risposta `ok`. Uscite in …-socket (con PROFILE=1: …-profilo-socket)

set -u
cd "$(dirname "$0")/../.."

WHAT="${1:-tutto}"
DLL="${2:-$HOME/.local/share/open-surround/DolbyAudioProcessing.dll}"
VERIFY=target/verify
FASE4=target/verify-fase4
OUT=target/verify-sessioni-cambi
PROFILE="${PROFILE:-}"
[ -n "$PROFILE" ] && OUT=target/verify-sessioni-cambi-profilo
SOCKET="${SOCKET:-}"
[ -n "$SOCKET" ] && OUT=$OUT-socket
OSBLOB=target/debug/os-blob
OSCTL=target/debug/os-ctl
NODE=target/debug/os-pipewire
OFFLINE=target/debug/os-dap-linux
WAVCMP=target/debug/wavcmp
PREFIX=os-verify
NULL=os-verify-null
REC="${XDG_RUNTIME_DIR:-/dev/shm}/os-verify-node.wav"
FIFO="${XDG_RUNTIME_DIR:-/dev/shm}/os-verify-control"
SOCK="${XDG_RUNTIME_DIR:-/dev/shm}/os-verify-control.sock"

for tool in pw-play pw-record pw-cli pw-dump wpctl ffmpeg python3; do
    command -v "$tool" > /dev/null || { echo "manca $tool"; exit 1; }
done
[ -x "$NODE" ] && [ -x "$OFFLINE" ] && [ -x "$WAVCMP" ] || { echo "prima: cargo build"; exit 1; }
mkdir -p "$OUT"

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

stop() {
    kill -INT "$1" 2> /dev/null || return 0
    for _ in $(seq 100); do
        kill -0 "$1" 2> /dev/null || { wait "$1" 2> /dev/null; return 0; }
        sleep 0.1
    done
    kill -KILL "$1" 2> /dev/null; wait "$1" 2> /dev/null
}

node_pid=""
rec_pid=""
cleanup() {
    [ -n "$rec_pid" ] && stop "$rec_pid"
    [ -n "$node_pid" ] && stop "$node_pid"
    id=$(node_id "$NULL")
    [ -n "$id" ] && pw-cli destroy "$id" > /dev/null 2>&1
    rm -f "$REC" "$FIFO" "$SOCK"
}
trap cleanup EXIT

if [ -z "$(node_id "$NULL")" ]; then
    pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$NULL \
        node.description=\"open-surround verifica (null)\" media.class=Audio/Sink \
        audio.position=[FL FR] audio.rate=48000 object.linger=true }" > /dev/null
fi
null_id=$(wait_node "$NULL") || { echo "null sink non creato"; exit 1; }
wpctl set-volume "$null_id" 1.0

# Segnale in float32 con 2 s di silenzio in coda (come verify-pipewire.sh).
prepare() {
    [ -f "$2" ] || ffmpeg -loglevel error -y -i "$1" -af apad=pad_dur=2 -c:a pcm_f32le "$2"
}

# Avvia il nodo ($1 = cartella, $2 = log, altri argomenti al nodo) e il registratore.
start() {
    local capture="$1" log="$2"
    shift 2
    rm -f "$REC"
    local blobs=(--capture "$capture")
    [ -n "$PROFILE" ] && blobs=(--profile "$("$OSBLOB" import "$capture")")
    "$NODE" "${blobs[@]}" --dll "$DLL" --name "$PREFIX" --target "$NULL" --volume-db -9.3428 \
        -v "$@" > "$log" 2>&1 &
    node_pid=$!
    for n in "$PREFIX-7.1" "$PREFIX-stereo" "$PREFIX-output"; do
        id=$(wait_node "$n") || { echo "il nodo non è partito (vedi $log)"; return 1; }
        [ "$n" != "$PREFIX-output" ] && wpctl set-volume "$id" 1.0
    done
    pw-record --target "$NULL" -P '{ stream.capture.sink = true }' --format f32 --rate 48000 \
        --channels 2 "$REC" > /dev/null 2>&1 &
    rec_pid=$!
    sleep 1
}

finish() {
    sleep 1
    stop "$rec_pid"; rec_pid=""
    stop "$node_pid"; node_pid=""
    mv "$REC" "$1"
}

play() {
    pw-play --target "$PREFIX-7.1" -P '{ channelmix.upmix = false }' --volume 1.0 "$1" > /dev/null 2>&1
}

# Residuo peggiore (dB) delle righe "canale N:" di un'uscita di wavcmp.
worst() {
    grep -E '^canale' "$1" | grep -oP 'guadagno stimato [-+]?[0-9.]+ dB, residuo \K-?[0-9.]+(?= dB rel\.)' |
        sort -g | tail -1
}

errors() {
    grep '^\[nodo\]' "$1" | tail -1 | grep -oP 'errori \K[0-9]+'
}

pass=0
fail=0
verdict() { # nome, residuo, errori, nota
    if [ -n "$2" ] && awk -v w="$2" 'BEGIN{exit (w+0 <= -130) ? 0 : 1}' && [ "${3:-1}" = "0" ]; then
        echo "OK    $1: residuo $2 dB$4"
        pass=$((pass + 1))
    else
        echo "FAIL  $1: residuo ${2:-?} dB, errori ${3:-?}$4"
        fail=$((fail + 1))
    fi
}

if [ "$WHAT" = sessioni ] || [ "$WHAT" = tutto ]; then
    ref=$VERIFY/film-balanced/canali71
    sig=$OUT/canali71.wav
    prepare "$VERIFY/signals/canali71.wav" "$sig"
    for pause in 500 3000; do
        out=$OUT/sessioni-p$pause
        mkdir -p "$out"
        start "$ref" "$out/node.log" || { fail=$((fail + 1)); continue; }
        play "$sig"
        sleep "$(awk -v p="$pause" 'BEGIN{print p/1000}')"
        play "$sig"
        finish "$out/node.wav"
        # Le due sessioni: si taglia a metà della pausa più lunga (zeri esatti: modulo fermo).
        python3 - "$out/node.wav" "$out" << 'EOF'
import sys, numpy as np
d = open(sys.argv[1], 'rb').read(); i = 12
while i < len(d):
    cid = d[i:i+4]; sz = int.from_bytes(d[i+4:i+8], 'little')
    if cid == b'data': x = np.frombuffer(d[i+8:i+8+sz], dtype='<f4').reshape(-1, 2); break
    i += 8 + sz + (sz & 1)
nz = np.nonzero(np.any(x != 0, axis=1))[0]
gaps = np.diff(nz)
k = int(np.argmax(gaps))
cut = int(nz[k] + gaps[k] // 2)
for name, part in (('sessione1', x[:cut]), ('sessione2', x[cut:])):
    # WAV float32 (formato 3) scritto a mano: il modulo wave non scrive float.
    b = part.astype('<f4').tobytes()
    fmt = (3).to_bytes(2,'little') + (2).to_bytes(2,'little') + (48000).to_bytes(4,'little') + (48000*8).to_bytes(4,'little') + (8).to_bytes(2,'little') + (32).to_bytes(2,'little')
    with open(f'{sys.argv[2]}/{name}.wav', 'wb') as f:
        f.write(b'RIFF' + (4 + 8 + len(fmt) + 8 + len(b)).to_bytes(4,'little') + b'WAVE')
        f.write(b'fmt ' + len(fmt).to_bytes(4,'little') + fmt + b'data' + len(b).to_bytes(4,'little') + b)
print(f'pausa fra le sessioni: {gaps[k]/48000:.2f} s di zeri, taglio al frame {cut}')
EOF
        e=$(errors "$out/node.log")
        resets=$(grep -c 'fine della sessione' "$out/node.log")
        for k in 1 2; do
            "$WAVCMP" --onset --segments 3 "$ref/loopback.wav" "$out/sessione$k.wav" > "$out/wavcmp-$k.txt" 2>&1
            verdict "sessioni p$pause, sessione $k" "$(worst "$out/wavcmp-$k.txt")" "$e" " (fini di sessione nel log: $resets)"
        done
    done
fi

if [ "$WHAT" = cambi ] || [ "$WHAT" = tutto ]; then
    sig=$OUT/cambio71.wav
    prepare "$FASE4/cambio71.wav" "$sig"
    # caso | cambi (frame:sottocartella), trovati offline cercando il frame che riproduce W8
    while read -r name changes; do
        dir=$FASE4/$name
        out=$OUT/$name
        mkdir -p "$out"
        offline_args=()
        for c in $changes; do offline_args+=(--change "${c%%:*}:$dir/${c#*:}"); done
        "$OFFLINE" --dll "$DLL" --capture "$dir" --volume-db -9.3428 "${offline_args[@]}" \
            "$FASE4/cambio71.wav" "$out/offline.wav" > "$out/offline.log" 2>&1
        ctl=(--control "$FIFO")
        [ -n "$SOCKET" ] && ctl=(--socket "$SOCK")
        start "$dir" "$out/node.log" "${ctl[@]}" || { fail=$((fail + 1)); continue; }
        refused=0
        for c in $changes; do
            target="$dir/${c#*:}"
            [ -n "$PROFILE" ] && target=$("$OSBLOB" import "$target")
            if [ -n "$SOCKET" ]; then
                "$OSCTL" --socket "$SOCK" profilo "$target" frame "${c%%:*}" >> "$out/os-ctl.txt" 2>&1 || refused=$((refused + 1))
            else
                echo "profilo $target frame ${c%%:*}" > "$FIFO"
            fi
        done
        [ -n "$SOCKET" ] && "$OSCTL" --socket "$SOCK" stato >> "$out/os-ctl.txt" 2>&1
        sleep 0.5
        play "$sig"
        finish "$out/node.wav"
        e=$(errors "$out/node.log")
        [ "$refused" -gt 0 ] && e="$e+rifiutati$refused"
        n=$(grep '^\[nodo\]' "$out/node.log" | tail -1 | grep -oP 'cambi di profilo \K[0-9]+')
        "$WAVCMP" --onset --segments 1 "$dir/loopback.wav" "$out/node.wav" > "$out/wavcmp.txt" 2>&1
        "$WAVCMP" --onset "$out/offline.wav" "$out/node.wav" > "$out/wavcmp-offline.txt" 2>&1
        same=$(grep -c 'BIT' "$out/wavcmp-offline.txt")
        verdict "cambi $name" "$(worst "$out/wavcmp.txt")" "$e" " (cambi consegnati $n; contro l'offline: $([ "$same" = 2 ] && echo bit-identico || echo "residuo $(worst "$out/wavcmp-offline.txt") dB"))"
    done << 'EOF'
cambio-film-balanced-a-film-warm 753920:cambio-2
cambio-gaming-performance-a-film-balanced 819200:cambio-2
cambio-film-balanced-a-custom1-virt-off 1263616:cambio-1 1438976:cambio-3
EOF
fi

echo
echo "passati: $pass   falliti: $fail"
[ "$fail" -eq 0 ] && [ "$pass" -gt 0 ]
