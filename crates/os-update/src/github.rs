//! L'ultima release dall'API di GitHub, in HTTPS. Non si manda niente dell'utente: una GET
//! anonima con uno User-Agent fisso.

use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

pub const LATEST_URL: &str = "https://api.github.com/repos/giacomomicoli/open-surround/releases/latest";
/// Pagina da aprire quando l'aggiornamento non si può fare dalla finestra.
pub const RELEASES_PAGE: &str = "https://github.com/giacomomicoli/open-surround/releases/latest";

/// Limiti delle risposte: la descrizione della release, il pacchetto.
const MAX_JSON: u64 = 4 << 20;
pub const MAX_PACKAGE: u64 = 256 << 20;

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub body: Option<String>,
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn parse(json: &str) -> Result<Release> {
        serde_json::from_str(json).context("risposta di GitHub non riconosciuta")
    }

    /// La versione del tag, senza `v`.
    pub fn version(&self) -> &str {
        self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)
    }

    pub fn notes(&self) -> &str {
        self.body.as_deref().unwrap_or("").trim()
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// Nome della cartella del pacchetto per una versione.
pub fn package_dir(version: &str) -> String {
    format!("open-surround-{version}-linux-x86_64")
}

/// Nome del tar.gz per una versione.
pub fn package_name(version: &str) -> String {
    format!("{}.tar.gz", package_dir(version))
}

pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    /// `allow_http` solo per le prove con un server locale.
    pub fn new(allow_http: bool) -> Http {
        let config = ureq::Agent::config_builder()
            .https_only(!allow_http)
            .timeout_global(Some(Duration::from_secs(300)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .user_agent("open-surround-updater")
            .build();
        Http { agent: ureq::Agent::new_with_config(config) }
    }

    fn get(&self, url: &str, accept: &str, limit: u64) -> Result<Vec<u8>> {
        let mut resp = self.agent.get(url).header("Accept", accept).call().with_context(|| format!("GET {url}"))?;
        resp.body_mut().with_config().limit(limit).read_to_vec().with_context(|| format!("GET {url}"))
    }

    pub fn latest(&self, api_url: &str) -> Result<Release> {
        let body = match self.get(api_url, "application/vnd.github+json", MAX_JSON) {
            Ok(b) => b,
            // GitHub risponde 404 anche per un repository privato (la richiesta è anonima).
            Err(e) if matches!(e.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(404))) => {
                bail!("nessuna release pubblica su GitHub (repository privato o senza release): {api_url}")
            }
            Err(e) => return Err(e.context("controllo dell'ultima release")),
        };
        Release::parse(&String::from_utf8_lossy(&body))
    }

    pub fn download(&self, asset: &Asset, limit: u64) -> Result<Vec<u8>> {
        self.get(&asset.browser_download_url, "application/octet-stream", limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risposta_di_github() {
        let json = r###"{"tag_name":"v0.2.0","name":"v0.2.0","body":"## Novità\n- x\n","html_url":"https://github.com/x/y/releases/tag/v0.2.0",
            "draft":false,"assets":[{"name":"open-surround-0.2.0-linux-x86_64.tar.gz","browser_download_url":"https://e/a","size":5,"other":1}]}"###;
        let r = Release::parse(json).unwrap();
        assert_eq!(r.version(), "0.2.0");
        assert_eq!(r.notes(), "## Novità\n- x");
        assert_eq!(r.asset(&package_name("0.2.0")).unwrap().size, 5);
        assert!(r.asset("altro").is_none());
        assert!(Release::parse(r#"{"message":"Not Found"}"#).is_err());
    }
}
