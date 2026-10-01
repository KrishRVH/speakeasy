//! Embeds Windows application resources before linking. Failures surface as Cargo errors, never as
//! a panic or a partial executable.

#![forbid(unsafe_code)]

const RESOURCE_DIR: &str = "../../packaging/windows";

fn main() {
    // Declaring any input stops Cargo from rerunning this script whenever a package file changes.
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    for resource in ["speakeasy.rc", "speakeasy.ico", "speakeasy.manifest"] {
        println!("cargo::rerun-if-changed={RESOURCE_DIR}/{resource}");
    }
    let embedded = embed_resource::compile_for_everything(
        format!("{RESOURCE_DIR}/speakeasy.rc"),
        embed_resource::NONE,
    )
    .manifest_required();
    if let Err(error) = embedded {
        println!("cargo::error=Cannot embed Windows resources: {error}");
    }
}
