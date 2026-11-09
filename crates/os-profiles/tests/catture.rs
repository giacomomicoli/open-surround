//! Andata e ritorno identico byte per byte su tutte le catture disponibili (doc 12.14, L1):
//! `docs/reference/captures` (nel repository, sempre) e `target/verify` (asset
//! `verify-data-2026-11-03` più le cartelle locali, se c'è).

use std::path::PathBuf;

use os_profiles::check::check_tree;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn catture_del_repository() {
    let r = check_tree(&[root().join("docs/reference/captures")]).unwrap();
    assert!(r.failures.is_empty(), "{:#?}", r.failures);
    // 8 endpoint della cattura completa del 2/11 più 6 cartelle con i soli runtime di 8 endpoint.
    assert_eq!(r.files, 48 + 6 * 16);
    assert_eq!(r.contexts, 8);
}

#[test]
fn catture_di_target_verify() {
    let dir = root().join("target/verify");
    if !dir.is_dir() {
        eprintln!("target/verify assente: test saltato (scaricare l'asset verify-data-2026-11-03)");
        return;
    }
    let r = check_tree(&[dir]).unwrap();
    assert!(r.failures.is_empty(), "{:#?}", r.failures);
    assert!(r.contexts >= 21 * 3, "contesti verificati: {}", r.contexts);
}
