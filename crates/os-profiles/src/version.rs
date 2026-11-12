//! Versione e commit di questa build (doc 12.15, "Aggiornamenti"). La versione è quella di
//! `[workspace.package]`; il commit lo scrive `build.rs`.

/// Versione del workspace, per esempio `0.1.0`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Commit completo della build, o `sconosciuto`.
pub const COMMIT: &str = env!("OS_GIT_COMMIT");

/// Il commit abbreviato (12 caratteri), come nei messaggi.
pub fn short_commit() -> &'static str {
    COMMIT.get(..12).unwrap_or(COMMIT)
}

/// `0.1.0 (commit 0123456789ab)`.
pub fn describe() -> String {
    format!("{VERSION} (commit {})", short_commit())
}

#[cfg(test)]
mod tests {
    #[test]
    fn versione_e_commit() {
        assert!(super::VERSION.split('.').count() >= 3, "{}", super::VERSION);
        assert!(!super::COMMIT.is_empty());
        assert!(super::short_commit().len() <= 12);
        assert!(super::describe().starts_with(super::VERSION));
    }
}
