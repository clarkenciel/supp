fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";
    println!("cargo:rerun-if-changed={proto_root}");
    let fds = protox::compile(
        ["supp/sync/v1/sync.proto", "supp/api/v1/api.proto"],
        [proto_root],
    )?;
    prost_build::Config::new().compile_fds(fds)?;
    Ok(())
}
