//! Il grafo del nodo (doc sez. 12.13). Tre flussi PipeWire nello stesso gruppo di nodi, quindi
//! con lo stesso driver e lo stesso ciclo:
//!
//! ```text
//!   giochi ──► [sink 7.1(.4), Audio/Sink] ─┐ bed 0-16, −3 dB
//!                                          ├─► Streamer → modulo DAP → × volume ─► [uscita] ─► cuffie
//!   app ─────► [sink stereo, Audio/Sink] ──┘ bypass 17-18, 0 dB
//! ```
//!
//! Le due sorgenti finiscono nella stessa chiamata a EncodeAudioData, come su Windows (bed e
//! bypass nello stesso blocco). I sink sono flussi in ingresso `ASYNC`: il loro `process` non
//! consuma i buffer, segna solo il ciclo e fa partire l'uscita (`TRIGGER`), aspettando l'altro
//! sink solo se questo aveva audio (`Shared::capture_ran`). L'uscita copia i buffer di ciascun
//! sink in una coda sua e li restituisce subito, poi elabora **in ordine** il ciclo più vecchio
//! di ciascun sink (fino a [`HOLD`] restano da parte). È lo schema di `module-filter-chain` di
//! PipeWire, esteso a due ingressi. Un sink in ritardo viene elaborato al ciclo successivo,
//! senza perdere audio.
//!
//! Prima (fino al 12/11/2026) un sink aspettava l'altro se questo era passato nel ciclo
//! precedente, anche vuoto, e l'uscita prendeva solo il buffer più recente. Con un driver ALSA vero e un
//! client a bassa latenza (Brave, `pacat --latency-msec=5`), un sink in ritardo di un ciclo
//! lasciava quel ciclo senza uscita e senza buffer restituiti, e il nodo si fermava in
//! un'alternanza stabile: un ciclo di audio e uno di silenzio (doc 12.15, prova a mano).
//!
//! Sessioni: il modulo ha memoria (leveler), e Windows lo distrugge appena l'ultimo flusso
//! dell'endpoint viene rilasciato e ne crea uno nuovo al flusso successivo, qualunque sia la
//! pausa (traccia ETW, doc sez. 12.13 W6). Qui l'ultimo flusso che si chiude è l'ultimo
//! collegamento ai due sink che sparisce dal registro di PipeWire: il thread principale lo vede e
//! il ciclo successivo riconfigura il modulo e lo ferma (`Streamer::end_session`). Lo Streamer non
//! lo chiama finché i sink non ricevono audio (buffer senza `SPA_CHUNK_FLAG_EMPTY`).
//!
//! Cambio di profilo a caldo: con `--control <fifo>` il nodo legge comandi `profilo <cartella>
//! [frame <n>]`; il thread principale legge i blob e il ciclo successivo li consegna al modulo
//! sulla stessa istanza, con le notifiche di Windows (`HandleNotification`, doc sez. 12.13 W8).
//!
//! Volume: i sink annunciano il volume impostato dall'utente (`channelVolumes`) ma lasciano il
//! segnale intatto (`softVolumes` = 1, come fa filter-chain con `capture.volumes`); il nodo lo
//! applica dopo il modulo, dove Windows applica il volume dell'endpoint (doc 12.9: −3 dB →
//! Dolby → volume). I due sink condividono un solo volume, come un endpoint Windows.

use std::cell::{RefCell, UnsafeCell};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use os_dap_core::layout::{db_to_gain, Mode, Routing, MODULE_DELAY_FRAMES, SAMPLE_RATE};
use os_dap_core::props::{BlobSource, PROFILE_CHANGE_KEYS};
use os_dap_core::{Change, Source, Streamer};
use os_dap_linux::LinuxBackend;
use pipewire as pw;
use pw::spa;
use pw::spa::pod::{serialize::PodSerializer, Object, Pod, Property, PropertyFlags, Value, ValueArray};
use pw::spa::sys as spa_sys;
use pw::stream::{StreamBox, StreamFlags, StreamState};
use pw::sys as pw_sys;

use crate::Args;

const GAMES: usize = 0;
const APPS: usize = 1;
const MAX_CHANNELS: usize = 12;
/// Quantum massimo di PipeWire (`clock.max-quantum` di default): cicli più lunghi vengono
/// spezzati dallo Streamer.
const MAX_QUANTUM: usize = 8192;
/// Bit di validità accanto al numero di ciclo (u32) nei contatori atomici.
const VALID: u64 = 1 << 32;

const POSITIONS_71: [u32; 8] = [
    spa_sys::SPA_AUDIO_CHANNEL_FL,
    spa_sys::SPA_AUDIO_CHANNEL_FR,
    spa_sys::SPA_AUDIO_CHANNEL_FC,
    spa_sys::SPA_AUDIO_CHANNEL_LFE,
    spa_sys::SPA_AUDIO_CHANNEL_RL,
    spa_sys::SPA_AUDIO_CHANNEL_RR,
    spa_sys::SPA_AUDIO_CHANNEL_SL,
    spa_sys::SPA_AUDIO_CHANNEL_SR,
];
const POSITIONS_714_TOP: [u32; 4] = [
    spa_sys::SPA_AUDIO_CHANNEL_TFL,
    spa_sys::SPA_AUDIO_CHANNEL_TFR,
    spa_sys::SPA_AUDIO_CHANNEL_TRL,
    spa_sys::SPA_AUDIO_CHANNEL_TRR,
];
const POSITIONS_STEREO: [u32; 2] = [spa_sys::SPA_AUDIO_CHANNEL_FL, spa_sys::SPA_AUDIO_CHANNEL_FR];

/// Ordine dei canali dei sink: lo stesso dei WAV (WAVEFORMATEXTENSIBLE), quindi la stessa
/// mappa sui bed di `layout::channel_map` (RL/RR di PipeWire = BL/BR di Windows).
fn positions(channels: usize) -> Vec<u32> {
    match channels {
        2 => POSITIONS_STEREO.to_vec(),
        8 => POSITIONS_71.to_vec(),
        _ => POSITIONS_71.iter().chain(&POSITIONS_714_TOP).copied().collect(),
    }
}

