fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);
    tonic_prost_build::configure()
        .build_transport(false)
        .compile_protos(&["proto/orchestrator.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/orchestrator.proto");
    Ok(())
}
