# open-surround: note per l'agente

open-surround esegue su Linux il `DolbyAudioProcessing.dll` originale di Dolby (Dolby Atmos for
Headphones) con un loader PE nativo, dentro un nodo PipeWire, con una finestra Slint per le
impostazioni. Il README pubblico (in inglese) spiega uso e installazione; questo file dice dove
stanno le informazioni complete.

## Repository pubblico e materiale privato

Il repository `giacomomicoli/open-surround` è pubblico. Il materiale che non deve esserlo sta nel
repository privato `giacomomicoli/open-surround-private`, montato come submodule in `private/`:

| Cosa | Dove sta adesso | Dove stava prima |
|---|---|---|
| Documento di analisi completo, in italiano: interfaccia del modulo ricostruita con REA e Ghidra, formato dei blob, piano ed esito di ogni fase (sezioni 12.x), procedure di verifica su Windows e Linux | `private/analisi-dolby-access.md` | `docs/analisi-dolby-access.md` (tolto dalla storia pubblica) |
| Registrazioni di riferimento fatte su Windows per la regressione (75 casi sintetici: blob, `info.txt`, `loopback.wav`; `signals/`) | `private/verify/` | pre-release GitHub "Verification data (2026-11-03)", eliminata |

- Per averle: `git submodule update --init private` (serve l'accesso al repository privato). Se
  `private/` è vuota, le informazioni complete non sono disponibili: dillo, non ricostruirle dai
  commenti del codice.
- I riferimenti "doc 12.x", "sez. 12.x", "passo L3/W1" nei commenti del codice, negli script di
  `docs/reference/` e nei messaggi di commit puntano a sezioni di `private/analisi-dolby-access.md`.
  Il README pubblico ne riporta solo un riassunto: per i dettagli leggi sempre il documento privato.
- Gli script di verifica cercano le registrazioni in `target/verify`:
  `ln -s ../private/verify target/verify` dalla radice del repository.
- Le 3 tracce di audio reale (`verify-reale.tar.gz`) non sono in nessun repository: restano sulla
  macchina Windows (doc 12.13, passo W3).

Il documento e le registrazioni sono privati per scelta legale: il documento descrive l'interno
dei DLL di Dolby e di Microsoft oltre a quanto serve all'interoperabilità, e le uscite di Dolby sui
segnali per canale descrivono di fatto i suoi filtri HRTF.

## Regole

- Nel repository pubblico non vanno mai: DLL o altri binari di Dolby, uscite audio di Dolby,
  dettagli interni dei moduli Dolby (indirizzi `FUN_…`, offset, comportamento interno) oltre
  all'interfaccia che il codice usa, percorsi locali o dati personali.
- Le note nuove di analisi e gli esiti delle verifiche vanno in `private/analisi-dolby-access.md`:
  commit e push nel submodule (`private/`, branch `main`), poi commit del puntatore aggiornato nel
  repository pubblico.
- Identità dei commit, in entrambi i repository: `Giacomo Micoli <dev@fakejack.mozmail.com>`, mai
  l'indirizzo personale, anche se è quello del contesto git:
  `git -c user.name="Giacomo Micoli" -c user.email=dev@fakejack.mozmail.com commit …`.
- Push via HTTPS: `git -c credential.helper='!gh auth git-credential' push …` (SSH non funziona).
- Il DLL usato dagli script e dai test è quello installato,
  `~/.local/share/open-surround/DolbyAudioProcessing.dll` (DAP 7.3.7, SHA-256 `0fada402…4e715`).
