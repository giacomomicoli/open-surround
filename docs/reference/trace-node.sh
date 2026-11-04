#!/usr/bin/env bash
# trace-node.sh — traccia degli shim sul nodo PipeWire (fase 4, doc sez. 12.13): os-pipewire sotto
# gdb con trace-shims.gdb, mentre suonano insieme un 7.1 nel sink dei giochi e uno stereo in quello
# delle applicazioni, con cambio di volume e muto, fine della sessione (riconfigurazione dopo 1 s)
# e una seconda sessione. Stampa i conteggi per shim, da confrontare con la tabella di 12.12/12.13.
# Suona solo su un null sink privato (os-trace-null). Richiede i segnali preparati da
# verify-pipewire.sh (target/verify-pipewire/signals) e la build debug.
#
# Uso: docs/reference/trace-node.sh [DLL]
set -u
cd "$(dirname "$0")/../.."
S=target/trace-node
mkdir -p "$S"
DLL="${1:-$HOME/Documenti/dolby-bins/DolbyAudioProcessing.dll}"
SIG=target/verify-pipewire/signals
NULL=os-trace-null
node_id() {
    pw-dump 2>/dev/null | python3 -c '
import json, sys
for o in json.load(sys.stdin):
    if o.get("type", "").endswith(":Node") and o["info"]["props"].get("node.name") == sys.argv[1]:
        print(o["id"]); break' "$1"
}
pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$NULL media.class=Audio/Sink audio.position=[FL FR] audio.rate=48000 object.linger=true }" > /dev/null
sleep 1
gdb -q -batch -ex "handle SIGINT SIGTERM nostop noprint pass" -x docs/reference/trace-shims.gdb --args \
    target/debug/os-pipewire --capture target/verify/film-balanced-2/canali71 --dll "$DLL" \
    --name os-trace --target "$NULL" --idle-ms 1000 -v > "$S/trace-node.log" 2>&1 &
gdb_pid=$!
for _ in $(seq 100); do [ -n "$(node_id os-trace-output)" ] && break; sleep 0.3; done
pw-record --target "$NULL" -P '{ stream.capture.sink = true }' --format f32 --channels 2 "$S/trace-node.wav" &
rec=$!
sleep 1
g=$(node_id os-trace-7.1); a=$(node_id os-trace-stereo)
# Sessione 1: giochi e applicazioni insieme, volume e muto durante la riproduzione.
pw-play --target os-trace-7.1 -P '{ channelmix.upmix = false }' "$SIG/canali71.wav" &
p1=$!
sleep 1
pw-play --target os-trace-stereo "$SIG/stereo.wav" &
p2=$!
sleep 2; wpctl set-volume "$a" 0.5
sleep 1; wpctl set-mute "$g" 1
sleep 1; wpctl set-mute "$g" 0
sleep 1; wpctl set-volume "$g" 1.0
wait $p1 $p2
# Fine sessione: dopo 1 s senza flussi il modulo viene riconfigurato.
sleep 3
# Sessione 2.
pw-play --target os-trace-stereo "$SIG/stereo.wav"
sleep 2
kill -INT $rec; wait $rec
pkill -TERM -f "os-pipewire --capture target/verify/film-balanced-2/canali71 --dll .* --name os-trace"
wait $gdb_pid
pw-cli destroy "$(node_id $NULL)" > /dev/null
grep '^\[trace\]' "$S/trace-node.log" | sed 's/ -> .*//; s/ rva .*//; s/ pc .*//' | sort | uniq -c | sort -rn
grep -E "volume dopo|riconfigurazioni|arresto|errore" "$S/trace-node.log" | tail -8
