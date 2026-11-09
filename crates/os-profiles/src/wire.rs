//! Formato a byte di presenza dei valori del contesto Dolby (doc sez. 10.3 e 12.14).
//!
//! Ogni valore è una sequenza di campi in ordine fisso; ogni campo comincia con 1 byte di
//! presenza (0 = assente, 1 = presente) seguito, se presente, dal valore: bool 1 byte (0 o 1),
//! int32 little endian, array = conteggio int32 + elementi. Nessun allineamento, niente in coda.

use std::fmt;

use anyhow::{bail, Result};

/// Lettore di un valore, con la posizione per i messaggi d'errore.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        if self.data.len() - self.pos < n {
            bail!("{what}: servono {n} byte all'offset {}, ne restano {}", self.pos, self.data.len() - self.pos);
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u8(&mut self, what: &str) -> Result<u8> {
        Ok(self.take(1, what)?[0])
    }

    pub fn i32(&mut self, what: &str) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4, what)?.try_into().unwrap()))
    }

    /// Un campo: byte di presenza e, se presente, il valore.
    pub fn field<T: Wire>(&mut self, name: &str) -> Result<Option<T>> {
        let at = self.pos;
        match self.u8(name)? {
            0 => Ok(None),
            1 => Ok(Some(T::read(self, name)?)),
            f => bail!("{name}: byte di presenza {f} all'offset {at} (atteso 0 o 1)"),
        }
    }

    /// Verifica che il valore sia stato letto fino all'ultimo byte.
    pub fn finish(&self) -> Result<()> {
        if self.pos != self.data.len() {
            bail!("{} byte in più dopo l'ultimo campo (offset {})", self.data.len() - self.pos, self.pos);
        }
        Ok(())
    }
}

/// Scrive un campo: byte di presenza e valore.
pub fn write_field<T: Wire>(out: &mut Vec<u8>, v: &Option<T>) {
    match v {
        None => out.push(0),
        Some(v) => {
            out.push(1);
            v.write(out);
        }
    }
}

/// Testo di un campo per i dump: il valore o `-` se assente.
pub fn show_field<T: Wire>(v: &Option<T>) -> String {
    v.as_ref().map_or_else(|| "-".into(), |v| v.show())
}

/// Un tipo che sa leggersi e scriversi nel formato dei blob.
pub trait Wire: Sized + Clone + PartialEq + fmt::Debug {
    fn read(r: &mut Reader, name: &str) -> Result<Self>;
    fn write(&self, out: &mut Vec<u8>);
    fn show(&self) -> String;
}

