#!/usr/bin/env bash
# verify-bypass.sh — verifica sul nodo vero dell'interruttore generale (doc 12.16, criterio 3).
#
#   giochi    nodo partito con --settings e "dolby": false; dinamica71 nel sink 7.1: l'uscita
#             deve essere il downmix stereo dell'ingresso (−3 dB dei giochi, poi
#             L = FL + 0,707·(FC + SL + BL), R simmetrico, LFE escluso), calcolato qui a parte
#   app       nodo partito con --bypass; stereo nel sink delle applicazioni: l'uscita deve
#             essere l'ingresso invariato
#   riacceso  nodo partito con --bypass, poi `dolby on` dal socket (os-ctl); canali71 nel sink
#             7.1: l'uscita deve coincidere con la registrazione di Windows (modulo nuovo, come
#             una sessione nuova), target/verify/film-balanced/canali71/loopback.wav
#
# Soglia: residuo <= −130 dB su ogni canale, nessun errore del nodo, `stato` con il dolby atteso.
# Il nodo usa nomi propri (os-verify-*) e suona solo sul null sink os-verify-null: le uscite
# audio reali non vengono toccate. Uscite in target/verify-bypass.
#
# Uso:
#   docs/reference/verify-bypass.sh [DLL]

set -u
cd "$(dirname "$0")/../.."

DLL="${1:-$HOME/.local/share/open-surround/DolbyAudioProcessing.dll}"
VERIFY=target/verify
OUT=target/verify-bypass
OSCTL=target/debug/os-ctl
NODE=target/debug/os-pipewire
WAVCMP=target/debug/wavcmp
PREFIX=os-verify
NULL=os-verify-null
REC="${XDG_RUNTIME_DIR:-/dev/shm}/os-verify-node.wav"
SOCK="${XDG_RUNTIME_DIR:-/dev/shm}/os-verify-control.sock"

for tool in pw-play pw-record pw-cli pw-dump wpctl ffmpeg python3; do
    command -v "$tool" > /dev/null || { echo "manca $tool"; exit 1; }
done
[ -x "$NODE" ] && [ -x "$OSCTL" ] && [ -x "$WAVCMP" ] || { echo "prima: cargo build"; exit 1; }
[ -d "$VERIFY/film-balanced/canali71" ] || { echo "manca $VERIFY (ln -s ../private/verify target/verify)"; exit 1; }
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
    rm -f "$REC" "$SOCK"
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

