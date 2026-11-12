//! Versioni `x.y.z[-pre][+build]`, confrontate come semver: una pre-release viene prima della
//! versione finale; le pre-release fra loro per identificatori (numerici come numeri).

use std::cmp::Ordering;

use anyhow::{bail, Context as _, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    /// Accetta anche la `v` iniziale dei tag (`v0.2.0`).
    pub fn parse(s: &str) -> Result<Version> {
        let t = s.trim();
        let t = t.strip_prefix('v').unwrap_or(t);
        let t = t.split_once('+').map_or(t, |(a, _)| a);
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, p.split('.').map(str::to_string).collect()),
            None => (t, Vec::new()),
        };
        let nums: Vec<&str> = core.split('.').collect();
        if nums.len() != 3 {
            bail!("versione non valida: {s:?}");
        }
        let n = |x: &str| x.parse::<u64>().with_context(|| format!("versione non valida: {s:?}"));
        let v = Version { major: n(nums[0])?, minor: n(nums[1])?, patch: n(nums[2])?, pre };
        if v.pre.iter().any(String::is_empty) {
            bail!("versione non valida: {s:?}");
        }
        Ok(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch)).then_with(|| match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&o.pre) {
                    let c = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        _ => a.cmp(b),
                    };
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                self.pre.len().cmp(&o.pre.len())
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// `latest` è più recente di `installed`.
pub fn newer(latest: &str, installed: &str) -> Result<bool> {
    Ok(Version::parse(latest)? > Version::parse(installed)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn confronto() {
        assert!(v("0.2.0") > v("0.1.0"));
        assert!(v("0.10.0") > v("0.9.9"), "numeri, non testo");
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.1") > v("0.1.0"));
        assert_eq!(v("v0.1.0"), v("0.1.0"));
        assert_eq!(v("0.1.0+abc"), v("0.1.0"));
        assert!(v("0.2.0-rc.1") < v("0.2.0"), "pre-release prima della finale");
        assert!(v("0.2.0-rc.1") > v("0.1.9"));
        assert!(v("0.2.0-rc.2") < v("0.2.0-rc.10"));
        assert!(v("0.2.0-alpha") < v("0.2.0-beta"));
        assert!(v("0.2.0-1") < v("0.2.0-alpha"), "numerico prima del testo");
        assert!(v("0.2.0-rc") < v("0.2.0-rc.1"));
        assert_eq!(v("0.2.0-rc.1").to_string(), "0.2.0-rc.1");
        assert!(newer("v0.2.0", "0.1.0").unwrap());
        assert!(!newer("v0.1.0", "0.1.0").unwrap());
        assert!(!newer("0.0.9", "0.1.0").unwrap());
        for bad in ["", "1", "1.2", "1.2.3.4", "a.b.c", "1.2.x", "1.2.3-", "1.2.3-a..b"] {
            assert!(Version::parse(bad).is_err(), "{bad:?}");
        }
    }
}
