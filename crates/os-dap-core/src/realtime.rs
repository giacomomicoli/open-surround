//! Elaborazione continua (fase 4, doc sez. 12.13): il grafo audio consegna blocchi di lunghezza
//! qualsiasi (il quantum di PipeWire, anche non multiplo di 256 e variabile nel tempo), il modulo
//! accetta solo blocchi fissi di `block_frames` (multipli di 256). [`Streamer`] fa da adattatore
//! senza cambiare un campione: gli stessi blocchi, con lo stesso contenuto, che vedrebbe
//! l'elaborazione offline, solo ritardati di [`Streamer::latency`] frame.
//!
//! La latenza aggiunta è la minima per il quantum del primo ciclo, fissata subito con degli zeri
//! in testa: `blocco − MCD(quantum, blocco)` frame, cioè 0 con un quantum multiplo del blocco
//! (256, 512, 1024…) e meno di un blocco altrimenti (224 con 480). Se poi il quantum cambia e un
//! ciclo chiede più frame di quanti il modulo ne abbia prodotti, la latenza cresce del necessario
//! (zeri in testa a quel ciclo, una volta sola). Non cala mai: abbassarla vorrebbe dire scartare
//! audio.
//!
//! Il modulo ha memoria (leveler, smoothing, filtri): la sua uscita dipende da tutto ciò che ha
//! elaborato prima, anche dal silenzio (Film con 10 s di silenzio in testa: −35 dB dal
//! riferimento). Su Windows ogni sessione parte da un modulo appena configurato, e così deve
//! partire qui: lo Streamer nasce *fermo* e non chiama il modulo finché nessuna sorgente è
//! presente (uscita a zero); al primo ciclo con una sorgente riparte con un blocco nuovo, come
//! l'offline dal primo campione del file. Con [`Streamer::set_idle_reset`], dopo un periodo senza
//! sorgenti (la coda del modulo è finita) lo riconfigura ([`Engine::reconfigure`], equivalente
//! a un'istanza nuova, verificato byte per byte) e torna fermo.
//!
//! Nessuna allocazione dopo [`Streamer::new`]: `process` è chiamabile dal thread real time.

use anyhow::Result;

use crate::engine::{Backend, Engine};
use crate::layout::{Routing, NUM_STATIC, OUT_CHANNELS};

/// Un flusso d'ingresso in un ciclo: i suoi canali planari, nell'ordine di `routing.map`.
/// I campioni oltre la lunghezza di un canale valgono zero. Sorgenti diverse devono usare
/// ingressi statici diversi (giochi sui bed, applicazioni sui bypass): non vengono sommate.
pub struct Source<'a> {
    pub routing: &'a Routing,
    pub channels: &'a [&'a [f32]],
}

pub struct Streamer<B: Backend> {
    engine: Engine<B>,
    /// Frame già scritti negli ingressi del blocco in costruzione.
    filled: usize,
    /// Per ogni ingresso statico, fin dove è stato scritto nel blocco in costruzione
    /// (0 = nessuna sorgente in questo blocco: inattivo).
    written: [usize; NUM_STATIC],
    /// FIFO dell'uscita del modulo, stereo interleaved, prima del volume.
    ring: Vec<f32>,
    head: usize,
    queued: usize,
    latency: usize,
    max_frames: usize,
    started: bool,
    /// Modulo appena configurato, non ancora chiamato: si aspetta una sorgente.
    idle: bool,
    /// Frame consecutivi senza sorgenti, e dopo quanti riconfigurare il modulo.
    silent: usize,
    idle_after: Option<usize>,
    resets: u64,
}

impl<B: Backend> Streamer<B> {
    /// `max_frames`: frame massimi elaborati in un colpo; `process` spezza i cicli più lunghi.
    pub fn new(engine: Engine<B>, max_frames: usize) -> Self {
        let block = engine.block_frames();
        let max_frames = max_frames.max(1);
        Streamer {
            engine,
            filled: 0,
            written: [0; NUM_STATIC],
            // Al massimo: latenza (< blocco) + un blocco appena prodotto + un ciclo intero.
            ring: vec![0.0; (max_frames + 2 * block) * OUT_CHANNELS],
            head: 0,
            queued: 0,
            latency: 0,
            max_frames,
            started: false,
            idle: true,
            silent: 0,
            idle_after: None,
            resets: 0,
        }
    }

