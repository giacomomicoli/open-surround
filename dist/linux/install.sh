#!/usr/bin/env bash
# install.sh — installa open-surround nella home dell'utente (fase 6, doc 12.15, L4).
#
#   dist/linux/install.sh <DolbyAudioProcessing.dll>   compila in release e installa
#   dist/linux/install.sh --uninstall                  toglie quello che ha installato
#                                                      (non le impostazioni né il DLL)
#
# Installa:
#   ~/.local/bin/os-pipewire, os-ui, os-ctl, os-blob
#   ~/.local/share/open-surround/DolbyAudioProcessing.dll   copia del DLL indicato
#   ~/.config/systemd/user/open-surround.service             il nodo come servizio utente
#   ~/.local/share/applications/open-surround.desktop       la finestra nel menu
# Non avvia il servizio: alla fine stampa il comando per farlo.

set -euo pipefail
cd "$(dirname "$0")/../.."

BIN="$HOME/.local/bin"
DATA="$HOME/.local/share/open-surround"
UNIT="$HOME/.config/systemd/user/open-surround.service"
DESKTOP="$HOME/.local/share/applications/open-surround.desktop"
TOOLS=(os-pipewire os-ui os-ctl os-blob)

if [ "${1:-}" = --uninstall ]; then
    systemctl --user disable --now open-surround.service 2> /dev/null || true
    for t in "${TOOLS[@]}"; do rm -f "$BIN/$t"; done
    rm -f "$UNIT" "$DESKTOP"
    systemctl --user daemon-reload
    echo "tolto; restano le impostazioni (~/.config/open-surround) e il DLL ($DATA)"
    exit 0
fi

DLL="${1:?uso: $0 <DolbyAudioProcessing.dll> | --uninstall}"
[ -f "$DLL" ] || { echo "$DLL: non trovato"; exit 1; }

cargo build --release -p os-pipewire -p os-ui -p os-profiles
mkdir -p "$BIN" "$DATA" "$(dirname "$UNIT")" "$(dirname "$DESKTOP")"
for t in "${TOOLS[@]}"; do install -m 755 "target/release/$t" "$BIN/$t"; done
[ "$(realpath "$DLL")" = "$(realpath -m "$DATA/DolbyAudioProcessing.dll")" ] || install -m 644 "$DLL" "$DATA/DolbyAudioProcessing.dll"
install -m 644 dist/linux/open-surround.service "$UNIT"
install -m 644 dist/linux/open-surround.desktop "$DESKTOP"
systemctl --user daemon-reload

echo "installato. Per avviare il nodo ora e a ogni accesso:"
echo "  systemctl --user enable --now open-surround"
echo "poi rendere predefinito il sink open-surround-stereo (o open-surround-7.1) e aprire"
echo "open-surround dal menu (os-ui)."
