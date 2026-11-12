#!/usr/bin/env bash
# verify-route.sh — verifica dell'instradamento automatico del nodo (fase 4, doc sez. 12.13, L6):
# con il sink di default su uno dei due sink del nodo, un flusso che non ha scelto il sink deve
# finire nel sink dei giochi se ha più di 2 canali, in quello delle applicazioni se no; un flusso
# con il sink scelto da sé resta dov'è. Per ogni caso riproduce un file senza --target e legge da
# pw-dump in quale sink entrano i collegamenti del flusso.
#
# ATTENZIONE: cambia per qualche secondo il sink di default dell'utente (wpctl set-default) e lo
# ripristina alla fine, anche in caso di errore. In quei secondi i flussi che seguono il default
# suonano sul null sink privato os-verify-null (silenzio). Le uscite reali non vengono toccate.
#
# Uso: docs/reference/verify-route.sh [DLL]

set -u
cd "$(dirname "$0")/../.."

DLL="${1:-$HOME/.local/share/open-surround/DolbyAudioProcessing.dll}"
NODE=target/debug/os-pipewire
OUT=target/verify-route
PREFIX=os-verify
NULL=os-verify-null
SIG=target/verify-pipewire/signals
mkdir -p "$OUT"
[ -x "$NODE" ] || { echo "prima: cargo build"; exit 1; }
[ -f "$SIG/canali71.wav" ] && [ -f "$SIG/stereo.wav" ] || { echo "prima: verify-pipewire.sh (segnali preparati)"; exit 1; }

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

# Sink in cui entrano i collegamenti dei flussi con media.name = $1 (nomi separati da spazio).
sinks_of() {
    pw-dump 2>/dev/null | python3 -c '
import json, sys
objs = json.load(sys.stdin)
nodes = {o["id"]: o["info"]["props"] for o in objs if o.get("type", "").endswith(":Node")}
streams = {i for i, p in nodes.items() if p.get("media.name", "").endswith(sys.argv[1])
           and p.get("media.class") == "Stream/Output/Audio"}
out = set()
for o in objs:
    if o.get("type", "").endswith(":Link"):
        i = o["info"]
        name = nodes.get(i["input-node-id"], {}).get("node.name", "?")
        # Solo i sink del nodo: il misuratore di volume del desktop collega anche i flussi.
        if i["output-node-id"] in streams and name.startswith(sys.argv[2]):
            out.add(name)
print(" ".join(sorted(out)) or "-")' "$1" "$PREFIX-"
}

prev=$(pw-metadata 0 default.configured.audio.sink 2>/dev/null | grep -oP "value:'\K[^']+")
node_pid=""
player=""
cleanup() {
    if [ -n "$prev" ]; then
        pw-metadata 0 default.configured.audio.sink "$prev" Spa:String:JSON > /dev/null
    else
        pw-metadata -d 0 default.configured.audio.sink > /dev/null
    fi
    echo "sink di default ripristinato: ${prev:-nessuno configurato}"
    [ -n "$player" ] && kill "$player" 2> /dev/null
    [ -n "$node_pid" ] && { kill -INT "$node_pid"; wait "$node_pid"; }
    id=$(node_id "$NULL")
    [ -n "$id" ] && pw-cli destroy "$id" > /dev/null 2>&1
}
trap cleanup EXIT

if [ -z "$(node_id "$NULL")" ]; then
    pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$NULL \
        node.description=\"open-surround verifica (null)\" media.class=Audio/Sink \
        audio.position=[FL FR] audio.rate=48000 object.linger=true }" > /dev/null
fi
wait_node "$NULL" > /dev/null || { echo "null sink non creato"; exit 1; }
"$NODE" --capture target/verify/gaming-performance/canali71 --dll "$DLL" --name "$PREFIX" \
    --target "$NULL" -v > "$OUT/node.log" 2>&1 &
node_pid=$!
g=$(wait_node "$PREFIX-7.1") && a=$(wait_node "$PREFIX-stereo") || { echo "il nodo non è partito"; exit 1; }
# File con nomi propri, per riconoscere i flussi in pw-dump.
c71="$OUT/canali71-os-verify-route.wav"
st="$OUT/stereo-os-verify-route.wav"
cp "$SIG/canali71.wav" "$c71"
cp "$SIG/stereo.wav" "$st"

pass=0
fail=0
check() { # descrizione, file, atteso, comando...
    local desc="$1" file="$2" want="$3"
    shift 3
    "$@" > /dev/null 2>&1 &
    player=$!
    sleep 2
    got=$(sinks_of "$(basename "$file")")
    kill "$player" 2> /dev/null; wait "$player" 2> /dev/null
    player=""
    if [ "$got" = "$want" ]; then
        echo "OK    $desc: $got"
        pass=$((pass + 1))
    else
        echo "FAIL  $desc: $got (atteso $want)"
        fail=$((fail + 1))
    fi
    sleep 0.5
}

for def in stereo 7.1; do
    id=$([ "$def" = stereo ] && echo "$a" || echo "$g")
    wpctl set-default "$id"
    sleep 0.5
    echo "-- sink di default: $PREFIX-$def"
    check "pw-play 7.1 senza --target" "$c71" "$PREFIX-7.1" pw-play --volume 0 "$c71"
    check "pw-play stereo senza --target" "$st" "$PREFIX-stereo" pw-play --volume 0 "$st"
    check "paplay 7.1 senza dispositivo" "$c71" "$PREFIX-7.1" paplay --volume 0 "$c71"
    check "paplay stereo senza dispositivo" "$st" "$PREFIX-stereo" paplay --volume 0 "$st"
done
echo "-- sink scelto dall'applicazione: resta libera"
check "pw-play 7.1 --target $PREFIX-stereo" "$c71" "$PREFIX-stereo" pw-play --volume 0 --target "$PREFIX-stereo" "$c71"
check "paplay stereo -d $PREFIX-7.1" "$st" "$PREFIX-7.1" paplay --volume 0 -d "$PREFIX-7.1" "$st"

echo
grep '^\[instradamento\]' "$OUT/node.log" | sed 's/^/  /'
echo "passati: $pass   falliti: $fail"
[ "$fail" -eq 0 ]
