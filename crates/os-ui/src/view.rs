//! Conversioni fra [`Settings`] e i valori mostrati dalla finestra, senza Slint (testabili).

use std::time::{Duration, Instant};

use os_profiles::model::{Ieq, GEQ_SLIDERS_HZ};
use os_profiles::settings::Kind;

/// Nomi dei profili come nell'app, nell'ordine di [`Kind::ALL`].
pub const PROFILE_NAMES: [&str; 7] = ["Gaming", "Film", "Musica", "Voce", "Personalizzato 1", "Personalizzato 2", "Personalizzato 3"];

/// Preset IEQ nell'ordine in cui l'app li mostra (indice dei pulsanti).
pub const IEQ_ORDER: [Ieq; 4] = [Ieq::Detailed, Ieq::Balanced, Ieq::Warm, Ieq::Off];

pub fn kind_index(kind: Kind) -> i32 {
    Kind::ALL.iter().position(|&k| k == kind).unwrap() as i32
}

pub fn kind_at(i: i32) -> Option<Kind> {
    usize::try_from(i).ok().and_then(|i| Kind::ALL.get(i).copied())
}

pub fn ieq_index(ieq: Ieq) -> i32 {
    IEQ_ORDER.iter().position(|&x| x == ieq).unwrap() as i32
}

pub fn ieq_at(i: i32) -> Option<Ieq> {
    usize::try_from(i).ok().and_then(|i| IEQ_ORDER.get(i).copied())
}

/// Etichette degli slider del GEQ: 32, 64, …, 1k, …, 16k.
pub fn band_labels() -> [String; 10] {
    GEQ_SLIDERS_HZ.map(|hz| if hz >= 1000 { format!("{}k", hz / 1000) } else { hz.to_string() })
}

/// Valore di uno slider (float della finestra) in 1/16 dB intero.
pub fn slider_value(v: f32) -> i32 {
    v.round() as i32
}

/// Invio limitato durante il trascinamento di uno slider (doc 12.15, "Cosa non si sa ancora"
/// punto 1): al più un invio ogni `interval`; l'ultimo valore parte comunque, al più
/// `interval` dopo l'ultima modifica.
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Throttle { interval, last: None }
    }

    /// Fra quanto mandare una modifica fatta ora: zero se si può subito.
    pub fn delay(&self, now: Instant) -> Duration {
        match self.last {
            Some(t) => (t + self.interval).saturating_duration_since(now),
            None => Duration::ZERO,
        }
    }

    pub fn sent(&mut self, now: Instant) {
        self.last = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indici() {
        for (i, k) in Kind::ALL.into_iter().enumerate() {
            assert_eq!(kind_at(kind_index(k)), Some(k));
            assert_eq!(kind_index(k), i as i32);
        }
        assert_eq!(kind_at(7), None);
        assert_eq!(kind_at(-1), None);
        for ieq in Ieq::ALL {
            assert_eq!(ieq_at(ieq_index(ieq)), Some(ieq));
        }
        assert_eq!(ieq_at(4), None);
        assert_eq!(band_labels()[0], "32");
        assert_eq!(band_labels()[5], "1k");
        assert_eq!(band_labels()[9], "16k");
        assert_eq!(slider_value(-144.6), -145);
    }

    #[test]
    fn invio_limitato() {
        let t0 = Instant::now();
        let mut th = Throttle::new(Duration::from_millis(100));
        assert_eq!(th.delay(t0), Duration::ZERO);
        th.sent(t0);
        assert_eq!(th.delay(t0 + Duration::from_millis(30)), Duration::from_millis(70));
        assert_eq!(th.delay(t0 + Duration::from_millis(150)), Duration::ZERO);
    }
}
