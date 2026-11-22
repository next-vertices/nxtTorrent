//! Client Torznab contre un faux indexeur HTTP : recherche, test de clé, téléchargement et garde-fous.
use master::torznab::{Torznab, TorznabError};
use testkit::torznab::MockTorznab;

#[tokio::test]
async fn search_download_and_key_check_over_http() {
    let idx = MockTorznab::start("sesame").await;
    let t1 = testkit::torrent::sample("Film.2024.1080p-GRP", 4096, 1);
    let t2 = testkit::torrent::sample("Film.2024.1080p-AUTRE", 4096, 2);
    idx.set_results(vec![
        ("Film.2024.1080p-GRP", t1.clone()),
        ("Film.2024.1080p-AUTRE", t2),
    ]);

    let c = Torznab::new(&idx.url(), "sesame").unwrap();
    c.test().await.unwrap();
    let items = c.search("Film 2024 1080p").await.unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(idx.searches(), vec!["Film 2024 1080p".to_owned()]);
    assert_eq!(items[0].title, "Film.2024.1080p-GRP");
    assert_eq!(items[0].size, 4096, "taille du contenu");
    // Téléchargement depuis l'hôte de l'indexeur : contenu exact.
    assert_eq!(c.download(&items[0].link).await.unwrap(), t1);
    assert_eq!(idx.downloads(), vec![0]);

    // Mauvaise clé : erreur explicite de l'indexeur, aucune donnée.
    let bad = Torznab::new(&idx.url(), "faux").unwrap();
    assert!(matches!(bad.test().await, Err(TorznabError::Auth(m)) if m.contains("credentials")));
    assert!(matches!(bad.search("x").await, Err(TorznabError::Auth(_))));
}

#[tokio::test]
async fn downloads_are_refused_outside_the_indexer_host_and_unreachable_is_reported() {
    let idx = MockTorznab::start("k").await;
    let c = Torznab::new(&idx.url(), "k").unwrap();
    for link in [
        "http://127.0.0.1:1/dl/0",
        "http://evil.test/dl/0",
        "file:///etc/passwd",
        "magnet:?xt=urn:btih:abc",
    ] {
        assert!(
            matches!(c.download(link).await, Err(TorznabError::BadUrl(_))),
            "{link}"
        );
    }
    assert!(idx.downloads().is_empty(), "rien n'a été demandé");
    // Indexeur injoignable : erreur générique, sans détail réseau ni clé.
    let down = Torznab::new("http://127.0.0.1:1", "SECRET").unwrap();
    let e = down.search("x").await.unwrap_err();
    assert!(matches!(e, TorznabError::Unreachable));
    assert!(!e.to_string().contains("SECRET"));
}
