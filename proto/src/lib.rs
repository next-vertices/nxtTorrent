//! Messages gRPC et types partagés master/agent.
#![forbid(unsafe_code)]

pub mod certinfo;
pub mod create;
pub mod fsutil;
pub mod infohash;
pub mod install;
pub mod metainfo;

/// Taille maximale d'un message master -> agent (un `DesiredState` peut transporter des fichiers .torrent).
pub const MAX_MASTER_MSG: usize = 16 * 1024 * 1024;
/// Un `DesiredState` est découpé en messages d'au plus cette taille (marge sous `MAX_MASTER_MSG`).
pub const DESIRED_CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub mod pairing;
pub mod release;
pub mod relpath;
pub mod version;

#[allow(clippy::all, clippy::pedantic)]
pub mod pb {
    tonic::include_proto!("orchestrator.v1");
}

/// Version du protocole applicatif (incrémentée à chaque rupture).
pub const PROTOCOL_VERSION: u32 = 1;

/// Plateformes pour lesquelles des binaires sont publiés.
pub const PLATFORMS: [&str; 3] = ["linux-amd64", "linux-arm64", "windows-amd64"];

/// Plateforme de ce binaire, si des releases la couvrent.
pub fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-amd64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("windows", "x86_64") => Some("windows-amd64"),
        _ => None,
    }
}

/// Préfixe de l'identité d'un nœud dans le SAN du certificat client.
pub const NODE_SAN_PREFIX: &str = "node-";

pub fn node_san(node_id: &uuid_like::NodeId) -> String {
    format!("{NODE_SAN_PREFIX}{node_id}")
}

/// Extrait le node_id d'un SAN `node-<uuid>`.
pub fn parse_node_san(san: &str) -> Option<String> {
    let id = san.strip_prefix(NODE_SAN_PREFIX)?;
    uuid_like::NodeId::parse(id).map(|n| n.to_string())
}

pub mod uuid_like {
    //! Identifiant de nœud : UUID textuel validé strictement (sans dépendre de `uuid` ici).
    use std::fmt;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct NodeId(String);

    impl NodeId {
        pub fn parse(s: &str) -> Option<Self> {
            let b = s.as_bytes();
            if b.len() != 36 {
                return None;
            }
            let ok = b.iter().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => *c == b'-',
                _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
            });
            ok.then(|| NodeId(s.to_owned()))
        }
    }

    impl fmt::Display for NodeId {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn san_roundtrip() {
        let id = uuid_like::NodeId::parse("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let san = node_san(&id);
        assert_eq!(
            parse_node_san(&san).as_deref(),
            Some("123e4567-e89b-12d3-a456-426614174000")
        );
        assert!(parse_node_san("node-../etc").is_none());
        assert!(parse_node_san("host-123").is_none());
    }
}
