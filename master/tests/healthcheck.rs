//! La sonde de santé (HEALTHCHECK Docker) réussit seulement si l'API répond 200 : elle interroge l'API
//! d'administration en HTTP, jamais le port TLS des agents (une connexion nue y serait un échec de poignée de main).
use std::io::{Read, Write};
use std::process::Command;

fn probe(admin_listen: &str) -> bool {
    Command::new(env!("CARGO_BIN_EXE_master"))
        .args(["healthcheck", "--admin-listen", admin_listen])
        .status()
        .unwrap()
        .success()
}

/// Serveur HTTP minimal qui répond toujours `status` ; rend son port.
fn fake_api(status: &'static str) -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut buf = [0u8; 256];
            let _ = s.read(&mut buf);
            let _ =
                s.write_all(format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n").as_bytes());
        }
    });
    port
}

#[test]
fn healthcheck_follows_the_api() {
    let ok = fake_api("200 OK");
    assert!(
        probe(&format!("127.0.0.1:{ok}")),
        "API qui répond 200 : sonde OK"
    );
    // Une adresse d'écoute « tous interfaces » est jointe via la boucle locale.
    assert!(probe(&format!("0.0.0.0:{ok}")));
    // Un serveur vivant mais malade (500) n'est pas « en bonne santé ».
    let sick = fake_api("500 Internal Server Error");
    assert!(!probe(&format!("127.0.0.1:{sick}")));
    // Port fermé / adresse invalide.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    assert!(!probe(&format!("127.0.0.1:{closed}")));
    assert!(!probe("pas-une-adresse"));
}
