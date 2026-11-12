#!/usr/bin/env bash
# package.sh — crea il pacchetto precompilato per Linux x86_64 (doc 12.15, "Pacchetto
# precompilato"): target/dist/open-surround-<versione>-linux-x86_64.tar.gz.
#
#   dist/linux/package.sh [versione]     default: la versione del workspace
#
# Il pacchetto non contiene il DLL di Dolby: chi lo scarica ci copia il proprio
# DolbyAudioProcessing.dll e poi lancia ./open-surround (senza installare) oppure ./install.sh
# (come dist/linux/install.sh, ma con i binari del pacchetto e senza compilare).

set -euo pipefail
cd "$(dirname "$0")/../.."

VER="${1:-$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)}"
NAME="open-surround-$VER-linux-x86_64"
OUT="target/dist/$NAME"
TOOLS=(os-pipewire os-ui os-ctl os-blob)

cargo build --release --locked -p os-pipewire -p os-ui -p os-profiles

rm -rf "$OUT" "$OUT.tar.gz"
mkdir -p "$OUT/bin" "$OUT/share"
for t in "${TOOLS[@]}"; do install -m 755 -s "target/release/$t" "$OUT/bin/$t"; done
install -m 755 dist/linux/open-surround dist/linux/install.sh "$OUT/"
install -m 644 dist/linux/open-surround.service dist/linux/open-surround.desktop "$OUT/share/"
install -m 644 dist/linux/LEGGIMI.txt LICENSE-MIT LICENSE-APACHE "$OUT/"

tar -C target/dist --owner=0 --group=0 -czf "$OUT.tar.gz" "$NAME"
(cd target/dist && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "$OUT.tar.gz"
