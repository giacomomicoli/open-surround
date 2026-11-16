# Changelog

All notable changes to open-surround. Versions follow [Semantic Versioning](https://semver.org/);
before 1.0, a minor version may add features or change behaviour.

## 0.3.0 — 2026-11-16

### Added
- **Global on/off switch** ("Elaborazione Dolby", at the top of the settings window). Off, the
  outputs stay where they are but Dolby's module is not used: stereo apps play unchanged and
  5.1/7.1 games are folded down to plain stereo (centre, side, rear and height channels at −3 dB
  on their side, LFE dropped). Handy to compare with and without Dolby, or to listen to
  something without virtualization, without moving any app. The switch is saved; turning it back
  on restarts Dolby's processing from a clean state. (#12)
- `os-ctl dolby on|off`, `os-pipewire --bypass`; `os-ctl stato` now reports `dolby=on|off`. (#12)

### Fixed
- The settings window starts on Windows (it no longer needs `XDG_RUNTIME_DIR`), and the
  workspace tests build there. (#10)
- PE loader: the free-threaded marshaler's `QueryInterface` returns `E_POINTER` on a null output
  pointer instead of writing to it. (#6)

### Notes
- The settings file stays readable by 0.2.0 unless the switch is off. If you go back to 0.2.0
  by hand, turn Dolby processing on first.

## 0.2.0 — 2026-11-12

### Added
- Updates from the settings window: signed releases (minisign), check of the installed Dolby
  DLL, automatic rollback if the service does not come back. (#1)
- Version and commit in every binary (`os-ctl versione`). (#1)

## 0.1.0 — 2026-11-12

First public release: Dolby's original module in a PipeWire node (7.1 sink for games, stereo
sink for apps), the seven Dolby Access profiles with byte-identical profile data, the settings
window, the systemd user service and the prebuilt Linux package.