    /// Dopo `frames` frame consecutivi senza sorgenti riconfigura il modulo e lo ferma fino alla
    /// prossima sorgente (None: mai, il modulo resta in moto; è il comportamento dell'offline).
    pub fn set_idle_reset(&mut self, frames: Option<usize>) {
        self.idle_after = frames;
    }

    /// Il modulo è fermo (in attesa di una sorgente).
    pub fn is_idle(&self) -> bool {
        self.idle
    }

    /// Riconfigurazioni fatte dopo un periodo senza sorgenti.
    pub fn resets(&self) -> u64 {
        self.resets
    }

    pub fn engine_mut(&mut self) -> &mut Engine<B> {
        &mut self.engine
    }

    pub fn block_frames(&self) -> usize {
        self.engine.block_frames()
    }

    /// Ritardo dell'uscita rispetto all'ingresso introdotto dall'adattatore, in frame
    /// (escluso il ritardo algoritmico del modulo, che è lo stesso dell'elaborazione offline).
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Elabora un ciclo di `frames` frame: consuma gli ingressi di `sources` e scrive `frames`
    /// frame in `out` (sinistro, destro), moltiplicati per `volume` dopo il modulo, come il
    /// volume dell'endpoint di Windows.
    pub fn process(&mut self, frames: usize, sources: &[Source], volume: f32, out: [&mut [f32]; 2]) -> Result<()> {
        let [left, right] = out;
        if !self.started && frames > 0 {
            self.started = true;
            let block = self.engine.block_frames();
            let q = frames.min(self.max_frames);
            // Con un quantum costante q, il ciclo k chiede k·q frame e il modulo ne ha prodotti
            // ⌊k·q / blocco⌋·blocco: lo scarto massimo è blocco − MCD(q, blocco).
            self.latency = block - gcd(q, block);
            if self.latency == block {
                self.latency = 0;
            }
            // Il ring è vuoto: gli zeri vanno in testa.
            self.ring[..self.latency * OUT_CHANNELS].fill(0.0);
            self.queued = self.latency;
        }
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(self.max_frames);
            self.push(pos, n, sources)?;
            self.pop(&mut left[pos..pos + n], &mut right[pos..pos + n], volume);
            pos += n;
        }
        Ok(())
    }

    fn push(&mut self, start: usize, frames: usize, sources: &[Source]) -> Result<()> {
        let block = self.engine.block_frames();
        if self.idle {
            if sources.is_empty() {
                // Modulo fermo: nessuna chiamata, uscita a zero.
                self.enqueue_zeros(frames);
                return Ok(());
            }
            // Si riparte con un blocco nuovo (da fermo `filled` è 0) dal primo frame del ciclo.
            self.idle = false;
        }
        let mut pos = 0;
        while pos < frames {
            let take = (block - self.filled).min(frames - pos);
            let filled = self.filled;
            if sources.is_empty() {
                self.silent += take;
            } else {
                self.silent = 0;
            }
            for src in sources {
                let gain = src.routing.gain;
                for (&b, ch) in src.routing.map.iter().zip(src.channels) {
                    let dst = self.engine.input_mut(b);
                    // Il buffer contiene ancora un blocco precedente: la parte del blocco in cui
                    // la sorgente era assente vale zero.
                    dst[self.written[b]..filled].fill(0.0);
                    self.written[b] = filled + take;
                    let from = (start + pos).min(ch.len());
                    let to = (start + pos + take).min(ch.len());
                    let dst = &mut dst[filled..filled + take];
                    let n = to - from;
                    for (d, s) in dst[..n].iter_mut().zip(&ch[from..to]) {
                        // Stessa operazione dell'offline (ingresso × guadagno, in f32).
                        *d = s * gain;
                    }
                    dst[n..].fill(0.0);
                }
            }
            self.filled += take;
            pos += take;
            if self.filled == block {
                let mut active = [false; NUM_STATIC];
                for (b, w) in self.written.iter_mut().enumerate() {
                    if *w > 0 {
                        self.engine.input_mut(b)[*w..].fill(0.0);
                        active[b] = true;
                    }
                    *w = 0;
                }
                let out = self.engine.process(&active)?;
                let cap = self.ring.len() / OUT_CHANNELS;
                debug_assert!(self.queued + block <= cap);
                let mut tail = (self.head + self.queued) % cap;
                for frame in out.chunks_exact(OUT_CHANNELS) {
                    self.ring[tail * OUT_CHANNELS..(tail + 1) * OUT_CHANNELS].copy_from_slice(frame);
                    tail = (tail + 1) % cap;
                }
                self.queued += block;
                self.filled = 0;
                if self.idle_after.is_some_and(|after| self.silent >= after) {
                    self.engine.reconfigure()?;
                    self.resets += 1;
                    self.idle = true;
                    self.silent = 0;
                    self.enqueue_zeros(frames - pos);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn enqueue_zeros(&mut self, frames: usize) {
        let cap = self.ring.len() / OUT_CHANNELS;
        debug_assert!(self.queued + frames <= cap);
        let mut tail = (self.head + self.queued) % cap;
        for _ in 0..frames {
            self.ring[tail * OUT_CHANNELS..(tail + 1) * OUT_CHANNELS].fill(0.0);
            tail = (tail + 1) % cap;
        }
        self.queued += frames;
    }

    fn pop(&mut self, left: &mut [f32], right: &mut [f32], volume: f32) {
        let frames = left.len();
        let missing = frames.saturating_sub(self.queued);
        if missing > 0 {
            // Il modulo non ha ancora prodotto abbastanza: si ritarda l'uscita di `missing`
            // frame, una volta per tutte.
            self.latency += missing;
            left[..missing].fill(0.0);
            right[..missing].fill(0.0);
        }
        let cap = self.ring.len() / OUT_CHANNELS;
        for i in missing..frames {
            let f = &self.ring[self.head * OUT_CHANNELS..(self.head + 1) * OUT_CHANNELS];
            left[i] = f[0] * volume;
            right[i] = f[1] * volume;
            self.head = (self.head + 1) % cap;
        }
        self.queued -= frames - missing;
    }
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::EncodeInput;
    use crate::engine::ModuleSetup;
    use crate::layout::{Mode, BYPASS_L, BYPASS_R, NUM_STATIC};
    use std::path::Path;

    /// Modulo finto con memoria: L = somma degli ingressi attivi + ultimo campione del blocco
    /// precedente, R = conteggio degli ingressi attivi + indice del blocco.
    struct Mock {
        last: f32,
        blocks: u32,
        frames: usize,
    }

    impl Backend for Mock {
        fn open(setup: &ModuleSetup) -> Result<Self> {
            Ok(Mock { last: 0.0, blocks: 0, frames: setup.block_frames() })
        }

        unsafe fn encode(&mut self, input: *mut EncodeInput, out: *mut f32) -> Result<()> {
            let entries = unsafe { std::slice::from_raw_parts((*input).entries, NUM_STATIC) };
            let out = unsafe { std::slice::from_raw_parts_mut(out, self.frames * 2) };
            let n_active = entries.iter().filter(|e| e.active != 0).count() as f32;
            for i in 0..self.frames {
                let sum: f32 = entries.iter().map(|e| unsafe { *e.samples.add(i) }).sum();
                out[2 * i] = sum + self.last;
                out[2 * i + 1] = n_active + self.blocks as f32;
            }
            self.last = out[2 * (self.frames - 1)];
            self.blocks += 1;
            Ok(())
        }

        fn reconfigure(&mut self) -> Result<()> {
            self.last = 0.0;
            self.blocks = 0;
            Ok(())
        }
    }

    fn engine(block: usize) -> Engine<Mock> {
        let setup = ModuleSetup::new(Path::new("mock"), Mode::Headphones, block, Vec::new(), false).unwrap();
        Engine::open(&setup).unwrap()
    }

    fn signal(frames: usize, channels: usize, seed: u32) -> Vec<Vec<f32>> {
        let mut x = seed;
        (0..channels)
            .map(|_| {
                (0..frames)
                    .map(|_| {
                        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                    })
                    .collect()
            })
            .collect()
    }

    /// Riferimento: lo stesso percorso dell'offline (blocchi interi, guadagno, volume dopo).
    fn reference(games: &[Vec<f32>], apps: &[Vec<f32>], rg: &Routing, ra: &Routing, block: usize, volume: f32) -> Vec<[f32; 2]> {
        let mut e = engine(block);
        let frames = games[0].len();
        let mut out = Vec::new();
        for start in (0..frames).step_by(block) {
            let mut active = [false; NUM_STATIC];
            for (chans, r) in [(games, rg), (apps, ra)] {
                for (c, &b) in r.map.iter().enumerate() {
                    let buf = e.input_mut(b);
                    for (i, s) in buf.iter_mut().enumerate() {
                        *s = chans[c].get(start + i).map_or(0.0, |v| v * r.gain);
                    }
                    active[b] = true;
                }
            }
            out.extend(e.process(&active).unwrap().chunks_exact(2).map(|f| [f[0] * volume, f[1] * volume]));
        }
        out
    }

    /// Fa passare lo stesso segnale con la sequenza di quantum `quanta` (ripetuta) e controlla
    /// che l'uscita sia quella del percorso a blocchi, bit per bit, con gli zeri dove la latenza
    /// è cresciuta. Restituisce la latenza dopo il primo ciclo e quella finale.
    fn run(quanta: &[usize], block: usize) -> (usize, usize) {
        let rg = Routing::new(8, false, None).unwrap();
        let ra = Routing::new(2, true, None).unwrap();
        assert_eq!(ra.map, &[BYPASS_L, BYPASS_R]);
        // Cicli sempre interi (un ultimo ciclo più corto cambierebbe la latenza): il segnale
        // ha margine per l'ultimo.
        let total = 20 * block + 77;
        let games = signal(total + 4096, 8, 1);
        let apps = signal(total + 4096, 2, 2);
        let volume = 0.34100;
        let reference = reference(&games, &apps, &rg, &ra, block, volume);

        let mut s = Streamer::new(engine(block), 1024);
        let mut expected: Vec<[f32; 2]> = Vec::new();
        let mut got: Vec<[f32; 2]> = Vec::new();
        let (mut pos, mut lat, mut first) = (0, 0, None);
        let (mut ref_pos, mut zeros) = (0, 0);
        for &q in quanta.iter().cycle() {
            if pos >= total {
                break;
            }
            let g: Vec<&[f32]> = games.iter().map(|c| &c[pos..pos + q]).collect();
            let a: Vec<&[f32]> = apps.iter().map(|c| &c[pos..pos + q]).collect();
            let (mut l, mut r) = (vec![0.0; q], vec![0.0; q]);
            s.process(q, &[Source { routing: &rg, channels: &g }, Source { routing: &ra, channels: &a }], volume, [&mut l, &mut r]).unwrap();
            // Gli zeri della latenza cresciuta escono per primi, anche su più cicli.
            zeros += s.latency() - lat;
            lat = s.latency();
            first.get_or_insert(lat);
            let z = zeros.min(q);
            zeros -= z;
            expected.extend(std::iter::repeat_n([0.0, 0.0], z));
            expected.extend(&reference[ref_pos..ref_pos + q - z]);
            ref_pos += q - z;
            got.extend(l.iter().zip(&r).map(|(&l, &r)| [l, r]));
            pos += q;
        }
        assert!(lat < block, "latenza {lat} con quanta {quanta:?}");
        for (i, (g, e)) in got.iter().zip(&expected).enumerate() {
            assert_eq!(g.map(f32::to_bits), e.map(f32::to_bits), "frame {i}, quanta {quanta:?}");
        }
        (first.unwrap(), lat)
    }

    #[test]
    fn quantum_multiplo_del_blocco_senza_latenza() {
        for q in [256, 512, 1024, 2048] {
            let rg = Routing::new(8, false, None).unwrap();
            let mut s = Streamer::new(engine(256), 1024);
            let ch = vec![0.25f32; q];
            let chans: Vec<&[f32]> = (0..8).map(|_| &ch[..]).collect();
            let (mut l, mut r) = (vec![0.0; q], vec![0.0; q]);
            for _ in 0..4 {
                s.process(q, &[Source { routing: &rg, channels: &chans }], 1.0, [&mut l, &mut r]).unwrap();
            }
            assert_eq!(s.latency(), 0, "quantum {q}");
        }
        assert_eq!(run(&[256], 256), (0, 0));
        assert_eq!(run(&[1024], 256), (0, 0));
        assert_eq!(run(&[1536], 512), (0, 0));
    }

    #[test]
    fn quantum_costante_latenza_fissa_dal_primo_ciclo() {
        assert_eq!(run(&[480], 256), (224, 224));
        assert_eq!(run(&[1000], 256), (248, 248));
        assert_eq!(run(&[128], 256), (128, 128));
        assert_eq!(run(&[1], 256), (255, 255));
    }

    #[test]
    fn quantum_variabile_identico_al_percorso_a_blocchi() {
        run(&[333, 1024, 17, 2048, 256, 1], 256);
        run(&[100, 3000], 512);
        run(&[1024, 480], 256);
        run(&[1, 1, 1, 255, 257], 256);
    }

    #[test]
    fn sorgente_presente_in_parte_del_blocco() {
        // Dove una sorgente manca, i suoi ingressi valgono zero; il modulo la vede attiva per
        // tutto il blocco in cui ha dato almeno un frame.
        let rg = Routing::new(2, false, None).unwrap();
        let mut s = Streamer::new(engine(256), 1024);
        let ones = vec![1.0f32; 128];
        let chans: Vec<&[f32]> = vec![&ones, &ones];
        let src = [Source { routing: &rg, channels: &chans }];
        let (mut left, mut right) = (Vec::new(), Vec::new());
        // Blocco 1: dati in [0, 128); blocco 2: dati in [128, 256); poi silenzio.
        for present in [true, false, false, true, false, false] {
            let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
            s.process(128, if present { &src } else { &[] }, 1.0, [&mut l, &mut r]).unwrap();
            left.extend(l);
            right.extend(r);
        }
        assert_eq!(s.latency(), 128);
        let g2 = 2.0 * rg.gain;
        let expected_l: Vec<f32> = [vec![0.0; 128], vec![g2; 128], vec![0.0; 128], vec![0.0; 128], vec![g2; 128]].concat();
        let expected_r: Vec<f32> = [vec![0.0; 128], vec![2.0; 256], vec![3.0; 256]].concat();
        assert_eq!(left[..640], expected_l[..]);
        assert_eq!(right[..640], expected_r[..]);
    }

    #[test]
    fn fermo_finche_non_arriva_una_sorgente_poi_riconfigurato() {
        // Prima della sorgente il modulo non viene chiamato (blocks resta 0 e il primo blocco
        // con la sorgente ha R = 2 attivi + 0); dopo idle_after frame senza sorgenti viene
        // riconfigurato (blocks torna 0) e la sessione successiva riparte come la prima.
        let rg = Routing::new(2, false, None).unwrap();
        let mut s = Streamer::new(engine(256), 1024);
        s.set_idle_reset(Some(512));
        let ones = vec![1.0f32; 256];
        let chans: Vec<&[f32]> = vec![&ones, &ones];
        let src = [Source { routing: &rg, channels: &chans }];
        let cycle = |s: &mut Streamer<Mock>, present: bool| {
            let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
            s.process(256, if present { &src } else { &[] }, 1.0, [&mut l, &mut r]).unwrap();
            (l[0], r[0], s.is_idle())
        };
        let g2 = 2.0 * rg.gain;
        assert_eq!(cycle(&mut s, false), (0.0, 0.0, true));
        assert_eq!(cycle(&mut s, false), (0.0, 0.0, true));
        let first = cycle(&mut s, true);
        assert_eq!(first, (g2, 2.0, false));
        assert_eq!(cycle(&mut s, true), (2.0 * g2, 3.0, false));
        // Coda: blocchi inattivi ancora elaborati (L = solo la memoria, R = 0 attivi + indice),
        // poi la riconfigurazione.
        assert_eq!(cycle(&mut s, false), (2.0 * g2, 2.0, false));
        assert_eq!(cycle(&mut s, false), (2.0 * g2, 3.0, true));
        assert_eq!(cycle(&mut s, false), (0.0, 0.0, true));
        assert_eq!(s.resets(), 1);
        assert_eq!(cycle(&mut s, true), first);
    }
}