fn position_names(channels: usize) -> &'static str {
    match channels {
        2 => "[ FL FR ]",
        8 => "[ FL FR FC LFE RL RR SL SR ]",
        _ => "[ FL FR FC LFE RL RR SL SR TFL TFR TRL TRR ]",
    }
}

/// Stato del thread real time: lo tocca solo il `process` dell'uscita.
struct Rt {
    streamer: Streamer<LinuxBackend>,
    routes: [Routing; 2],
    channels: [usize; 2],
    /// Ultima fine di sessione già eseguita (confronto con `Shared::sessions_ended`).
    ended: u64,
}

/// Cicli d'ingresso che l'uscita tiene da parte per ciascun sink, oltre a quello in elaborazione.
const HOLD: usize = 2;

/// Un ciclo d'ingresso di un sink, copiato dal buffer di PipeWire (che torna subito al sink:
/// tenerlo fra un ciclo e l'altro non è sicuro, PipeWire può liberarlo se il sink viene
/// sospeso o rinegoziato).
struct Slot {
    frames: usize,
    /// Canali con dati (gli altri sono vuoti: `SPA_CHUNK_FLAG_EMPTY`, cioè zeri).
    has: [bool; MAX_CHANNELS],
    /// Canale c in `data[c * MAX_QUANTUM ..][..frames]`.
    data: Vec<f32>,
}

