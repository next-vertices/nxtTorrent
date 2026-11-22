//! Validation syntaxique des chemins relatifs (partagée master/agent).
//! La résolution réelle (symlinks) reste de la responsabilité de l'agent.

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error("chemin invalide")]
    Invalid,
    #[error("chemin absolu interdit")]
    Absolute,
    #[error("composant '..' interdit")]
    Traversal,
    #[error("le chemin sort de la racine")]
    Escape,
    #[error("racine inaccessible")]
    Root,
}

/// Découpe `rel` en composants. Rejette : trop long, contrôles, `:` (lecteurs Windows),
/// chemins absolus, `..`. Une chaîne vide (ou `.`) donne zéro composant (= la racine).
pub fn split_relative(rel: &str) -> Result<Vec<&str>, PathError> {
    if rel.len() > 1024 || rel.chars().any(|c| c.is_control() || c == ':') {
        return Err(PathError::Invalid);
    }
    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err(PathError::Absolute);
    }
    let mut parts = Vec::new();
    for p in rel.split(['/', '\\']) {
        match p {
            "" | "." => {}
            ".." => return Err(PathError::Traversal),
            p => parts.push(p),
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_rejects() {
        assert_eq!(split_relative("a//b/./c").unwrap(), vec!["a", "b", "c"]);
        assert!(split_relative("").unwrap().is_empty());
        assert_eq!(split_relative("/x"), Err(PathError::Absolute));
        assert_eq!(split_relative("a/../b"), Err(PathError::Traversal));
        assert_eq!(split_relative("C:\\x"), Err(PathError::Invalid));
    }
}
