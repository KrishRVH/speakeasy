//! Embed Windows application resources before linking.

#![forbid(unsafe_code)]

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=../../packaging/windows/speakeasy.rc");
        println!("cargo:rerun-if-changed=../../packaging/windows/speakeasy.ico");
        println!("cargo:rerun-if-changed=../../packaging/windows/speakeasy.manifest");
        if let Err(error) = embed_resource::compile_for_everything(
            "../../packaging/windows/speakeasy.rc",
            embed_resource::NONE,
        )
        .manifest_required()
        {
            // Cargo reports a build error without a panic or a partial executable.
            println!("cargo::error=Cannot embed the Windows manifest: {error}");
        }
    }
}