impl Wire for bool {
    fn read(r: &mut Reader, name: &str) -> Result<Self> {
        match r.u8(name)? {
            0 => Ok(false),
            1 => Ok(true),
            v => bail!("{name}: bool con valore {v}"),
        }
    }
    fn write(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn show(&self) -> String {
        self.to_string()
    }
}

impl Wire for i32 {
    fn read(r: &mut Reader, name: &str) -> Result<Self> {
        r.i32(name)
    }
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn show(&self) -> String {
        self.to_string()
    }
}

/// Coppia (frequenza in Hz, guadagno in 1/16 dB) degli array di bande.
impl Wire for (i32, i32) {
    fn read(r: &mut Reader, name: &str) -> Result<Self> {
        Ok((r.i32(name)?, r.i32(name)?))
    }
    fn write(&self, out: &mut Vec<u8>) {
        self.0.write(out);
        self.1.write(out);
    }
    fn show(&self) -> String {
        format!("{}:{}", self.0, self.1)
    }
}

/// Array: conteggio int32 degli elementi, poi gli elementi.
impl<T: Wire> Wire for Vec<T> {
    fn read(r: &mut Reader, name: &str) -> Result<Self> {
        let n = r.i32(name)?;
        if n < 0 || n as usize > r.data.len() - r.pos {
            bail!("{name}: conteggio {n} non valido");
        }
        (0..n).map(|_| T::read(r, name)).collect()
    }
    fn write(&self, out: &mut Vec<u8>) {
        (self.len() as i32).write(out);
        for v in self {
            v.write(out);
        }
    }
    fn show(&self) -> String {
        let items: Vec<String> = self.iter().map(Wire::show).collect();
        format!("[{}] {}", self.len(), items.join(" "))
    }
}

/// Campo sempre assente nei blob catturati, di tipo ignoto: se un blob lo contiene la lettura
/// fallisce, invece di indovinarne il formato.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unknown {}

impl Wire for Unknown {
    fn read(r: &mut Reader, name: &str) -> Result<Self> {
        bail!("{name}: campo di tipo ignoto presente all'offset {} (mai visto nelle catture)", r.pos - 1)
    }
    fn write(&self, _: &mut Vec<u8>) {
        match *self {}
    }
    fn show(&self) -> String {
        match *self {}
    }
}

/// Definisce un record: struttura con un `Option` per campo, nell'ordine del formato, e
/// lettura/scrittura/dump.
macro_rules! record {
    ($(#[$m:meta])* $name:ident { $( $(#[$fm:meta])* $field:ident : $ty:ty ),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Debug, Default, PartialEq)]
        pub struct $name {
            $( $(#[$fm])* pub $field: Option<$ty>, )*
        }

        impl $name {
            /// Nomi dei campi, nell'ordine del formato.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($field)),*];

            /// Legge il valore senza l'header del registro; deve finire esattamente all'ultimo campo.
            pub fn decode(payload: &[u8]) -> anyhow::Result<Self> {
                let mut r = $crate::wire::Reader::new(payload);
                // I campi di un'espressione struct sono valutati nell'ordine scritto.
                let v = $name { $( $field: r.field::<$ty>(stringify!($field))?, )* };
                r.finish()?;
                Ok(v)
            }

            /// Il valore senza l'header del registro (quello che riceve il modulo).
            pub fn encode(&self) -> Vec<u8> {
                let mut out = Vec::new();
                $( $crate::wire::write_field(&mut out, &self.$field); )*
                out
            }

            /// (nome, valore) di ogni campo, per i dump.
            pub fn describe(&self) -> Vec<(&'static str, String)> {
                vec![$( (stringify!($field), $crate::wire::show_field(&self.$field)) ),*]
            }
        }
    };
}
pub(crate) use record;

#[cfg(test)]
mod tests {
    // In un modulo a parte per tollerare i metodi del record che i test non usano.
    #[allow(dead_code)]
    mod prova {
        use super::super::*;
        record!(Prova { a: bool, b: i32, c: Vec<i32>, d: Vec<(i32, i32)>, e: Unknown });
    }
    use prova::Prova;

    #[test]
    fn andata_e_ritorno() {
        let p = Prova { a: Some(true), b: Some(-320), c: None, d: Some(vec![(47, 157), (141, -3)]), e: None };
        let bytes = p.encode();
        assert_eq!(&bytes[..7], &[1, 1, 1, 0xc0, 0xfe, 0xff, 0xff]);
        assert_eq!(Prova::decode(&bytes).unwrap(), p);
    }

    #[test]
    fn errori() {
        // byte di presenza non valido
        assert!(Prova::decode(&[2]).is_err());
        // bool fuori da 0/1
        assert!(Prova::decode(&[1, 2, 0, 0, 0, 0]).is_err());
        // troppo corto
        assert!(Prova::decode(&[1, 1, 1, 0]).is_err());
        // campo ignoto presente
        assert!(Prova::decode(&[0, 0, 0, 0, 1]).is_err());
        // byte in coda
        assert!(Prova::decode(&[0, 0, 0, 0, 0, 0]).is_err());
        assert!(Prova::decode(&[0, 0, 0, 0, 0]).is_ok());
    }
}
