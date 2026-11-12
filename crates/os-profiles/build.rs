//! Versione e commit nei binari (doc 12.15, "Aggiornamenti"): `OS_GIT_COMMIT` dall'ambiente
//! (la CI lo imposta, dentro il container git può rifiutare il checkout), altrimenti
//! `git rev-parse HEAD` del repository; "sconosciuto" fuori da un repository.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=OS_GIT_COMMIT");
    let commit = std::env::var("OS_GIT_COMMIT").ok().filter(|c| !c.trim().is_empty()).map(|c| c.trim().to_string()).or_else(git).unwrap_or_else(|| "sconosciuto".into());
    println!("cargo:rustc-env=OS_GIT_COMMIT={commit}");
}

fn git() -> Option<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").arg("-C").arg(&root).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
    };
    // Ricompilare quando cambia il commit: HEAD, il ref a cui punta e packed-refs (solo i file
    // che esistono, altrimenti cargo rilancerebbe lo script a ogni build).
    let git_dir = run(&["rev-parse", "--absolute-git-dir"])?;
    let common = run(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).unwrap_or_else(|| git_dir.clone());
    let mut watch = vec![Path::new(&git_dir).join("HEAD"), Path::new(&common).join("packed-refs")];
    if let Some(r) = run(&["symbolic-ref", "-q", "HEAD"]) {
        watch.push(Path::new(&common).join(r));
    }
    for p in watch.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    run(&["rev-parse", "HEAD"])
}