# Avvia il nodo ($1 = log, altri argomenti al nodo) e il registratore.
start() {
    local log="$1"
    shift
    rm -f "$REC"
    "$NODE" --dll "$DLL" --name "$PREFIX" --target "$NULL" --socket "$SOCK" -v "$@" > "$log" 2>&1 &
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

play() { # sink, file
    pw-play --target "$PREFIX-$1" -P '{ channelmix.upmix = false }' --volume 1.0 "$2" > /dev/null 2>&1
}

# Riferimento del bypass: downmix in float32, nello stesso ordine di operazioni del nodo.
downmix() { # ingresso, uscita, guadagno dB
    python3 - "$1" "$2" "$3" << 'EOF'
import sys, numpy as np
d = open(sys.argv[1], 'rb').read(); i = 12; ch = None
while i < len(d):
    cid = d[i:i+4]; sz = int.from_bytes(d[i+4:i+8], 'little')
    if cid == b'fmt ': ch = int.from_bytes(d[i+10:i+12], 'little')
    if cid == b'data': x = np.frombuffer(d[i+8:i+8+sz], dtype='<f4').reshape(-1, ch); break
    i += 8 + sz + (sz & 1)
g = np.float32(10 ** (float(sys.argv[3]) / 20)) if float(sys.argv[3]) != 0 else np.float32(1)
h = np.float32(np.sqrt(0.5))
s = x * g
if ch == 2:
    out = s
else:
    # Ordine dei sink: FL FR FC LFE RL(BL) RR(BR) SL SR.
    l = s[:, 0] * np.float32(1) + s[:, 2] * h + s[:, 4] * h + s[:, 6] * h
    r = s[:, 1] * np.float32(1) + s[:, 2] * h + s[:, 5] * h + s[:, 7] * h
    out = np.stack([l, r], axis=1)
b = out.astype('<f4').tobytes()
fmt = (3).to_bytes(2,'little') + (2).to_bytes(2,'little') + (48000).to_bytes(4,'little') + (48000*8).to_bytes(4,'little') + (8).to_bytes(2,'little') + (32).to_bytes(2,'little')
with open(sys.argv[2], 'wb') as f:
    f.write(b'RIFF' + (4 + 8 + len(fmt) + 8 + len(b)).to_bytes(4,'little') + b'WAVE')
    f.write(b'fmt ' + len(fmt).to_bytes(4,'little') + fmt + b'data' + len(b).to_bytes(4,'little') + b)
EOF
}

worst() {
    grep -E '^canale' "$1" | grep -oP 'guadagno stimato [-+]?[0-9.]+ dB, residuo \K-?[0-9.]+(?= dB rel\.)' |
        sort -g | tail -1
}

errors() {
    grep '^\[nodo\]' "$1" | tail -1 | grep -oP 'errori \K[0-9]+'
}

pass=0
fail=0
verdict() { # nome, residuo, errori, dolby atteso in `stato`, `stato` letto
    if [ -n "$2" ] && awk -v w="$2" 'BEGIN{exit (w+0 <= -130) ? 0 : 1}' && [ "${3:-1}" = "0" ] &&
        grep -q "dolby=$4 " <<< "$5"; then
        echo "OK    $1: residuo $2 dB, dolby=$4"
        pass=$((pass + 1))
    else
        echo "FAIL  $1: residuo ${2:-?} dB, errori ${3:-?}, stato: ${5:-?}"
        fail=$((fail + 1))
    fi
}

sig71=$OUT/dinamica71.wav
sig2=$OUT/stereo.wav
sigc=$OUT/canali71.wav
prepare "$VERIFY/signals/dinamica71.wav" "$sig71"
prepare "$VERIFY/signals/stereo.wav" "$sig2"
prepare "$VERIFY/signals/canali71.wav" "$sigc"

# giochi: spento dal file delle impostazioni.
out=$OUT/giochi
mkdir -p "$out"
cat > "$out/settings.json" << 'EOF'
{"version":1,"active":"film","gaming_performance":true,"dolby":false,"profiles":["film:balanced"]}
EOF
downmix "$sig71" "$out/riferimento.wav" -3
if start "$out/node.log" --settings "$out/settings.json"; then
    stato=$("$OSCTL" --socket "$SOCK" stato 2>&1)
    play 7.1 "$sig71"
    finish "$out/node.wav"
    "$WAVCMP" --onset "$out/riferimento.wav" "$out/node.wav" > "$out/wavcmp.txt" 2>&1
    verdict "giochi, --settings con dolby false" "$(worst "$out/wavcmp.txt")" "$(errors "$out/node.log")" off "$stato"
else
    fail=$((fail + 1))
fi

# app: spento da --bypass, stereo invariato.
out=$OUT/app
mkdir -p "$out"
downmix "$sig2" "$out/riferimento.wav" 0
if start "$out/node.log" --profile film:balanced --bypass; then
    stato=$("$OSCTL" --socket "$SOCK" stato 2>&1)
    play stereo "$sig2"
    finish "$out/node.wav"
    "$WAVCMP" --onset "$out/riferimento.wav" "$out/node.wav" > "$out/wavcmp.txt" 2>&1
    verdict "app, --bypass" "$(worst "$out/wavcmp.txt")" "$(errors "$out/node.log")" off "$stato"
else
    fail=$((fail + 1))
fi

# riacceso: da spento a acceso con os-ctl, poi identico a Windows.
out=$OUT/riacceso
mkdir -p "$out"
ref=$VERIFY/film-balanced/canali71
if start "$out/node.log" --capture "$ref" --volume-db -9.3428 --bypass; then
    "$OSCTL" --socket "$SOCK" dolby on > "$out/os-ctl.txt" 2>&1
    stato=$("$OSCTL" --socket "$SOCK" stato 2>&1)
    play 7.1 "$sigc"
    finish "$out/node.wav"
    "$WAVCMP" --onset --segments 3 "$ref/loopback.wav" "$out/node.wav" > "$out/wavcmp.txt" 2>&1
    verdict "riacceso con dolby on" "$(worst "$out/wavcmp.txt")" "$(errors "$out/node.log")" on "$stato"
else
    fail=$((fail + 1))
fi

echo "riepilogo: $pass OK, $fail FAIL (uscite in $OUT)"
[ "$fail" -eq 0 ]
