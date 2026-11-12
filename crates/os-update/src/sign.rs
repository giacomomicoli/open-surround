//! Firma delle release: minisign (Ed25519 su BLAKE2b del file). La CI firma il tar.gz con la
//! chiave segreta del repository (secret `MINISIGN_KEY`) e il commento fidato
//! `file:<nome del tar.gz>`; qui si verifica con la chiave pubblica compilata nel binario
//! (`dist/linux/minisign.pub`). Il commento fidato è coperto dalla firma: lega la firma al nome
//! del pacchetto, quindi alla versione, e impedisce di spacciare un pacchetto vecchio, firmato
//! davvero, per uno nuovo.

use anyhow::{bail, Context as _, Result};
use minisign_verify::{PublicKey, Signature};

/// La chiave pubblica delle release, nel formato di minisign (due righe).
pub const RELEASE_PUBLIC_KEY: &str = include_str!("../../../dist/linux/minisign.pub");

/// Verifica `data` con la firma `sig` (contenuto del `.minisig`; `None` se la release non ne
/// ha) e controlla che il commento fidato nomini `file`.
pub fn verify(public_key: &str, data: &[u8], sig: Option<&str>, file: &str) -> Result<()> {
    let Some(sig) = sig else {
        bail!("firma mancante: la release non ha {file}.minisig; aggiornamento rifiutato");
    };
    let pk = PublicKey::decode(public_key.trim()).or_else(|_| PublicKey::from_base64(public_key.trim())).map_err(|e| anyhow::anyhow!("chiave pubblica non valida: {e}"))?;
    let sig = Signature::decode(sig).map_err(|e| anyhow::anyhow!("firma illeggibile: {e}"))?;
    pk.verify(data, &sig, false).map_err(|e| anyhow::anyhow!("firma non valida per {file}: {e}"))?;
    let expected = format!("file:{file}");
    if !sig.trusted_comment().split_whitespace().any(|w| w == expected) {
        bail!("la firma è valida ma per un altro file ({:?}, atteso {expected:?})", sig.trusted_comment());
    }
    Ok(())
}

/// SHA-256 in esadecimale minuscolo.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Controlla `data` con il contenuto di un file `.sha256` (`<hash>  <nome>`, come sha256sum).
pub fn verify_sha256(data: &[u8], sha_file: &str, file: &str) -> Result<()> {
    let line = sha_file.lines().find(|l| l.split_whitespace().nth(1).map(|n| n.trim_start_matches('*')) == Some(file)).or_else(|| sha_file.lines().next()).context("file .sha256 vuoto")?;
    let expected = line.split_whitespace().next().context("file .sha256 vuoto")?.to_ascii_lowercase();
    let got = sha256_hex(data);
    if got != expected {
        bail!("SHA-256 di {file} diverso: {got}, atteso {expected}");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Coppia di chiavi di prova: (pubblica, segreta) nel formato dei file di minisign.
    pub fn keypair() -> (String, minisign::SecretKey) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        (kp.pk.to_box().unwrap().into_string(), kp.sk)
    }

    pub fn sign(sk: &minisign::SecretKey, data: &[u8], file: &str) -> String {
        minisign::sign(None, sk, data, Some(&format!("file:{file}")), None).unwrap().into_string()
    }

    #[test]
    fn la_chiave_delle_release_si_legge() {
        PublicKey::decode(RELEASE_PUBLIC_KEY.trim()).unwrap();
    }

    #[test]
    fn firma_buona_cattiva_mancante() {
        let (pk, sk) = keypair();
        let data = b"pacchetto".to_vec();
        let name = "open-surround-0.2.0-linux-x86_64.tar.gz";
        let sig = sign(&sk, &data, name);
        verify(&pk, &data, Some(&sig), name).unwrap();

        // contenuto cambiato
        let e = verify(&pk, b"pacchetto!", Some(&sig), name).unwrap_err();
        assert!(e.to_string().contains("firma non valida"), "{e}");
        // firmato da un'altra chiave
        let (pk2, _) = keypair();
        assert!(verify(&pk2, &data, Some(&sig), name).is_err());
        // la firma vera di un altro pacchetto (per esempio una versione vecchia)
        let e = verify(&pk, &data, Some(&sig), "open-surround-0.3.0-linux-x86_64.tar.gz").unwrap_err();
        assert!(e.to_string().contains("altro file"), "{e}");
        // firma mancante o illeggibile
        let e = verify(&pk, &data, None, name).unwrap_err();
        assert!(e.to_string().contains("firma mancante"), "{e}");
        assert!(verify(&pk, &data, Some("niente"), name).is_err());
        // la firma vera con il commento fidato cambiato
        let forged = sig.replace("file:open-surround-0.2.0", "file:open-surround-0.9.0");
        assert!(verify(&pk, &data, Some(&forged), "open-surround-0.9.0-linux-x86_64.tar.gz").is_err());
    }

    #[test]
    fn sha256() {
        let data = b"abc";
        let h = sha256_hex(data);
        assert_eq!(h, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        verify_sha256(data, &format!("{h}  x.tar.gz\n"), "x.tar.gz").unwrap();
        verify_sha256(data, &h.to_uppercase(), "x.tar.gz").unwrap();
        assert!(verify_sha256(b"abd", &format!("{h}  x.tar.gz\n"), "x.tar.gz").is_err());
        assert!(verify_sha256(data, "", "x.tar.gz").is_err());
    }
}