/// Coda FIFO di cicli d'ingresso di un sink, preallocata all'avvio (niente allocazioni nel
/// thread real time). Un solo produttore (il `process` del sink, che copia e restituisce subito
/// il buffer: così il sink non resta mai senza buffer, anche se l'uscita salta un ciclo) e un
/// solo consumatore (il `process` dell'uscita), anche su thread diversi.
struct Fifo {
    slots: Box<[UnsafeCell<Slot>]>,
    /// Prossimo ciclo da elaborare e prossimo da scrivere (contatori, modulo `slots.len()`).
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl Fifo {
    fn new() -> Fifo {
        let slots = (0..HOLD + 1)
            .map(|_| UnsafeCell::new(Slot { frames: 0, has: [false; MAX_CHANNELS], data: vec![0.0; MAX_CHANNELS * MAX_QUANTUM] }))
            .collect();
        Fifo { slots, head: AtomicUsize::new(0), tail: AtomicUsize::new(0) }
    }

    fn len(&self) -> usize {
        self.tail.load(Ordering::Acquire).wrapping_sub(self.head.load(Ordering::Acquire))
    }

    /// Produttore: copia in coda un buffer di PipeWire con `channels` canali. Con la coda piena
    /// il ciclo si scarta e restituisce false.
    ///
    /// # Safety
    /// `b` è un buffer valido appena prelevato dal sink; un solo produttore per coda.
    unsafe fn push(&self, b: *mut pw_sys::pw_buffer, channels: usize) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail.wrapping_sub(self.head.load(Ordering::Acquire)) == self.slots.len() {
            return false;
        }
        // SAFETY: il consumatore non legge questo slot finché `tail` non lo pubblica.
        let slot = unsafe { &mut *self.slots[tail % self.slots.len()].get() };
        slot.frames = 0;
        slot.has = [false; MAX_CHANNELS];
        let sb = unsafe { (*b).buffer };
        let datas = unsafe { std::slice::from_raw_parts((*sb).datas, (*sb).n_datas as usize) };
        for (c, d) in datas.iter().take(channels.min(MAX_CHANNELS)).enumerate() {
            if d.data.is_null() || d.chunk.is_null() {
                continue;
            }
            let chunk = unsafe { &*d.chunk };
            if chunk.flags & spa_sys::SPA_CHUNK_FLAG_EMPTY as i32 != 0 {
                continue;
            }
            let off = (chunk.offset).min(d.maxsize) as usize;
            let size = (chunk.size as usize).min(d.maxsize as usize - off);
            let n = (size / 4).min(MAX_QUANTUM);
            let src = unsafe { std::slice::from_raw_parts((d.data as *const u8).add(off) as *const f32, n) };
            slot.data[c * MAX_QUANTUM..][..n].copy_from_slice(src);
            slot.has[c] = true;
            slot.frames = slot.frames.max(n);
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumatore: il ciclo più vecchio, da rilasciare con [`Fifo::release`] dopo l'uso.
    fn front(&self) -> Option<&Slot> {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: il produttore non riscrive questo slot finché `head` non avanza.
        Some(unsafe { &*self.slots[head % self.slots.len()].get() })
    }

    fn release(&self) {
        self.head.store(self.head.load(Ordering::Relaxed).wrapping_add(1), Ordering::Release);
    }
}

#[derive(Default)]
struct Stats {
    cycles: AtomicU64,
    quantum: AtomicUsize,
    present: [AtomicU64; 2],
    empty: [AtomicU64; 2],
    no_output: AtomicU64,
    errors: AtomicU64,
    trigger_failed: AtomicU64,
    max_process_ns: AtomicU64,
    resets: AtomicU64,
    changes: AtomicU64,
    idle: std::sync::atomic::AtomicBool,
    /// Esecuzioni del `process` di ciascun sink (con -v: un sink che salta cicli si vede qui).
    ran: [AtomicU64; 2],
    /// Durata dell'ultimo ciclo del grafo (frame, dalla posizione del driver) e frame in
    /// ingresso nell'ultimo ciclo elaborato.
    duration: AtomicUsize,
    in_frames: AtomicUsize,
    /// Sink che hanno aspettato l'altro, e partenze dell'uscita chieste dai sink.
    waits: AtomicU64,
    triggers: AtomicU64,
    /// Buffer d'ingresso scartati perché ne erano già da parte HOLD, e massimo tenuti da parte
    /// dopo un ciclo (0 o 1 normalmente).
    dropped: AtomicU64,
    held_max: AtomicUsize,
    /// Salti nel numero di ciclo visto da un sink fra due esecuzioni (cicli persi).
    skipped: AtomicU64,
    /// Cicli con i frame d'ingresso diversi da quelli chiesti dall'uscita (cambio di quantum),
    /// e frame d'uscita scartati dallo Streamer perché la coda era piena.
    mismatched: AtomicU64,
    overflows: AtomicU64,
}

/// Condiviso tra i callback dei tre flussi (thread real time e thread principale).
struct Shared {
    rt: UnsafeCell<Rt>,
    captures: [AtomicPtr<pw_sys::pw_stream>; 2],
    playback: AtomicPtr<pw_sys::pw_stream>,
    position: [AtomicPtr<spa_sys::spa_io_position>; 2],
    /// Ultimo ciclo in cui è passato ciascun sink, e ultimo ciclo che ha fatto partire l'uscita.
    seen: [AtomicU64; 2],
    triggered: AtomicU64,
    /// Cicli d'ingresso dei sink copiati ma non ancora elaborati.
    held: [Fifo; 2],
    /// Canali di ciascun sink.
    channels: [usize; 2],
    /// L'ultimo ciclo elaborato di ciascun sink aveva audio (lo scrive l'uscita, lo leggono i sink).
    audio: [std::sync::atomic::AtomicBool; 2],
    /// Guadagno dopo il modulo (f32): volume dei sink × --volume-db.
    volume: AtomicU32,
    latency: AtomicUsize,
    /// Sessioni finite (l'ultimo collegamento ai sink è sparito), contate dal thread principale.
    sessions_ended: AtomicU64,
    /// Cambi di profilo pronti, dal thread principale al real time (che non aspetta il lock).
    control: std::sync::Mutex<Vec<Change>>,
    stats: Stats,
}

// SAFETY: `rt` è usato solo dal process dell'uscita, che PipeWire non esegue mai in parallelo
// con se stesso; il resto è atomico. I puntatori ai flussi restano validi finché il main loop
// gira (i flussi vivono in `run`).
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl Shared {
    fn capture_ptr(&self, i: usize) -> *mut pw_sys::pw_stream {
        self.captures[i].load(Ordering::Acquire)
    }

    /// `process` di un sink: segna il ciclo e fa partire l'uscita, salvo aspettare l'altro sink
    /// se nel ciclo precedente l'altro è passato **e aveva audio**: così, quando una sola
    /// sorgente suona, il suo blocco arriva al modulo nello stesso ciclo (come su Windows), e
    /// il sink con l'audio non aspetta mai quello vuoto. Se l'atteso non arriva, il ciclo resta
    /// senza uscita (un ciclo di silenzio) ma i suoi dati restano in coda per il ciclo dopo.
    fn capture_ran(&self, idx: usize) {
        self.stats.ran[idx].fetch_add(1, Ordering::Relaxed);
        // I dati del sink si copiano subito in coda e il buffer torna al sink.
        let cap = self.capture_ptr(idx);
        loop {
            let b = unsafe { pw_sys::pw_stream_dequeue_buffer(cap) };
            if b.is_null() {
                break;
            }
            // SAFETY: buffer appena prelevato; questo process è l'unico produttore della coda.
            if !unsafe { self.held[idx].push(b, self.channels[idx]) } {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
            unsafe { pw_sys::pw_stream_queue_buffer(cap, b) };
        }
        let pos = self.position[idx].load(Ordering::Acquire);
        if pos.is_null() {
            self.trigger(idx);
            return;
        }
        let cycle = unsafe { (*pos).clock.cycle };
        self.stats.duration.store(unsafe { (*pos).clock.duration } as usize, Ordering::Relaxed);
        let me = cycle as u64 | VALID;
        let prev = self.seen[idx].swap(me, Ordering::AcqRel);
        if prev & VALID != 0 {
            let gap = cycle.wrapping_sub(prev as u32);
            if gap > 1 && gap < 1000 {
                self.stats.skipped.fetch_add((gap - 1) as u64, Ordering::Relaxed);
            }
        }
        let other = self.seen[1 - idx].load(Ordering::Acquire);
        let other_audio = self.audio[1 - idx].load(Ordering::Acquire);
        if other_audio && other & VALID != 0 && other != me && (other as u32).wrapping_add(1) == cycle {
            self.stats.waits.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.triggered.swap(me, Ordering::AcqRel) != me {
            self.stats.triggers.fetch_add(1, Ordering::Relaxed);
            self.trigger(idx);
        }
    }

    fn trigger(&self, idx: usize) {
        let res = unsafe { pw_sys::pw_stream_trigger_process(self.playback.load(Ordering::Acquire)) };
        if res < 0 {
            // L'uscita non è pronta (non ancora collegata): il ciclo di questo sink si scarta,
            // altrimenti la coda resterebbe piena.
            self.stats.trigger_failed.fetch_add(1, Ordering::Relaxed);
            if self.held[idx].front().is_some() {
                self.held[idx].release();
            }
        }
    }

    /// `process` dell'uscita: un ciclo intero del nodo.
    fn playback_process(&self) {
        let start = Instant::now();
        let pb = self.playback.load(Ordering::Acquire);
        // Per ogni sink il ciclo più vecchio non ancora elaborato (i successivi restano in coda).
        let inb: [Option<&Slot>; 2] = [self.held[0].front(), self.held[1].front()];
        for f in &self.held {
            self.stats.held_max.fetch_max(f.len().saturating_sub(1), Ordering::Relaxed);
        }
        let out = unsafe { pw_sys::pw_stream_dequeue_buffer(pb) };
        if !out.is_null() {
            // SAFETY: il buffer d'uscita e quelli d'ingresso restano nostri fino a queue_buffer.
            unsafe { self.render(out, &inb) };
            unsafe { pw_sys::pw_stream_queue_buffer(pb, out) };
        } else {
            self.stats.no_output.fetch_add(1, Ordering::Relaxed);
        }
        for (f, b) in self.held.iter().zip(inb) {
            if b.is_some() {
                f.release();
            }
        }
        let ns = start.elapsed().as_nanos() as u64;
        self.stats.max_process_ns.fetch_max(ns, Ordering::Relaxed);
        self.stats.cycles.fetch_add(1, Ordering::Relaxed);
    }

    unsafe fn render(&self, out: *mut pw_sys::pw_buffer, inb: &[Option<&Slot>; 2]) {
        // SAFETY: vedi `Shared`: solo questo callback usa `rt`.
        let rt = unsafe { &mut *self.rt.get() };
        let ended = self.sessions_ended.load(Ordering::Acquire);
        if ended != rt.ended {
            rt.ended = ended;
            if rt.streamer.end_session().is_err() {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Ok(mut queue) = self.control.try_lock() {
            for c in queue.drain(..) {
                if rt.streamer.change_properties(c).is_err() {
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let (sb, requested) = unsafe { ((*out).buffer, (*out).requested as usize) };
        let datas = unsafe { std::slice::from_raw_parts_mut((*sb).datas, (*sb).n_datas as usize) };
        if datas.len() < 2 || datas.iter().any(|d| d.data.is_null()) {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let room = datas.iter().map(|d| d.maxsize as usize / 4).min().unwrap_or(0);

        let mut chans: [[&[f32]; MAX_CHANNELS]; 2] = [[&[]; MAX_CHANNELS]; 2];
        let mut present = [false; 2];
        let mut in_frames = 0;
        for i in 0..2 {
            let Some(slot) = inb[i] else { continue };
            for (c, ch) in chans[i].iter_mut().enumerate().take(rt.channels[i]) {
                // Canale senza dati (nessun client collegato, o fermo): resta vuoto, cioè zeri.
                if !slot.has[c] {
                    self.stats.empty[i].fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                *ch = &slot.data[c * MAX_QUANTUM..][..slot.frames];
                in_frames = in_frames.max(slot.frames);
                // Come su Windows: gli oggetti di un flusso sono attivi finché il flusso suona.
                present[i] = true;
            }
            if present[i] {
                self.stats.present[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        for i in 0..2 {
            // Un sink senza ciclo nuovo (in ritardo) conserva lo stato: non si sa ancora se ha audio.
            if inb[i].is_some() {
                self.audio[i].store(present[i], Ordering::Release);
            }
        }
        let frames = if requested > 0 { requested } else { in_frames }.min(room);
        // Al modulo vanno i frame arrivati dai sink, non quelli chiesti dall'uscita: al cambio
        // di quantum per un ciclo sono diversi (doc 12.15, "Instabilità all'avvio delle sessioni").
        let input_frames = if present.iter().any(|&p| p) { in_frames } else { frames };
        if input_frames != frames {
            self.stats.mismatched.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.quantum.store(frames, Ordering::Relaxed);
        self.stats.in_frames.store(in_frames, Ordering::Relaxed);

        let all = [
            Source { routing: &rt.routes[GAMES], channels: &chans[GAMES][..rt.channels[GAMES]] },
            Source { routing: &rt.routes[APPS], channels: &chans[APPS][..rt.channels[APPS]] },
        ];
        let sources: &[Source] = match present {
            [true, true] => &all,
            [true, false] => &all[..1],
            [false, true] => &all[1..],
            [false, false] => &[],
        };
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let (l, r) = datas.split_at_mut(1);
        let left = unsafe { std::slice::from_raw_parts_mut(l[0].data as *mut f32, frames) };
        let right = unsafe { std::slice::from_raw_parts_mut(r[0].data as *mut f32, frames) };
        if rt.streamer.process_io(input_frames, sources, volume, [left, right]).is_err() {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            left.fill(0.0);
            right.fill(0.0);
        }
        self.latency.store(rt.streamer.latency(), Ordering::Relaxed);
        self.stats.resets.store(rt.streamer.resets(), Ordering::Relaxed);
        self.stats.changes.store(rt.streamer.changes(), Ordering::Relaxed);
        self.stats.overflows.store(rt.streamer.overflows(), Ordering::Relaxed);
        self.stats.idle.store(rt.streamer.is_idle(), Ordering::Relaxed);
        for d in datas.iter_mut().take(2) {
            let chunk = unsafe { &mut *d.chunk };
            chunk.offset = 0;
            chunk.size = (frames * 4) as u32;
            chunk.stride = 4;
        }
    }
}

/// Volume dei sink (thread principale): ne legge i cambiamenti, li neutralizza prima del modulo
/// e li pubblica come guadagno dopo il modulo.
struct Volumes {
    shared: Arc<Shared>,
    fixed: f32,
    master: f32,
    vols: [Vec<f32>; 2],
    mute: [bool; 2],
    verbose: bool,
}

#[derive(Default)]
struct PropsSeen {
    volumes: Option<Vec<f32>>,
    mute: Option<bool>,
    soft_volumes: Option<Vec<f32>>,
    soft_mute: Option<bool>,
}

fn parse_props(pod: &Pod) -> PropsSeen {
    let mut seen = PropsSeen::default();
    let Ok((_, Value::Object(obj))) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(pod.as_bytes()) else {
        return seen;
    };
    for p in obj.properties {
        match (p.key, p.value) {
            (spa_sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(v))) => seen.volumes = Some(v),
            (spa_sys::SPA_PROP_mute, Value::Bool(b)) => seen.mute = Some(b),
            (spa_sys::SPA_PROP_softVolumes, Value::ValueArray(ValueArray::Float(v))) => seen.soft_volumes = Some(v),
            (spa_sys::SPA_PROP_softMute, Value::Bool(b)) => seen.soft_mute = Some(b),
            _ => {}
        }
    }
    seen
}

fn serialize(value: Value) -> Vec<u8> {
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value).expect("pod").0.into_inner()
}

fn prop(key: u32, value: Value) -> Property {
    Property { key, flags: PropertyFlags::empty(), value }
}

impl Volumes {
    fn publish(&self) {
        self.shared.volume.store((self.master * self.fixed).to_bits(), Ordering::Relaxed);
    }

    /// Imposta sul sink `idx` volume e muto annunciati, con il volume software neutro.
    fn set_props(&self, idx: usize) {
        let n = self.vols[idx].len();
        let pod = serialize(Value::Object(Object {
            type_: spa_sys::SPA_TYPE_OBJECT_Props,
            id: spa_sys::SPA_PARAM_Props,
            properties: vec![
                prop(spa_sys::SPA_PROP_mute, Value::Bool(self.mute[idx])),
                prop(spa_sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(self.vols[idx].clone()))),
                prop(spa_sys::SPA_PROP_softMute, Value::Bool(false)),
                prop(spa_sys::SPA_PROP_softVolumes, Value::ValueArray(ValueArray::Float(vec![1.0; n]))),
            ],
        }));
        let res = unsafe {
            pw_sys::pw_stream_set_param(self.shared.capture_ptr(idx), spa_sys::SPA_PARAM_Props, pod.as_ptr() as *const spa_sys::spa_pod)
        };
        if res < 0 {
            eprintln!("impostazione del volume del sink {idx} fallita: {res}");
        }
    }

    fn on_props(&mut self, idx: usize, pod: &Pod) {
        let p = parse_props(pod);
        if p.volumes.is_none() && p.mute.is_none() {
            return;
        }
        if let Some(v) = p.volumes.filter(|v| !v.is_empty()) {
            self.vols[idx] = v;
        }
        if let Some(m) = p.mute {
            self.mute[idx] = m;
        }
        let neutral = p.soft_volumes.is_some_and(|s| s.iter().all(|&x| x == 1.0)) && p.soft_mute != Some(true);
        if !neutral {
            self.set_props(idx);
        }
        let master = if self.mute[idx] { 0.0 } else { self.vols[idx].iter().copied().fold(0.0, f32::max) };
        if master != self.master {
            self.master = master;
            self.publish();
            if self.verbose {
                eprintln!("volume dopo il modulo: {:.4} (sink {idx}{})", self.master * self.fixed, if self.mute[idx] { ", muto" } else { "" });
            }
            // Un solo volume per entrambi i sink, come l'endpoint di Windows.
            let o = 1 - idx;
            if !self.mute[idx] {
                self.vols[o] = vec![master; self.vols[o].len()];
            }
            self.mute[o] = self.mute[idx];
            self.set_props(o);
        }
    }
}

fn audio_format(channels: usize) -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32P);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(channels as u32);
    let mut pos = [0u32; spa_sys::SPA_AUDIO_MAX_CHANNELS as usize];
    for (d, s) in pos.iter_mut().zip(positions(channels)) {
        *d = s;
    }
    info.set_position(pos);
    serialize(Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Format,
        id: spa_sys::SPA_PARAM_EnumFormat,
        properties: info.into(),
    }))
}

fn process_latency(frames: usize) -> Vec<u8> {
    serialize(Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamProcessLatency,
        id: spa_sys::SPA_PARAM_ProcessLatency,
        properties: vec![prop(spa_sys::SPA_PARAM_PROCESS_LATENCY_rate, Value::Int(frames as i32))],
    }))
}

/// SIGINT e SIGTERM bloccati in ogni thread (quelli del modulo e di PipeWire nascono dopo e
/// ereditano la maschera): li raccoglie solo il main loop, con signalfd. Altrimenti un segnale
/// può finire in un altro thread e perdersi (per esempio con SIGINT ignorato, come per i
/// processi in background di uno script).
fn block_signals() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

pub fn run(args: Args) -> Result<()> {
    block_signals();
    let verbose = args.module.verbose;
    let engine = args.module.open::<LinuxBackend>()?;
    let block = engine.block_frames();
    let channels = [args.channels, 2];
    let routes = [Routing::new(args.channels as u16, false, args.gain_db)?, Routing::new(2, true, None)?];
    eprintln!(
        "modulo DAP configurato ({:?}, blocco {block} frame); giochi {} canali a {:+.1} dB, applicazioni nei bypass a 0 dB",
        args.module.mode,
        args.channels,
        20.0 * routes[GAMES].gain.log10()
    );
    let fixed = db_to_gain(args.volume_db);
    let mut streamer = Streamer::new(engine, MAX_QUANTUM);
    if args.idle_ms > 0 {
        streamer.set_idle_reset(Some(args.idle_ms as usize * SAMPLE_RATE as usize / 1000));
    }
    let shared = Arc::new(Shared {
        rt: UnsafeCell::new(Rt { streamer, routes, channels, ended: 0 }),
        captures: Default::default(),
        playback: AtomicPtr::new(std::ptr::null_mut()),
        position: Default::default(),
        seen: Default::default(),
        triggered: AtomicU64::new(0),
        held: [Fifo::new(), Fifo::new()],
        channels,
        audio: Default::default(),
        volume: AtomicU32::new(fixed.to_bits()),
        latency: AtomicUsize::new(0),
        sessions_ended: AtomicU64::new(0),
        control: std::sync::Mutex::new(Vec::with_capacity(8)),
        stats: Stats::default(),
    });
    // Lo Streamer parte fermo; senza questo `stato` direbbe "in moto" fino al primo ciclo.
    shared.stats.idle.store(true, Ordering::Relaxed);

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("main loop di PipeWire")?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("connessione a PipeWire")?;

    let group = format!("{}-{}", args.name, std::process::id());
    let latency = args.latency.map(|n| format!("{n}/{SAMPLE_RATE}"));
    let make = |suffix: &str, desc: &str, class: Option<&str>, ch: usize, extra: &[(&str, &str)]| -> Result<StreamBox<'_>> {
        let mut props = pw::properties::PropertiesBox::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::NODE_NAME, format!("{}-{suffix}", args.name));
        props.insert(*pw::keys::NODE_DESCRIPTION, desc);
        props.insert(*pw::keys::AUDIO_CHANNELS, ch.to_string());
        props.insert("audio.position", position_names(ch));
        props.insert("audio.rate", SAMPLE_RATE.to_string());
        props.insert("node.group", group.as_str());
        props.insert("node.link-group", group.as_str());
        if let Some(c) = class {
            props.insert(*pw::keys::MEDIA_CLASS, c);
        }
        if let Some(l) = &latency {
            props.insert(*pw::keys::NODE_LATENCY, l.as_str());
        }
        for (k, v) in extra {
            props.insert(*k, *v);
        }
        Ok(StreamBox::new(&core, &format!("{}-{suffix}", args.name), props)?)
    };
    let games_name = if args.channels == 12 { "7.1.4" } else { "7.1" };
    let games = make(games_name, &format!("open-surround {games_name} (giochi)"), Some("Audio/Sink"), args.channels, &[])?;
    let apps = make("stereo", "open-surround stereo (applicazioni)", Some("Audio/Sink"), 2, &[])?;
    let mut out_extra: Vec<(&str, &str)> = vec![(*pw::keys::MEDIA_CATEGORY, "Playback"), ("node.dont-remix", "true")];
    if !args.always_on {
        // Come le uscite di filter-chain: il collegamento non tiene sveglio il dispositivo.
        out_extra.push(("node.passive", "true"));
    }
    if let Some(t) = &args.target {
        out_extra.push(("target.object", t.as_str()));
    }
    let output = make("output", "open-surround uscita binaurale", None, 2, &out_extra)?;

    shared.captures[GAMES].store(games.as_raw_ptr(), Ordering::Release);
    shared.captures[APPS].store(apps.as_raw_ptr(), Ordering::Release);
    shared.playback.store(output.as_raw_ptr(), Ordering::Release);

    let volumes = Rc::new(RefCell::new(Volumes {
        shared: shared.clone(),
        fixed,
        master: 1.0,
        vols: [vec![1.0; args.channels], vec![1.0; 2]],
        mute: [false; 2],
        verbose,
    }));

    let mut listeners = Vec::new();
    for (idx, stream) in [(GAMES, &games), (APPS, &apps)] {
        let (sh_proc, sh_io, vol) = (shared.clone(), shared.clone(), volumes.clone());
        let name = stream.name();
        listeners.push(
            stream
                .add_local_listener_with_user_data(())
                .process(move |_, _| sh_proc.capture_ran(idx))
                .io_changed(move |_, _, id, area, _| {
                    if id == spa_sys::SPA_IO_Position {
                        sh_io.position[idx].store(area as *mut spa_sys::spa_io_position, Ordering::Release);
                    }
                })
                .param_changed(move |_, _, id, param| {
                    if id == spa_sys::SPA_PARAM_Props {
                        if let Some(p) = param {
                            vol.borrow_mut().on_props(idx, p);
                        }
                    }
                })
                .state_changed(move |_, _, old, new| state_changed(&name, old, new, verbose))
                .register()?,
        );
    }
    let sh_pb = shared.clone();
    let name = output.name();
    let playback_listener = output
        .add_local_listener_with_user_data(())
        .process(move |_, _| sh_pb.playback_process())
        .state_changed(move |_, _, old, new| state_changed(&name, old, new, verbose))
        .register()?;

    let flags_in = StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS | StreamFlags::from_bits_retain(pw_sys::pw_stream_flags_PW_STREAM_FLAG_ASYNC);
    for (stream, ch) in [(&games, args.channels), (&apps, 2)] {
        let fmt = audio_format(ch);
        let mut params = [Pod::from_bytes(&fmt).context("formato")?];
        stream.connect(spa::utils::Direction::Input, None, flags_in, &mut params)?;
    }
    let fmt = audio_format(2);
    let lat0 = process_latency(MODULE_DELAY_FRAMES);
    let mut params = [Pod::from_bytes(&fmt).context("formato")?, Pod::from_bytes(&lat0).context("latenza")?];
    output.connect(
        spa::utils::Direction::Output,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS | StreamFlags::TRIGGER,
        &mut params,
    )?;

    // Fine delle sessioni: i collegamenti che entrano nei due sink, dal registro.
    let registry = core.get_registry_rc()?;
    let links = Rc::new(RefCell::new(Links { input_node: HashMap::new(), count: 0 }));
    let sinks = [games.as_raw_ptr(), apps.as_raw_ptr()];
    let recount = {
        let shared = shared.clone();
        move |l: &mut Links| {
            let ids = sinks.map(|s| unsafe { pw_sys::pw_stream_get_node_id(s) });
            let count = l.input_node.values().filter(|n| ids.contains(n)).count();
            if count == 0 && l.count > 0 {
                let n = shared.sessions_ended.fetch_add(1, Ordering::AcqRel) + 1;
                if verbose {
                    eprintln!("[nodo] ultimo flusso chiuso: fine della sessione {n}, modulo riconfigurato");
                }
            } else if count > 0 && l.count == 0 && verbose {
                eprintln!("[nodo] flusso collegato ai sink ({count} collegamenti)");
            }
            l.count = count;
        }
    };
    let recount = Rc::new(recount);
    let (links_add, recount_add) = (links.clone(), recount.clone());
    let (links_rm, recount_rm) = (links.clone(), recount.clone());
    let _registry_listener = registry
        .add_listener_local()
        .global(move |g| {
            if g.type_ != pw::types::ObjectType::Link {
                return;
            }
            let Some(node) = g.props.and_then(|p| p.get("link.input.node")).and_then(|v| v.parse::<u32>().ok()) else {
                return;
            };
            let mut l = links_add.borrow_mut();
            l.input_node.insert(g.id, node);
            recount_add(&mut l);
        })
        .global_remove(move |id| {
            let mut l = links_rm.borrow_mut();
            if l.input_node.remove(&id).is_some() {
                recount_rm(&mut l);
            }
        })
        .register();

    // Instradamento automatico dei flussi che seguono il default (L6).
    let _router = args.route.then(|| {
        crate::route::Router::start(
            &registry,
            [format!("{}-{games_name}", args.name), format!("{}-stereo", args.name)],
            format!("{}-output", args.name),
            group.clone(),
            verbose,
        )
    });

    // Comandi di controllo (cambio di profilo a caldo): FIFO per gli script, socket con
    // risposta per l'interfaccia (doc 12.15, L2). Stesso stato per entrambi.
    let ctl = Rc::new(RefCell::new(Control::new(shared.clone(), &args.module.blobs, args.module.mode)));
    let _control = match &args.control {
        Some(path) => {
            let fifo = open_fifo(path)?;
            let ctl = ctl.clone();
            eprintln!("comandi da {}", path.display());
            Some(mainloop.loop_().add_io(fifo, spa::support::system::IoFlags::IN, move |f| ctl.borrow_mut().read(f)))
        }
        None => None,
    };
    let _socket = match &args.socket {
        Some(path) => {
            let socket = ControlSocket::bind(path)?;
            let listener = socket.listener.try_clone()?;
            let ctl = ctl.clone();
            eprintln!("comandi con risposta su {}", path.display());
            let source = mainloop.loop_().add_io(listener, spa::support::system::IoFlags::IN, move |l| serve(l, &ctl));
            Some((source, socket))
        }
        None => None,
    };

    // Ogni secondo: latenza dell'adattatore annunciata al grafo (ProcessLatency, così i client
    // la vedono nella latenza del sink) e, con -v, statistiche.
    let sh_t = shared.clone();
    let out_ptr = output.as_raw_ptr();
    let published = std::cell::Cell::new(usize::MAX);
    let last_cycles = std::cell::Cell::new(0u64);
    let timer = mainloop.loop_().add_timer(move |_| {
        let lat = sh_t.latency.load(Ordering::Relaxed);
        if sh_t.stats.cycles.load(Ordering::Relaxed) > 0 && lat != published.get() {
            published.set(lat);
            let total = lat + MODULE_DELAY_FRAMES;
            let pod = process_latency(total);
            let mut p = [pod.as_ptr() as *const spa_sys::spa_pod];
            unsafe { pw_sys::pw_stream_update_params(out_ptr, p.as_mut_ptr(), 1) };
            let ms = |f: usize| f as f64 * 1000.0 / SAMPLE_RATE as f64;
            eprintln!(
                "latenza dell'adattatore: {lat} frame ({:.2} ms); annunciata al grafo con il modulo: {total} frame ({:.2} ms)",
                ms(lat),
                ms(total)
            );
        }
        if verbose {
            let s = &sh_t.stats;
            let cycles = s.cycles.load(Ordering::Relaxed);
            eprintln!(
                "[nodo] cicli {} (+{}), quantum {} (ciclo del grafo {}, ingresso {}), sink eseguiti {}/{} (attese {}, partenze {}, cicli saltati {}, cicli da parte max {}, scartati {}, ingresso diverso dall'uscita {}, uscita scartata {}), giochi {} (vuoti {}), app {} (vuoti {}), modulo {}, riconfigurazioni {}, cambi di profilo {}, senza buffer d'uscita {}, errori {}, trigger falliti {}, process max {:.3} ms",
                cycles,
                cycles - last_cycles.get(),
                s.quantum.load(Ordering::Relaxed),
                s.duration.load(Ordering::Relaxed),
                s.in_frames.load(Ordering::Relaxed),
                s.ran[GAMES].load(Ordering::Relaxed),
                s.ran[APPS].load(Ordering::Relaxed),
                s.waits.load(Ordering::Relaxed),
                s.triggers.load(Ordering::Relaxed),
                s.skipped.load(Ordering::Relaxed),
                s.held_max.load(Ordering::Relaxed),
                s.dropped.load(Ordering::Relaxed),
                s.mismatched.load(Ordering::Relaxed),
                s.overflows.load(Ordering::Relaxed),
                s.present[GAMES].load(Ordering::Relaxed),
                s.empty[GAMES].load(Ordering::Relaxed),
                s.present[APPS].load(Ordering::Relaxed),
                s.empty[APPS].load(Ordering::Relaxed),
                if s.idle.load(Ordering::Relaxed) { "fermo" } else { "in moto" },
                s.resets.load(Ordering::Relaxed),
                s.changes.load(Ordering::Relaxed),
                s.no_output.load(Ordering::Relaxed),
                s.errors.load(Ordering::Relaxed),
                s.trigger_failed.load(Ordering::Relaxed),
                s.max_process_ns.swap(0, Ordering::Relaxed) as f64 / 1e6,
            );
            last_cycles.set(cycles);
        }
    });
    timer.update_timer(Some(Duration::from_millis(500)), Some(Duration::from_secs(1))).into_result()?;

    let ml = mainloop.clone();
    let _sigint = mainloop.loop_().add_signal_local(pw::loop_::Signal::INT, move || ml.quit());
    let ml = mainloop.clone();
    let _sigterm = mainloop.loop_().add_signal_local(pw::loop_::Signal::TERM, move || ml.quit());

    eprintln!(
        "nodo pronto: sink {}-{games_name} e {}-stereo, uscita {}-output{}",
        args.name,
        args.name,
        args.name,
        args.target.as_ref().map(|t| format!(" → {t}")).unwrap_or_default()
    );
    mainloop.run();

    eprintln!("arresto");
    for s in [&games, &apps, &output] {
        let _ = s.disconnect();
    }
    drop((listeners, playback_listener));
    if shared.stats.errors.load(Ordering::Relaxed) > 0 {
        bail!("{} cicli con errori di elaborazione", shared.stats.errors.load(Ordering::Relaxed));
    }
    Ok(())
}

/// Collegamenti del grafo per nodo d'ingresso, e quanti entrano nei sink del nodo.
struct Links {
    input_node: HashMap<u32, u32>,
    count: usize,
}

/// Crea (se manca) e apre il FIFO dei comandi. In lettura e scrittura: senza scrittori aperti un
/// FIFO in sola lettura darebbe fine file a ogni giro.
fn open_fifo(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    match std::fs::metadata(path) {
        Ok(m) if m.file_type().is_fifo() => {}
        Ok(_) => bail!("{}: esiste e non è un FIFO", path.display()),
        Err(_) => {
            let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
                bail!("mkfifo {}: {}", path.display(), std::io::Error::last_os_error());
            }
        }
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("apertura di {}", path.display()))
}

/// Comandi dal FIFO e dal socket, una riga ciascuno:
///   profilo <cartella|stato> [frame <n>]   blob della cartella di cattura, o generati dal
///                                     modello dei profili per lo stato (`film:warm`, …), come
///                                     profilo nuovo, sulla stessa istanza del modulo; con
///                                     `frame` dal blocco che inizia al frame n della sessione
///                                     (per la verifica)
///   stato                             profilo attivo, stato del modulo, cambi consegnati,
///                                     versione e commit del nodo
/// Sul socket ogni comando ha una riga di risposta, `ok …` oppure `errore …`.
struct Control {
    shared: Arc<Shared>,
    blobs: BlobSource,
    mode: Mode,
    pending: String,
    /// Il profilo attivo come è stato dato (stato del modello o cartella di cattura).
    current: String,
    /// Cambi accettati (consegnati al modulo o in attesa).
    accepted: u64,
}

impl Control {
    fn new(shared: Arc<Shared>, blobs: &BlobSource, mode: Mode) -> Control {
        let current = match &blobs.profile {
            Some(p) => p.clone(),
            None => blobs.captures.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(","),
        };
        Control { shared, blobs: blobs.clone(), mode, pending: String::new(), current, accepted: 0 }
    }

    fn read(&mut self, f: &mut std::fs::File) {
        let mut buf = [0u8; 4096];
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                break;
            }
            self.pending.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        while let Some(i) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=i).collect();
            if let Err(e) = self.command(line.trim()) {
                eprintln!("comando «{}»: {e:#}", line.trim());
            }
        }
    }

