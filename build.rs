//! Compile the vendored cereal capnp schemas into Rust, under a `cereal`
//! parent module. Generated files land in OUT_DIR and are `include!`d by
//! src/cereal/mod.rs. Requires the `capnp` compiler binary at build time.

fn main() {
    println!("cargo:rerun-if-changed=vendor/cereal");

    let schemas = ["log.capnp", "car.capnp", "custom.capnp", "legacy.capnp"];
    let mut cmd = capnpc::CompilerCommand::new();
    cmd.src_prefix("vendor/cereal")
        .default_parent_module(vec!["cereal".into()]);
    for s in schemas {
        cmd.file(format!("vendor/cereal/{s}"));
    }
    cmd.run().expect("compiling cereal capnp schemas failed");
}
