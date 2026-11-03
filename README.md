# open-surround

**Dolby Atmos for Headphones on Linux, with exactly the same sound as Windows.**

open-surround runs the original Dolby audio processing module that ships with Dolby Access on
Windows (`DolbyAudioProcessing.dll`, DAP 7.3.7) outside of the Windows audio stack, feeds it the
same profile data that Dolby Access writes, and produces the same binaural output, bit for bit.
The goal is to use a Dolby Access license you already own on Linux, with every profile and
setting of the Windows app (Game, Movie, Music, Voice, Custom 1-3, intelligent equalizer,
graphic equalizer, volume leveler, virtualizer) and no audible difference.

> **Status: work in progress.** Phases 0-3 of the [roadmap](#roadmap) are done and verified:
> the original module runs on Linux through a native PE loader (no Wine) and reproduces the
> Windows output to within float32 rounding. Nothing here is usable as an everyday Linux audio
> sink yet (that is phase 4, PipeWire).

## Why not reimplement the DSP?

Rewriting or decompiling the Dolby DSP would at best sound *similar*. The module is plain
x86-64 machine code with a small, now documented, COM-style interface, so the same file can run
on both operating systems. On Windows the system loader loads it; on Linux a native PE loader
(no Wine) will load it and provide the handful of Windows APIs it imports. A shared core makes
sure both platforms hand the module exactly the same data.

```
              os-dap-core (shared: profile blobs, objects, metadata, gain chain, routing)
                 │                                         │
   Windows backend: LoadLibrary + COM objects       Linux backend: native PE loader + shims
   (os-dap-host, verified)                          of the Windows APIs the module uses
                 │                                         │                 │
        DolbyAudioProcessing.dll (original, the same file on both platforms)
```

On Linux the loader maps the sections, applies relocations, fills the import table with Rust
implementations of the ~130 Windows APIs the module imports (win64 ABI), installs a synthetic
TEB/PEB in the GS segment for the module's thread-local storage, runs `DllMain` and the WinRT
activation sequence. Details in the [analysis document](docs/analisi-dolby-access.md),
section 12.11 (Italian).

## Verified results

Every state reachable from the Dolby Access UI (19 configurations: Game ×5, Movie ×4,
Music ×4, Voice, Custom 1 with virtualizer on/off and leveler off, Custom 2, Custom 3) was
recorded on Windows through `ISpatialAudioClient` plus endpoint loopback, then reproduced
offline by open-surround with the same profile blobs — on Windows with the system loader
(phase 1) and on Linux with the native PE loader (phase 3):

| Signal | Residual vs Windows |
|---|---|
| Pink noise on each 7.1 channel, dynamic 7.1 (−40…0 dBFS steps), stereo as game bed | −148.7 … −149.0 dB (float32 rounding) |
| Stereo from a regular application (bypass objects) | −134 … −147 dB |
| Real Atmos content (5.1 core, 194 s) on Movie, Game, Custom 1 | −148.8 dB |

On Linux, all 74 saved captures replay with the same numbers as the Windows host, case by
case (`docs/reference/regress-linux.sh`): beds and dynamics at −148.7…−149.0 dB, application
stereo at −134.0…−147.4 dB, estimated gain 0.000 dB everywhere.

The estimated gain difference is 0.000 dB in every case. Details, including the Windows gain
chain (−3 dB → Dolby → endpoint volume) and how application stereo differs from game beds, are
in the [analysis document](docs/analisi-dolby-access.md) (Italian), sections 12.8-12.10.

## Repository layout

```
crates/
├─ os-dap-core/   platform-independent core (no Windows dependencies)
│   ├─ layout.rs    beds, bypass and dynamic objects, encoder config, channel maps, input gain
│   ├─ abi.rs       in-memory structures read by EncodeAudioData (ObjectEntry, GranuleMeta, …)
│   ├─ props.rs     profile blobs from captures, endpoint properties
│   ├─ engine.rs    ModuleSetup, the Backend trait, Engine (buffers, metadata, block processing)
│   └─ offline.rs   command line and WAV → WAV rendering, identical on every platform
├─ os-dap-host/    Windows backend: system loader, IAT hooks, in-memory COM objects
├─ os-pe-loader/   native PE32+ loader for Linux: sections, relocations, imports, TLS, DllMain,
│                  synthetic TEB/PEB + GS base, Rust shims of the imported Windows APIs
├─ os-dap-linux/   Linux backend: same COM sequence as the Windows shell, on top of the loader
└─ os-verify/      reference tools: ref-capture (Windows recording) and wavcmp (comparison)
docs/
├─ analisi-dolby-access.md   full analysis: interface reconstruction, blob format, verification (Italian)
└─ reference/                capture and verification scripts, captured profile blobs
```

A platform backend only has to implement `os_dap_core::Backend`:

```rust
pub trait Backend: Sized {
    /// Load the module and run DllGetActivationFactory → Initialize →
    /// SetAPOInitParameters → ConfigureEncoder with the data in `setup`.
    fn open(setup: &ModuleSetup) -> Result<Self>;
    /// IDAPModule2::EncodeAudioData.
    unsafe fn encode(&mut self, input: *mut EncodeInput, out: *mut f32) -> Result<()>;
}
```

## Requirements

- Rust (stable, 2021 edition).
- `DolbyAudioProcessing.dll` from **your own** Dolby Access installation. It is not included and
  never will be.
- Profile blobs: the values Dolby Access writes for an audio endpoint, captured with
  `docs/reference/capture_asar.ps1` or `ref-capture` on Windows.
- For verification: ffmpeg (test signals) and a Windows machine with Dolby Access and an
  endpoint with Dolby Atmos for Headphones enabled.

## Building and usage

Same command line on both platforms; only the backend changes.

Windows:

```
cargo build
target\debug\os-dap-host --capture docs\reference\captures\current-2026-11-02 ^
    --capture docs\reference\captures\movie-balanced --endpoint 32ed16b6 in71.wav out.wav
```

Linux (x86-64):

```
cargo build
target/debug/os-dap-linux --capture target/verify/gaming-performance/canali71 \
    --dll /path/to/DolbyAudioProcessing.dll --volume-db -9.3428 \
    target/verify/signals/canali71.wav out.wav
```

Input is a 48 kHz WAV with 1, 2, 6 (5.1), 8 (7.1) or 12 (7.1.4) channels; output is binaural
stereo float32. Main options (`--help` for all of them):

| Option | Meaning |
|---|---|
| `--capture <dir>` | capture folder with profile blobs; repeatable, later folders override earlier ones |
| `--endpoint <hex8>` | endpoint to take from the captures |
| `--mode cuffie\|speaker` | headphones (default) or built-in speakers |
| `--stereo-bypass` | treat stereo input like a regular application (bypass objects, 0 dB) instead of a game bed |
| `--gain-db`, `--volume-db` | gain before the module (default −3 dB like Windows) and endpoint volume after it |
| `--dll <file>` | path to `DolbyAudioProcessing.dll` |
| `-v` | trace the module's calls into the host and print its internal log |

## Verification workflow

- `docs/reference/verify-profile.ps1 -Name <profile>`: with the profile selected in Dolby Access,
  plays the test signals on Windows, records the processed loopback and the profile blobs, renders
  the same signals with open-surround and compares them.
- `docs/reference/regress-host.ps1 [-Baseline <exe>]`: replays every saved capture in
  `target/verify` without Dolby Access, compares against the Windows loopback and optionally
  against another build byte for byte. This is the acceptance test for every later phase.
- `docs/reference/regress-linux.sh [dll] [filter]`: same replay on Linux with the native PE
  loader (`os-dap-linux`), same acceptance threshold of −130 dB residual per case.
- `docs/reference/trace-shims.gdb`: gdb script that prints which risky Windows API shims the
  module actually calls during a run (used to triage phase 3-bis).

The recordings in `target/verify` are large and are not part of the repository. The synthetic-signal
cases (75 captures with their test signals) are published as a release asset:

```
gh release download verify-data-2026-11-03 -p verify-data.tar.gz
tar -xzf verify-data.tar.gz -C target
```

Some `info.txt` files reference their input with an absolute Windows path; the input files are in
`target/verify/signals/`.

## Roadmap

| Phase | Goal | Status |
|---|---|---|
| 0 | Reconstruct the `IDAPModule2` interface and the call sequence of the Windows shell | done |
| 1 | Windows host that loads the module and renders WAV → binaural stereo, identical to Windows | done |
| 2 | Shared core `os-dap-core` behind a backend trait | done |
| 3 | Native PE loader for Linux (sections, relocations, imports, TLS, static CRT, `DllMain`) plus Rust shims for the imported Windows APIs | done |
| 3-bis | Shim correctness before real time: defects found in review after phase 3 (RTWorkQ, backend IIDs, `WindowsStringHasEmbeddedNull`, condition variables, `VirtualProtect`, named objects, thread exit), off the offline path or working by chance; see section 12.12 of the analysis | next |
| 4 | Real time on Linux: PipeWire node with a virtual 7.1(.4) sink for games and stereo routing for regular applications | planned |
| 5 | Profile model and blob serializer, byte-identical to what Dolby Access writes | planned |
| 6 | User interface with the same controls as Dolby Access, on both platforms | planned |

Acceptance for phases 3-4: the same inputs and blobs give a residual of about −149 dB against the
Windows recordings in every configuration.

## Legal

open-surround is an independent project, not affiliated with or endorsed by Dolby Laboratories
or Microsoft. Dolby, Dolby Atmos and Dolby Access are trademarks of Dolby Laboratories.

It is meant for personal interoperability use by people who hold a valid Dolby Access license.
It contains no Dolby binaries; you must supply the module from your own installation and comply
with its license terms. The profile blobs under `docs/reference/captures` were captured from the
author's own installation for testing.

## License

The code in this repository is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. This does not cover any Dolby software or data the code interoperates with.
