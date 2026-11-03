//! ref-capture: registra il riferimento "Windows vero" per verificare os-dap-host.
//!
//! Riproduce un WAV multicanale con ISpatialAudioClient (canali come oggetti statici, come fa un
//! gioco) sull'endpoint scelto, dove Windows applica il formato spaziale attivo (Dolby Atmos for
//! Headphones), e registra in contemporanea il loopback dello stesso endpoint. Salva anche i blob
//! Dolby dell'endpoint letti in quel momento, così os-dap-host può usare gli stessi parametri.
//!
//! Uso:
//!   ref-capture --list
//!   ref-capture [--wasapi] --endpoint <hex8|parte del nome> <ingresso.wav> <cartella-uscita>
//!     --wasapi   riproduce come un'applicazione qualsiasi (stream WASAPI condiviso nel formato di
//!                mix dell'endpoint) invece che con oggetti statici ISpatialAudioClient|parte del nome> <ingresso.wav> <cartella-uscita>
//!
//! In <cartella-uscita>: loopback.wav (formato di mix dell'endpoint), i blob
//! <hex8>-<pid>.bin / <hex8>-<fmtid8>-<pid>.bin (stesso formato di capture_asar.ps1) e info.txt.

#[cfg(not(windows))]
fn main() {
    eprintln!("ref-capture funziona solo su Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    win::main()
}

#[cfg(windows)]
mod win {
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use anyhow::{anyhow, bail, Context, Result};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
    use windows::Win32::System::Com::StructuredStorage::{PropVariantToStringAlloc, PROPVARIANT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_BINARY,
        REG_VALUE_TYPE,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::Variant::VT_BLOB;
    use windows_core::{HSTRING, PWSTR};

    const SAMPLE_RATE: u32 = 48_000;
    const DOLBY_CONTEXT: &str = "{45da5c30-2837-4ac4-b1e2-50acc3865974}";
    const DAPVR_PSET: &str = "{1b4dab55-b1fb-4d8c-8317-f2d4a96efbb8}";

    struct Endpoint {
        device: IMMDevice,
        guid: String,
        name: String,
    }

    fn endpoints() -> Result<Vec<Endpoint>> {
        unsafe {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let coll = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut out = Vec::new();
            for i in 0..coll.GetCount()? {
                let device = coll.Item(i)?;
                let id = pwstr(device.GetId()?);
                // Formato: {0.0.0.00000000}.{guid}
                let guid = id.rsplit('.').next().unwrap_or(&id).to_lowercase();
                let store = device.OpenPropertyStore(STGM_READ)?;
                let pv = store.GetValue(&PKEY_Device_FriendlyName)?;
                let name = PropVariantToStringAlloc(&pv).map(|p| pwstr(p)).unwrap_or_default();
                out.push(Endpoint { device, guid, name });
            }
            Ok(out)
        }
    }

    unsafe fn pwstr(p: PWSTR) -> String {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }

    fn short(guid: &str) -> &str {
        guid.trim_start_matches('{').get(..8).unwrap_or(guid)
    }

    /// Descrive il formato di mix e lo stato dell'audio spaziale di un endpoint.
    fn describe(ep: &Endpoint) -> String {
        let mut s = String::new();
        unsafe {
            if let Ok(vol) = ep.device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                let db = vol.GetMasterVolumeLevel().unwrap_or(f32::NAN);
                let scalar = vol.GetMasterVolumeLevelScalar().unwrap_or(f32::NAN);
                let mute = vol.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                let _ = write!(s, "volume {db:.4} dB (scalare {scalar:.4}{}), ", if mute { ", muto" } else { "" });
            }
            if let Ok(client) = ep.device.Activate::<IAudioClient>(CLSCTX_ALL, None) {
                if let Ok(mix) = client.GetMixFormat() {
                    let m = &*mix;
                    let (ch, rate, bits) = (m.nChannels, m.nSamplesPerSec, m.wBitsPerSample);
                    let _ = write!(s, "mix {ch} ch {rate} Hz {bits} bit");
                    CoTaskMemFree(Some(mix as *const _));
                }
            }
            match ep.device.Activate::<ISpatialAudioClient>(CLSCTX_ALL, None) {
                Ok(sp) => {
                    let dynamic = sp.GetMaxDynamicObjectCount().unwrap_or(0);
                    let mask = sp.GetNativeStaticObjectTypeMask().map(|m| m.0).unwrap_or(0);
                    let _ = write!(s, ", spaziale: max oggetti dinamici {dynamic}, maschera statica nativa {mask:#x}");
                    if dynamic == 0 {
                        s.push_str(" (nessun formato spaziale attivo)");
                    } else {
                        // Posizioni dei 17 bed (AudioObjectType 0x2..0x20000), coordinate Windows:
                        // metri, x destra, y su, z indietro.
                        for bit in 1..18 {
                            let (mut x, mut y, mut z) = (0.0f32, 0.0f32, 0.0f32);
                            if sp.GetStaticObjectPosition(AudioObjectType(1 << bit), &mut x, &mut y, &mut z).is_ok() {
                                let _ = write!(s, "\n      tipo {:#07x}: x {x:+.4} y {y:+.4} z {z:+.4}", 1 << bit);
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = write!(s, ", spaziale non disponibile ({:#010x})", e.code().0 as u32);
                }
            }
        }
        s
    }

    /// Tipi di oggetto statico nell'ordine dei canali WAVEFORMATEXTENSIBLE.
    fn channel_types(channels: u16) -> Result<Vec<AudioObjectType>> {
        Ok(match channels {
            2 => vec![AudioObjectType_FrontLeft, AudioObjectType_FrontRight],
            6 => vec![
                AudioObjectType_FrontLeft, AudioObjectType_FrontRight, AudioObjectType_FrontCenter,
                AudioObjectType_LowFrequency, AudioObjectType_SideLeft, AudioObjectType_SideRight,
            ],
            8 | 12 => {
                let mut v = vec![
                    AudioObjectType_FrontLeft, AudioObjectType_FrontRight, AudioObjectType_FrontCenter,
                    AudioObjectType_LowFrequency, AudioObjectType_BackLeft, AudioObjectType_BackRight,
                    AudioObjectType_SideLeft, AudioObjectType_SideRight,
                ];
                if channels == 12 {
                    v.extend([
                        AudioObjectType_TopFrontLeft, AudioObjectType_TopFrontRight,
                        AudioObjectType_TopBackLeft, AudioObjectType_TopBackRight,
                    ]);
                }
                v
            }
            n => bail!("{n} canali non supportati (2, 6, 8, 12)"),
        })
    }

    fn read_wav(path: &Path) -> Result<(u16, Vec<f32>)> {
        let mut r = hound::WavReader::open(path).with_context(|| format!("apertura {}", path.display()))?;
        let spec = r.spec();
        if spec.sample_rate != SAMPLE_RATE {
            bail!("serve un WAV a 48 kHz");
        }
        let samples = match spec.sample_format {
            hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
                r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<Vec<_>, _>>()?
            }
        };
        Ok((spec.channels, samples))
    }

    /// Copia i valori del contesto Dolby dell'endpoint (come capture_asar.ps1).
    fn snapshot_dolby(guid: &str, dir: &Path) -> Result<Vec<String>> {
        let path = format!(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{guid}\FxProperties\{DOLBY_CONTEXT}\User"
        );
        let mut saved = Vec::new();
        unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(path.as_str()), None, KEY_READ, &mut key).is_err() {
                return Ok(saved);
            }
            for i in 0.. {
                let mut name = [0u16; 256];
                let mut name_len = name.len() as u32;
                let mut kind = REG_VALUE_TYPE::default();
                let mut data = vec![0u8; 64 * 1024];
                let mut data_len = data.len() as u32;
                let rc = RegEnumValueW(
                    key, i, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None,
                    Some(&mut kind as *mut REG_VALUE_TYPE as *mut u32), Some(data.as_mut_ptr()), Some(&mut data_len),
                );
                if rc.is_err() {
                    break;
                }
                let name = String::from_utf16_lossy(&name[..name_len as usize]).to_lowercase();
                if kind != REG_BINARY {
                    continue;
                }
                let Some((fmtid, pid)) = name.rsplit_once(',') else { continue };
                let file = if fmtid == DAPVR_PSET {
                    format!("{}-{pid}.bin", short(guid))
                } else {
                    format!("{}-{}-{pid}.bin", short(guid), short(fmtid))
                };
                std::fs::write(dir.join(&file), &data[..data_len as usize])?;
                saved.push(file);
            }
            let _ = RegCloseKey(key);
        }
        saved.sort();
        Ok(saved)
    }

    fn record(ep: &Endpoint, input: &Path, out_dir: &Path, wasapi: bool) -> Result<()> {
        let (channels, samples) = read_wav(input)?;
        let types = channel_types(channels)?;
        let frames_in = samples.len() / channels as usize;
        std::fs::create_dir_all(out_dir)?;
        let blobs = snapshot_dolby(&ep.guid, out_dir)?;

        unsafe {
            let spatial: ISpatialAudioClient = ep.device.Activate(CLSCTX_ALL, None).context("ISpatialAudioClient")?;
            if spatial.GetMaxDynamicObjectCount()? == 0 {
                bail!("sull'endpoint non è attivo un formato spaziale (Dolby Atmos for Headphones)");
            }
            let event = CreateEventW(None, false, false, None)?;
            let source = if wasapi {
                Source::Wasapi(open_wasapi(ep, channels, event)?)
            } else {
                Source::Spatial(open_spatial(&spatial, &types, event)?)
            };
            record_loop(ep, source, event, channels, &samples, frames_in, input, out_dir, &blobs)
        }
    }

    enum Source {
        /// Canali come oggetti statici (come un gioco con Windows Spatial Audio).
        Spatial((ISpatialAudioObjectRenderStream, Vec<ISpatialAudioObject>)),
        /// Stream WASAPI condiviso normale (come un'applicazione qualsiasi).
        Wasapi((IAudioClient, IAudioRenderClient, u32)),
    }

    unsafe fn open_wasapi(ep: &Endpoint, channels: u16, event: HANDLE) -> Result<(IAudioClient, IAudioRenderClient, u32)> {
        let client: IAudioClient = ep.device.Activate(CLSCTX_ALL, None)?;
        let mix = client.GetMixFormat()?;
        let (mix_ch, mix_rate) = ((*mix).nChannels, (*mix).nSamplesPerSec);
        if mix_ch != channels || mix_rate != SAMPLE_RATE {
            CoTaskMemFree(Some(mix as *const _));
            bail!("in modalità WASAPI il WAV deve avere il formato di mix dell'endpoint ({mix_ch} canali, {mix_rate} Hz)");
        }
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0, 0, mix, None)?;
        CoTaskMemFree(Some(mix as *const _));
        client.SetEventHandle(event)?;
        let render: IAudioRenderClient = client.GetService()?;
        let size = client.GetBufferSize()?;
        Ok((client, render, size))
    }

    unsafe fn open_spatial(
        spatial: &ISpatialAudioClient,
        types: &[AudioObjectType],
        event: HANDLE,
    ) -> Result<(ISpatialAudioObjectRenderStream, Vec<ISpatialAudioObject>)> {
        {
            let fmt = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
                nChannels: 1,
                nSamplesPerSec: SAMPLE_RATE,
                nAvgBytesPerSec: SAMPLE_RATE * 4,
                nBlockAlign: 4,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            spatial.IsAudioObjectFormatSupported(&fmt).context("formato oggetti float 48 kHz non supportato")?;
            let mask = types.iter().fold(0, |m, t| m | t.0);
            let params = SpatialAudioObjectRenderStreamActivationParams {
                ObjectFormat: &fmt,
                StaticObjectTypeMask: AudioObjectType(mask),
                MinDynamicObjectCount: 0,
                MaxDynamicObjectCount: 0,
                Category: AudioCategory_Media,
                EventHandle: event,
                NotifyObject: std::mem::ManuallyDrop::new(None),
            };
            // pv punta a memoria nostra: il Drop di PROPVARIANT (PropVariantClear) la libererebbe
            // con CoTaskMemFree e corromperebbe l'heap, quindi non va mai distrutto.
            let mut pv = std::mem::ManuallyDrop::new(PROPVARIANT::default());
            {
                let inner = &mut pv.Anonymous.Anonymous;
                inner.vt = VT_BLOB;
                inner.Anonymous.blob.cbSize = std::mem::size_of_val(&params) as u32;
                inner.Anonymous.blob.pBlobData = &params as *const _ as *mut u8;
            }
            let stream: ISpatialAudioObjectRenderStream =
                spatial.ActivateSpatialAudioStream(&*pv).context("ActivateSpatialAudioStream")?;
            let objects = types
                .iter()
                .map(|t| stream.ActivateSpatialAudioObject(*t))
                .collect::<windows_core::Result<Vec<_>>>()
                .context("ActivateSpatialAudioObject")?;
            Ok((stream, objects))
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn record_loop(
        ep: &Endpoint,
        source: Source,
        event: HANDLE,
        channels: u16,
        samples: &[f32],
        frames_in: usize,
        input: &Path,
        out_dir: &Path,
        blobs: &[String],
    ) -> Result<()> {
        {
            // Loopback dello stesso endpoint.
            let lb: IAudioClient = ep.device.Activate(CLSCTX_ALL, None)?;
            let mix = lb.GetMixFormat()?;
            let mix_ch = (*mix).nChannels;
            let mix_rate = (*mix).nSamplesPerSec;
            let is_float = match (*mix).wFormatTag as u32 {
                WAVE_FORMAT_IEEE_FLOAT => true,
                WAVE_FORMAT_EXTENSIBLE => {
                    let ext = &*(mix as *const WAVEFORMATEXTENSIBLE);
                    let sub = ext.SubFormat;
                    sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                }
                _ => false,
            } && (*mix).wBitsPerSample == 32;
            if !is_float {
                bail!("formato di mix dell'endpoint non float32");
            }
            lb.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 20_000_000, 0, mix, None)?;
            CoTaskMemFree(Some(mix as *const _));
            let capture: IAudioCaptureClient = lb.GetService()?;
            if mix_rate != SAMPLE_RATE {
                eprintln!("attenzione: l'endpoint mixa a {mix_rate} Hz, Windows ricampiona il segnale Dolby");
            }

            let mut recorded: Vec<f32> = Vec::new();
            let drain = |recorded: &mut Vec<f32>| -> Result<()> {
                loop {
                    let n = capture.GetNextPacketSize()?;
                    if n == 0 {
                        return Ok(());
                    }
                    let mut data = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    let len = frames as usize * mix_ch as usize;
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                        recorded.extend(std::iter::repeat_n(0.0, len));
                    } else {
                        recorded.extend_from_slice(std::slice::from_raw_parts(data as *const f32, len));
                    }
                    capture.ReleaseBuffer(frames)?;
                }
            };

            let ch = channels as usize;
            let sample = |f: usize, c: usize| if f < frames_in { samples[f * ch + c] } else { 0.0 };
            lb.Start()?;
            match &source {
                Source::Spatial((stream, _)) => stream.Start()?,
                Source::Wasapi((client, _, _)) => client.Start()?,
            }
            let started = Instant::now();
            let tail = SAMPLE_RATE as usize; // 1 s di coda per raccogliere la risposta completa
            let mut pos = 0usize;
            while pos < frames_in + tail {
                if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                    bail!("timeout in attesa del motore audio");
                }
                match &source {
                    Source::Spatial((stream, objects)) => {
                        let mut avail = 0u32;
                        let mut frames = 0u32;
                        stream.BeginUpdatingAudioObjects(&mut avail, &mut frames)?;
                        for (c, obj) in objects.iter().enumerate() {
                            let mut buf = std::ptr::null_mut();
                            let mut len = 0u32;
                            obj.GetBuffer(&mut buf, &mut len)?;
                            let out = std::slice::from_raw_parts_mut(buf as *mut f32, frames as usize);
                            for (i, s) in out.iter_mut().enumerate() {
                                *s = sample(pos + i, c);
                            }
                        }
                        stream.EndUpdatingAudioObjects()?;
                        pos += frames as usize;
                    }
                    Source::Wasapi((client, render, size)) => {
                        let frames = size - client.GetCurrentPadding()?;
                        if frames > 0 {
                            let buf = render.GetBuffer(frames)?;
                            let out = std::slice::from_raw_parts_mut(buf as *mut f32, frames as usize * ch);
                            for (i, s) in out.iter_mut().enumerate() {
                                *s = sample(pos + i / ch, i % ch);
                            }
                            render.ReleaseBuffer(frames, 0)?;
                            pos += frames as usize;
                        }
                    }
                }
                drain(&mut recorded)?;
            }
            // Lascia uscire quanto ancora in coda nel motore.
            while started.elapsed().as_secs_f64() < (frames_in + 2 * tail) as f64 / SAMPLE_RATE as f64 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                drain(&mut recorded)?;
            }
            match &source {
                Source::Spatial((stream, _)) => stream.Stop()?,
                Source::Wasapi((client, _, _)) => client.Stop()?,
            }
            lb.Stop()?;
            let _ = CloseHandle(event);

            let mut w = hound::WavWriter::create(
                out_dir.join("loopback.wav"),
                hound::WavSpec { channels: mix_ch, sample_rate: mix_rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float },
            )?;
            for s in &recorded {
                w.write_sample(*s)?;
            }
            w.finalize()?;

            let peak = recorded.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let info = format!(
                "endpoint: {} {}\n{}\ningresso: {} ({} canali, {} frame, {})\nloopback: {} canali, {} Hz, {} frame, picco {:.1} dBFS\nblob Dolby: {}\n",
                ep.guid, ep.name, describe(ep), input.display(), channels, frames_in,
                if matches!(source, Source::Wasapi(_)) { "stream WASAPI" } else { "oggetti statici" },
                mix_ch, mix_rate, recorded.len() / mix_ch as usize,
                20.0 * (peak.max(1e-10) as f64).log10(),
                if blobs.is_empty() { "nessuno".into() } else { blobs.join(", ") },
            );
            std::fs::write(out_dir.join("info.txt"), &info)?;
            eprint!("{info}");
        }
        Ok(())
    }

    pub fn main() -> Result<()> {
        let mut args: Vec<String> = std::env::args().skip(1).collect();
        let wasapi = args.iter().position(|a| a == "--wasapi").map(|i| args.remove(i)).is_some();
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        let eps = endpoints()?;
        match args.as_slice() {
            [flag] if flag == "--list" => {
                for ep in &eps {
                    println!("{} {}\n    {}", short(&ep.guid), ep.name, describe(ep));
                }
                Ok(())
            }
            [flag, sel, input, out] if flag == "--endpoint" => {
                let sel_l = sel.to_lowercase();
                let ep = eps
                    .iter()
                    .find(|e| short(&e.guid) == sel_l || e.name.to_lowercase().contains(&sel_l))
                    .ok_or_else(|| anyhow!("endpoint '{sel}' non trovato (vedi --list)"))?;
                eprintln!("endpoint: {} {}", short(&ep.guid), ep.name);
                record(ep, Path::new(input), &PathBuf::from(out), wasapi)
            }
            _ => bail!("uso: ref-capture --list | [--wasapi] --endpoint <hex8|nome> <ingresso.wav> <cartella-uscita>"),
        }
    }
}
