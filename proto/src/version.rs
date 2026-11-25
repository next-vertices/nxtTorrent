//! Versions sémantiques (`0.1.0-rc.3`), partagées par le master (notification des nouvelles versions) et l'agent
//! (vérification d'un manifeste de release) : une seule définition de « plus récent ».
use std::cmp::Ordering;

/// Identifiant de pré-version (`rc`, `3`). Les numériques précèdent les alphanumériques (semver).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Num(u64),
    Alpha(String),
}

/// Version sémantique : suffisant pour ordonner nos propres étiquettes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: (u64, u64, u64),
    pre: Vec<Ident>,
}

impl Version {
    /// Version finale `major.minor.patch`.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            core: (major, minor, patch),
            pre: Vec::new(),
        }
    }

    /// `v0.1.0-rc.3` ou `0.1.0-rc.3` ; les métadonnées de build (`+…`) sont ignorées.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => p
                .split('.')
                .map(|x| {
                    if x.is_empty() {
                        None
                    } else if x.bytes().all(|b| b.is_ascii_digit()) {
                        x.parse().ok().map(Ident::Num)
                    } else if x.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                        Some(Ident::Alpha(x.to_owned()))
                    } else {
                        None
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self {
            core: (major, minor, patch),
            pre,
        })
    }

    /// Pré-version (`-rc.N`…) ?
    pub fn is_pre(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.core.0, self.core.1, self.core.2)?;
        if self.is_pre() {
            let pre: Vec<String> = self
                .pre
                .iter()
                .map(|i| match i {
                    Ident::Num(n) => n.to_string(),
                    Ident::Alpha(a) => a.clone(),
                })
                .collect();
            write!(f, "-{}", pre.join("."))?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            // Une version finale est plus récente que ses pré-versions (0.1.0 > 0.1.0-rc.9).
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("version invalide : {s}"))
    }

    #[test]
    fn versions_are_ordered_like_semver() {
        // Les pré-versions s'ordonnent numériquement (rc.10 après rc.3), la finale passe après ses pré-versions.
        let order = [
            "0.1.0-rc.1",
            "0.1.0-rc.2",
            "0.1.0-rc.3",
            "0.1.0-rc.10",
            "0.1.0",
            "0.1.1-rc.1",
            "0.1.1",
            "0.2.0",
            "1.0.0",
        ];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("v0.1.0-rc.3"), v("0.1.0-rc.3"), "préfixe v ignoré");
        assert_eq!(
            v("0.1.0+build.7"),
            v("0.1.0"),
            "métadonnées de build ignorées"
        );
        assert!(v("0.1.0-alpha") < v("0.1.0-alpha.1"), "plus court d'abord");
        assert!(
            v("0.1.0-1") < v("0.1.0-alpha"),
            "numérique avant alphanumérique"
        );
        assert_eq!(v("0.1.0-rc.3").to_string(), "0.1.0-rc.3");
    }

    #[test]
    fn malformed_versions_are_refused() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "a.b.c",
            "1.2.3-",
            "1.2.3-rc..1",
            "1.2.3-r_c",
            "v",
            "-1.2.3",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
    }
}
