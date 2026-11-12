#!/usr/bin/env bash
# install.sh — installa open-surround nella home dell'utente (fase 6, doc 12.15, L4).
#
#   dist/linux/install.sh [DolbyAudioProcessing.dll]   compila in release e installa
#   dist/linux/install.sh --uninstall                  toglie quello che ha installato
#                                                      (non le impostazioni né il DLL)
#
# Lo stesso script è nel pacchetto precompilato (dist/linux/package.sh), accanto a bin/: lì
# installa i binari del pacchetto senza compilare.
#
# Il DLL si può omettere: si usa quello copiato nella cartella del pacchetto, altrimenti la
# copia già installata (~/.local/share/open-surround/DolbyAudioProcessing.dll).
#
# Installa:
#   ~/.local/bin/os-pipewire, os-ui, os-ctl, os-blob, os-update
#   ~/.local/share/open-surround/DolbyAudioProcessing.dll   copia del DLL indicato
#   ~/.local/share/open-surround/installed.json              versione, commit, origine
#                                                            (release o repo), SHA-256 del DLL
#   ~/.config/systemd/user/open-surround.service             il nodo come servizio utente
#   ~/.local/share/applications/open-surround.desktop       la finestra nel menu
# Le installazioni da una release si aggiornano dalla finestra (os-update, doc 12.15
# "Aggiornamenti"); quelle dal repository ricevono solo l'avviso.
# Non avvia il servizio: alla fine stampa il comando per farlo.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -x "$HERE/bin/os-pipewire" ]; then
    # pacchetto precompilato
    SRC_BIN="$HERE/bin"
    SRC_SHARE="$HERE/share"
    DEFAULT_DLL="$HERE/DolbyAudioProcessing.dll"
    SOURCE=release
    VERSION="$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$HERE/versione.json" 2> /dev/null || true)"
    COMMIT="$(sed -n 's/.*"commit": *"\([^"]*\)".*/\1/p' "$HERE/versione.json" 2> /dev/null || true)"
else
    # repository
    cd "$HERE/../.."
    SRC_BIN=target/release
    SRC_SHARE=dist/linux
    DEFAULT_DLL=""
    SOURCE=repo
    VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
    COMMIT="$(git rev-parse HEAD 2> /dev/null || true)"
fi

BIN="$HOME/.local/bin"
DATA="$HOME/.local/share/open-surround"
UNIT="$HOME/.config/systemd/user/open-surround.service"
DESKTOP="$HOME/.local/share/applications/open-surround.desktop"
INSTALLED="$DATA/installed.json"
TOOLS=(os-pipewire os-ui os-ctl os-blob os-update)

if [ "${1:-}" = --uninstall ]; then
    systemctl --user disable --now open-surround.service 2> /dev/null || true
    for t in "${TOOLS[@]}"; do rm -f "$BIN/$t"; done
    rm -f "$UNIT" "$DESKTOP" "$INSTALLED"
    systemctl --user daemon-reload
    echo "tolto; restano le impostazioni (~/.config/open-surround) e il DLL ($DATA)"
    exit 0
fi

DLL="${1:-}"
if [ -z "$DLL" ]; then
    # Nessun argomento: il DLL del pacchetto, altrimenti la copia già installata.
    if [ -n "$DEFAULT_DLL" ] && [ -f "$DEFAULT_DLL" ]; then
        DLL="$DEFAULT_DLL"
    elif [ -f "$DATA/DolbyAudioProcessing.dll" ]; then
        DLL="$DATA/DolbyAudioProcessing.dll"
        echo "uso il DLL già installato: $DLL"
    else
        echo "uso: $0 [DolbyAudioProcessing.dll] | --uninstall"
        echo "manca il DLL: indicalo, oppure copialo in ${DEFAULT_DLL:-$DATA/DolbyAudioProcessing.dll}"
        exit 2
    fi
fi
[ -f "$DLL" ] || { echo "$DLL: non trovato"; exit 1; }

[ "$SRC_BIN" != target/release ] || cargo build --release -p os-pipewire -p os-ui -p os-profiles -p os-update
mkdir -p "$BIN" "$DATA" "$(dirname "$UNIT")" "$(dirname "$DESKTOP")"
for t in "${TOOLS[@]}"; do install -m 755 "$SRC_BIN/$t" "$BIN/$t"; done
[ "$(realpath "$DLL")" = "$(realpath -m "$DATA/DolbyAudioProcessing.dll")" ] || install -m 644 "$DLL" "$DATA/DolbyAudioProcessing.dll"
install -m 644 "$SRC_SHARE/open-surround.service" "$UNIT"
install -m 644 "$SRC_SHARE/open-surround.desktop" "$DESKTOP"
DLL_SHA="$(sha256sum "$DATA/DolbyAudioProcessing.dll" | cut -d' ' -f1)"
printf '{\n  "version": "%s",\n  "commit": "%s",\n  "source": "%s",\n  "dll_sha256": "%s"\n}\n' \
    "${VERSION:-sconosciuta}" "$COMMIT" "$SOURCE" "$DLL_SHA" > "$INSTALLED.nuovo"
mv "$INSTALLED.nuovo" "$INSTALLED"
systemctl --user daemon-reload

echo "installato. Per avviare il nodo ora e a ogni accesso:"
echo "  systemctl --user enable --now open-surround"
echo "poi rendere predefinito il sink open-surround-stereo (o open-surround-7.1) e aprire"
echo "open-surround dal menu (os-ui)."