    /// Esegue un comando; il testo restituito è la risposta dopo `ok`.
    fn command(&mut self, line: &str) -> Result<String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [] => Ok(String::new()),
            ["stato"] => {
                let s = &self.shared.stats;
                Ok(format!(
                    "profilo={} modulo={} cambi={} consegnati={} fini-sessione={} versione={} commit={}",
                    self.current,
                    if s.idle.load(Ordering::Relaxed) { "fermo" } else { "in-moto" },
                    self.accepted,
                    s.changes.load(Ordering::Relaxed),
                    self.shared.sessions_ended.load(Ordering::Relaxed),
                    os_profiles::version::VERSION,
                    os_profiles::version::short_commit(),
                ))
            }
            ["profilo", target, rest @ ..] => {
                let at = match rest {
                    [] => None,
                    ["frame", n] => Some(n.parse::<u64>().context("frame")?),
                    _ => bail!("uso: profilo <cartella|stato> [frame <n>]"),
                };
                let blobs = self.blobs.with_target(target);
                let values = blobs.fx_values(self.mode)?;
                // Le sessioni successive ripartono da questi blob (il property store è cambiato).
                self.blobs = blobs;
                let change = Change { values, notify: PROFILE_CHANGE_KEYS.to_vec(), at };
                self.shared.control.lock().unwrap_or_else(|e| e.into_inner()).push(change);
                eprintln!("profilo {target}{}: in consegna al modulo", at.map(|f| format!(" dal frame {f} della sessione")).unwrap_or_default());
                self.current = target.to_string();
                self.accepted += 1;
                Ok(format!("profilo {target}"))
            }
            _ => bail!("comando sconosciuto (profilo <cartella|stato> [frame <n>], stato)"),
        }
    }
}

