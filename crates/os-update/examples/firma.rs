//! Firma delle release con minisign (doc 12.15, "Aggiornamenti"): lo usa la CI e, una volta,
//! chi genera le chiavi. Stesso formato del programma `minisign`, quindi la firma si controlla
//! anche con `minisign -Vm <file> -p dist/linux/minisign.pub`.
//!
//!   cargo run -p os-update --example firma -- genera <chiave.pub> <chiave.segreta>
//!       nuova coppia di chiavi, la segreta senza password (va solo nel secret della CI)
//!   cargo run -p os-update --example firma -- firma <chiave.segreta> <file>
//!       scrive <file>.minisig con il commento fidato `file:<nome del file>`
//!
//!   cargo run -p os-update --example firma -- verifica <chiave.pub> <file>
//!       controlla <file>.minisig con la chiave pubblica (la CI, dopo aver firmato: una
//!       chiave segreta che non corrisponde a dist/linux/minisign.pub fa fallire la release)
//!
//! La chiave segreta si può dare anche in `MINISIGN_KEY` (contenuto, non percorso) con `-`
//! al posto del percorso.

use std::io::Write;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["genera", pk_path, sk_path] => {
            let kp = minisign::KeyPair::generate_unencrypted_keypair()?;
            let pk = kp.pk.to_box()?.into_string();
            let sk = kp.sk.to_box(Some("open-surround release key"))?.into_string();
            os_update::paths::create_new(Path::new(sk_path), 0o600)?.write_all(sk.as_bytes())?;
            std::fs::write(pk_path, pk)?;
            eprintln!("chiave pubblica in {pk_path}, segreta in {sk_path} (0600)");
        }
        ["firma", sk_path, file] => {
            let sk_text = if *sk_path == "-" { std::env::var("MINISIGN_KEY")? } else { std::fs::read_to_string(sk_path)? };
            let sk = minisign::SecretKeyBox::from_string(sk_text.trim())?.into_unencrypted_secret_key()?;
            let name = Path::new(file).file_name().ok_or("file senza nome")?.to_string_lossy().into_owned();
            let data = std::fs::File::open(file)?;
            let sig = minisign::sign(None, &sk, data, Some(&format!("file:{name}")), Some("open-surround release"))?;
            std::fs::write(format!("{file}.minisig"), sig.into_string())?;
            eprintln!("{file}.minisig");
        }
        ["verifica", pk_path, file] => {
            let pk = minisign::PublicKeyBox::from_string(std::fs::read_to_string(pk_path)?.trim())?.into_public_key()?;
            let sig = minisign::SignatureBox::from_string(&std::fs::read_to_string(format!("{file}.minisig"))?)?;
            minisign::verify(&pk, &sig, std::fs::File::open(file)?, true, false, false)?;
            let name = Path::new(file).file_name().ok_or("file senza nome")?.to_string_lossy().into_owned();
            if sig.trusted_comment()? != format!("file:{name}") {
                return Err(format!("commento fidato inatteso: {:?}", sig.trusted_comment()?).into());
            }
            eprintln!("{file}: firma valida");
        }
        _ => {
            eprintln!("uso: firma genera <chiave.pub> <chiave.segreta> | firma <chiave.segreta|-> <file> | verifica <chiave.pub> <file>");
            std::process::exit(2);
        }
    }
    Ok(())
}
