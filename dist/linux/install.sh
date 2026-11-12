#!/usr/bin/env bash
# install.sh — installa open-surround nella home dell'utente (fase 6, doc 12.15, L4).
#
#   dist/linux/install.sh <DolbyAudioProcessing.dll>   compila in release e installa
#   dist/linux/install.sh --uninstall                  toglie quello che ha installato
#                                                      (non le impostazioni né il DLL)
#
# Lo stesso script è nel pacchetto precompilato (dist/linux/package.sh), accanto a bin/: lì
# installa i binari del pacchetto senza compilare, e il DLL si può omettere se è stato copiato
# nella cartella del pacchetto.
#
# Installa:
#   ~/.local/bin/os-pipewire, os-ui, os-ctl, os-blob
#   ~/.local/share/open-surround/DolbyAudioProcessing.dll   copia del DLL indicato
#   ~/.config/systemd/user/open-surround.service             il nodo come servizio utente
#   ~/.local/share/applications/open-surround.desktop       la finestra nel menu
# Non avvia il servizio: alla fine stampa il comando per farlo.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -x "$HERE/bin/os-pipewire" ]; then
    # pacchetto precompilato
    SRC_BIN="$HERE/bin"
    SRC_SHARE="$HERE/share"
    DEFAULT_DLL="$HERE/DolbyAudioProcessing.dll"
else
    # repository
    cd "$HERE/../.."
    SRC_BIN=target/release
    SRC_SHARE=dist/linux
    DEFAULT_DLL=""
fi

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

DLL="${1:-$DEFAULT_DLL}"
[ -n "$DLL" ] || { echo "uso: $0 <DolbyAudioProcessing.dll> | --uninstall"; exit 2; }
[ -f "$DLL" ] || { echo "$DLL: non trovato"; exit 1; }

[ "$SRC_BIN" != target/release ] || cargo build --release -p os-pipewire -p os-ui -p os-profiles
mkdir -p "$BIN" "$DATA" "$(dirname "$UNIT")" "$(dirname "$DESKTOP")"
for t in "${TOOLS[@]}"; do install -m 755 "$SRC_BIN/$t" "$BIN/$t"; done
[ "$(realpath "$DLL")" = "$(realpath -m "$DATA/DolbyAudioProcessing.dll")" ] || install -m 644 "$DLL" "$DATA/DolbyAudioProcessing.dll"
install -m 644 "$SRC_SHARE/open-surround.service" "$UNIT"
install -m 644 "$SRC_SHARE/open-surround.desktop" "$DESKTOP"
systemctl --user daemon-reload

echo "installato. Per avviare il nodo ora e a ogni accesso:"
echo "  systemctl --user enable --now open-surround"
echo "poi rendere predefinito il sink open-surround-stereo (o open-surround-7.1) e aprire"
echo "open-surround dal menu (os-ui)."