/// Socket di controllo: crea la cartella (0700), toglie un socket rimasto da un nodo che non
/// c'è più e lo cancella alla fine.
struct ControlSocket {
    path: std::path::PathBuf,
    listener: std::os::unix::net::UnixListener,
}

impl ControlSocket {
    fn bind(path: &Path) -> Result<ControlSocket> {
        use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
        use std::os::unix::net::{UnixListener, UnixStream};
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty() && !d.exists()) {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creazione di {}", dir.display()))?;
        }
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_socket() => {
                if UnixStream::connect(path).is_ok() {
                    bail!("{}: c'è già un nodo in ascolto", path.display());
                }
                std::fs::remove_file(path).with_context(|| format!("rimozione di {}", path.display()))?;
            }
            Ok(_) => bail!("{}: esiste e non è un socket", path.display()),
            Err(_) => {}
        }
        let listener = UnixListener::bind(path).with_context(|| format!("socket {}", path.display()))?;
        listener.set_nonblocking(true)?;
        Ok(ControlSocket { path: path.to_path_buf(), listener })
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Serve le connessioni in attesa: una riga di comando, una riga di risposta, chiusura. Il
/// client ha [`CLIENT_TIMEOUT`] per scrivere la riga, così un client fermo non blocca a lungo
/// il main loop (che gestisce volume e sessioni; il tempo reale è su un altro thread).
fn serve(listener: &mut std::os::unix::net::UnixListener, ctl: &Rc<RefCell<Control>>) {
    use std::io::{BufRead, BufReader, Write};
    while let Ok((stream, _)) = listener.accept() {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
        let mut line = String::new();
        let reply = match BufReader::new((&stream).take(4096)).read_line(&mut line) {
            Ok(0) => continue,
            Ok(_) => match ctl.borrow_mut().command(line.trim()) {
                Ok(s) if s.is_empty() => "ok".to_string(),
                Ok(s) => format!("ok {s}"),
                Err(e) => {
                    eprintln!("comando «{}»: {e:#}", line.trim());
                    format!("errore {}", format!("{e:#}").replace('\n', " "))
                }
            },
            Err(e) => format!("errore lettura del comando: {e}"),
        };
        let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
    }
}

const CLIENT_TIMEOUT: Duration = Duration::from_millis(500);

fn state_changed(name: &str, old: StreamState, new: StreamState, verbose: bool) {
    match &new {
        StreamState::Error(e) => eprintln!("{name}: errore: {e}"),
        _ if verbose => eprintln!("{name}: {old:?} → {new:?}"),
        _ => {}
    }
}
