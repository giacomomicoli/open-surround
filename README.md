# open-surround

**Dolby Atmos for Headphones on Linux, with exactly the same sound as Windows.**

open-surround runs the original Dolby audio processing module that ships with Dolby Access on
Windows (`DolbyAudioProcessing.dll`, DAP 7.3.7) outside of the Windows audio stack, feeds it the
same profile data that Dolby Access writes, and produces the same binaural output, bit for bit.
The goal is to use a Dolby Access license you already own on Linux, with every profile and
setting of the Windows app (Game, Movie, Music, Voice, Custom 1-3, intelligent equalizer,
graphic equalizer, volume leveler, virtualizer) and no audible difference.

> **Status: work in progress.** Phases 0-3 and 3-bis of the [roadmap](#roadmap) are done, and
> all the verification of phase 4 is done: the original module runs on Linux through a native PE
> loader (no Wine), and a PipeWire node (`os-pipewire`) runs it in real time with a virtual 7.1
> sink for games and a stereo sink for regular applications, reproducing the Windows output to
> within float32 rounding, on the synthetic test signals and on 194 s of real 5.1 audio alike,
> with graph quanta of 1024 and 480 frames. Phase 4 stays open for three behaviours that are not
> matched yet: what Windows does on a profile change during playback, when it starts the module
> afresh between listening sessions (both need new reference recordings on Windows), and
> automatic routing of games and applications to the right sink. The resume point, with the
> exact steps, is section 12.13 of the analysis, "Punto di ripresa".
> Profiles are still taken from captures made on Windows: the profile model and the user
> interface are phases 5 and 6.

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
stereo at −134.0…−147.4 dB, estimated gain 0.000 dB everywhere. The same 74 cases played through
PipeWire into the real-time node and recorded from its output give the same residuals, case by
case (`docs/reference/verify-pipewire.sh`), and so do the 3 real-audio tracks; the node adds no
latency beyond the module's own 512 frames (10.7 ms). The node's recorded output is
bit-identical to the offline render, with the default graph quantum of 1024 frames and with the
graph forced to 480 frames.

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
│   ├─ options.rs   module options shared by the offline renderer and the PipeWire node
│   ├─ realtime.rs  Streamer: graph cycles of any length → fixed module blocks, sessions, volume
│   └─ offline.rs   command line and WAV → WAV rendering, identical on every platform
├─ os-dap-host/    Windows backend: system loader, IAT hooks, in-memory COM objects
├─ os-pe-loader/   native PE32+ loader for Linux: sections, relocations, imports, TLS, DllMain,
│                  synthetic TEB/PEB + GS base, Rust shims of the imported Windows APIs
├─ os-dap-linux/   Linux backend: same COM sequence as the Windows shell, on top of the loader
├─ os-pipewire/    Linux real time: PipeWire node (two virtual sinks → module → binaural output)
│                  and os-pw-latency (latency measured inside the graph)
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
- For real time on Linux: PipeWire (developed with 1.6) with the `libpipewire-0.3` and
  `libspa-0.2` headers, and libclang for the bindings; `pw-play`, `pw-record`, `wpctl` for the
  verification script.
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

### Real time on Linux (PipeWire)

`os-pipewire` takes the same module options and creates two virtual sinks in front of the
module plus a binaural stereo output, all in one PipeWire node group:

```
cargo build --release
target/release/os-pipewire --capture docs/reference/captures/<profile> \
    --dll /path/to/DolbyAudioProcessing.dll
```

| Node | Role |
|---|---|
| `open-surround-7.1` (`--channels 12`: `open-surround-7.1.4`) | games: the channels go to the module's beds with −3 dB, like a spatial audio client on Windows |
| `open-surround-stereo` | regular applications: the two bypass objects at 0 dB, like a plain WASAPI stream on Windows |
| `open-surround-output` | binaural output, linked to the default device or to `--target <node>` |

Both sinks feed the same `EncodeAudioData` call in the same graph cycle, as on Windows. Their
volume (one value shared by the two sinks, like a Windows endpoint) is applied after the module,
where Windows applies the endpoint volume; the signal entering the module is never scaled. Make
`open-surround-stereo` the default sink and send games to `open-surround-7.1` (for example
`PULSE_SINK=open-surround-7.1 %command%` in Steam). PipeWire upmixes stereo streams played
into the 7.1 sink unless the client sets `channelmix.upmix = false`.

The module is only called while a sink receives audio, and after `--idle-ms` (default 2000) of
silence on both sinks it is reconfigured, so each listening session starts from a clean module
state, as the Windows reference recordings do (the volume leveler remembers what it processed).
The 2 s are a choice, not a measurement: when exactly Windows starts the module afresh is one of
the open points of phase 4. Changing profile means restarting the node with other captures.
The node adds no latency when the graph quantum is a multiple of 256 frames (256, 512,
1024…) and less than 256 frames otherwise; the module itself delays the signal by 512 frames
(10.7 ms). Both are announced to the graph (`ProcessLatency`), so players can compensate.
`os-pw-latency` measures what the node adds inside the graph. Other options: `--help`.

## Verification workflow

- `docs/reference/verify-profile.ps1 -Name <profile>`: with the profile selected in Dolby Access,
  plays the test signals on Windows, records the processed loopback and the profile blobs, renders
  the same signals with open-surround and compares them.
- `docs/reference/regress-host.ps1 [-Baseline <exe>]`: replays every saved capture in
  `target/verify` without Dolby Access, compares against the Windows loopback and optionally
  against another build byte for byte. This is the acceptance test for every later phase.
- `docs/reference/regress-linux.sh [dll] [filter]`: same replay on Linux with the native PE
  loader (`os-dap-linux`), same acceptance threshold of −130 dB residual per case.
- `QUANTUM=<n> docs/reference/regress-linux.sh`: the same replay through the real-time adapter
  in cycles of n frames (`--quantum`); the outputs must stay byte-identical to the baseline
  (`docs/reference/regress-linux-baseline.sha256`).
- `docs/reference/verify-pipewire.sh [dll] [filter]`: the phase 4 acceptance test. For every
  saved capture it starts `os-pipewire` on a private null sink, plays the test signal into the
  right sink with `pw-play`, records the node's output with `pw-record` and compares it with the
  Windows loopback (same −130 dB threshold). `LATENCY=<n>` asks the graph for another quantum
  (on a machine with a higher `clock.min-quantum`, force it with `pw-metadata -n settings 0
  clock.force-quantum <n>` and set it back to 0 afterwards). The recording goes to memory
  (`$XDG_RUNTIME_DIR`) first: written straight to disk at 480 frames, `pw-record` dropped whole
  cycles on long tracks.
- `docs/reference/trace-shims.gdb`: gdb script that prints which risky Windows API shims the
  module actually calls during a run, with arguments, return values and the calling RVA (used to
  triage phase 3-bis); `docs/reference/trace-node.sh` runs it on the live PipeWire node with a
  fixed scenario (both sinks at once, volume and mute changes, end of session, second session).
- `os-pw-latency`: measures, inside the PipeWire graph, the latency the node adds (impulses into a
  sink, detected on the monitor of the sink the node plays to).

The recordings in `target/verify` are large and are not part of the repository. The synthetic-signal
cases (75 captures with their test signals) are published as a release asset:

```
gh release download verify-data-2026-11-03 -p verify-data.tar.gz
tar -xzf verify-data.tar.gz -C target
```

Some `info.txt` files reference their input with an absolute Windows path; the input files are in
`target/verify/signals/`.

The 3 real-audio cases (an excerpt of a Dolby Atmos test track, 5.1, 194 s; Movie Balanced, Game
performance, Custom 1 without virtualizer) contain non-redistributable material and are not
published anywhere. They live on the Windows capture machine and travel to Linux by hand as
`verify-reale.tar.gz` (exported with step W3 of section 12.13; the SHA-256 is written there). It
is extracted with `tar -xzf verify-reale.tar.gz -C target/verify` into three folders
`target/verify/reale-real-*`, each with its own copy of the input, and checked with the filter
`reale-` of `regress-linux.sh` and `verify-pipewire.sh` (steps L1-L3).

## Roadmap

| Phase | Goal | Status |
|---|---|---|
| 0 | Reconstruct the `IDAPModule2` interface and the call sequence of the Windows shell | done |
| 1 | Windows host that loads the module and renders WAV → binaural stereo, identical to Windows | done |
| 2 | Shared core `os-dap-core` behind a backend trait | done |
| 3 | Native PE loader for Linux (sections, relocations, imports, TLS, static CRT, `DllMain`) plus Rust shims for the imported Windows APIs | done |
| 3-bis | Shim correctness before real time: defects found in review after phase 3 (RTWorkQ, backend IIDs, `WindowsStringHasEmbeddedNull`, condition variables, `VirtualProtect`, named objects, thread exit), off the offline path or working by chance; see section 12.12 of the analysis | done: every fix has its unit test, Linux regression byte-identical to phase 3; on Windows the auxiliary IID is confirmed (`67b16434`, both backends) and the workspace builds; final Linux check done on 2026-11-04 (same `[qi]` line as Windows, 74/74 cases byte-identical to the baseline) |
| 4 | Real time on Linux: PipeWire node with a virtual 7.1(.4) sink for games and stereo routing for regular applications | in progress: `os-pipewire`; all 74 cases recorded from the node's output through PipeWire match the Windows references (−134.0…−149.0 dB, gain 0.000 dB), volume after the module, the node adds only the module's 512 frames of latency. Windows part done on 2026-11-05: build and tests green, `regress-host.ps1` 78/78 identical to the baseline, the 3 real-audio tracks exported. Linux checks done on 2026-11-05: the 3 real-audio tracks give −148.8 dB on Linux offline, through the real-time adapter and from the PipeWire node, whose output is bit-identical to the offline render; with the graph forced to 480 frames all 77 cases are bit-identical to the offline render. Remaining: Windows references for a profile change during playback and for two sessions separated by a pause, with an ETW trace of the real module, then their use in the node, and automatic routing (section 12.13, "Punto di ripresa") |
| 5 | Profile model and blob serializer, byte-identical to what Dolby Access writes | planned |
| 6 | User interface with the same controls as Dolby Access, on both platforms | planned |

Acceptance for phases 3-4: the same inputs and blobs give a residual of about −149 dB against the
Windows recordings in every configuration (−134…−147 dB for application stereo, as on Windows).

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
