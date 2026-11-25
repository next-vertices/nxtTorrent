//! `release-tool manifest` / `release-tool verify` : voir la documentation de la bibliothèque.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use proto::release::TRUSTED_KEYS;
use proto::version::Version;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Fabrique et vérifie le manifeste de release signé")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Écrit le manifeste (à signer ensuite) des fichiers nxt-* de `--dist`.
    Manifest {
        #[arg(long)]
        dist: PathBuf,
        /// Version de la release (ex. 0.1.0-rc.3).
        #[arg(long)]
        version: String,
        /// Identifiant de la clé qui signera (8 hexadécimaux) ; par défaut, celui de la clé de release compilée.
        #[arg(long)]
        key_id: Option<String>,
        #[arg(long, default_value_t = 180)]
        valid_days: i64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Vérifie un manifeste signé comme le fera l'agent ; code de sortie non nul au moindre doute.
    Verify {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        sig: PathBuf,
        /// Vérifie aussi les fichiers de ce dossier contre le manifeste.
        #[arg(long)]
        dist: Option<PathBuf>,
        /// Refuse une version qui ne serait pas plus récente que celle-ci.
        #[arg(long)]
        installed: Option<String>,
        /// Clé publique brute (64 hexadécimaux) à utiliser À LA PLACE des clés compilées : répétition avec une clé
        /// jetable uniquement. La release publiée se vérifie toujours avec les clés compilées.
        #[arg(long)]
        trusted_key: Option<String>,
    },
}

fn now() -> Result<i64> {
    Ok(i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Manifest {
            dist,
            version,
            key_id,
            valid_days,
            out,
        } => {
            let key_id = key_id.unwrap_or_else(|| TRUSTED_KEYS[0].id());
            let bytes = release_tool::make_manifest(&dist, &version, &key_id, valid_days, now()?)?;
            std::fs::write(&out, &bytes)
                .with_context(|| format!("écriture de {}", out.display()))?;
            println!(
                "manifeste écrit : {} ({} octets, clé {key_id})",
                out.display(),
                bytes.len()
            );
        }
        Cmd::Verify {
            manifest,
            sig,
            dist,
            installed,
            trusted_key,
        } => {
            let installed = installed
                .map(|v| Version::parse(&v).context("version installée illisible"))
                .transpose()?;
            let custom;
            let keys = match trusted_key {
                Some(h) => {
                    custom = [release_tool::trusted_key_from_hex(&h)?];
                    &custom[..]
                }
                None => TRUSTED_KEYS,
            };
            let v = release_tool::verify_release(
                &std::fs::read(&manifest)
                    .with_context(|| format!("lecture de {}", manifest.display()))?,
                &std::fs::read(&sig).with_context(|| format!("lecture de {}", sig.display()))?,
                keys,
                now()?,
                dist.as_deref(),
                installed.as_ref(),
            )?;
            println!(
                "OK : version {} (clé {}), {} fichier(s), valable jusqu'au {}",
                v.version,
                v.key_id,
                v.assets().count(),
                v.expires_at
            );
        }
    }
    Ok(())
}
