//! L'interface web (`ui/dist`) est embarquée dans le binaire. Le dossier doit exister à la compilation :
//! on le crée vide si l'interface n'a pas été construite (le master sert alors une page de secours).
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/dist");
    if let Err(e) = std::fs::create_dir_all(&dist) {
        println!("cargo:warning=impossible de créer {} : {e}", dist.display());
    }
    println!("cargo:rerun-if-changed=../ui/dist");
}
