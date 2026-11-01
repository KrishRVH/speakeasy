#![forbid(unsafe_code)]

use std::io::{self, Write as _};

const RESOURCE_DIR: &str = "../../packaging/windows";

fn main() -> io::Result<()> {
    let mut output = io::stdout().lock();

    writeln!(output, "cargo::rerun-if-changed=build.rs")?;
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    for resource in ["speakeasy.rc", "speakeasy.ico", "speakeasy.manifest"] {
        writeln!(output, "cargo::rerun-if-changed={RESOURCE_DIR}/{resource}")?;
    }
    let embedded = embed_resource::compile_for_everything(
        format!("{RESOURCE_DIR}/speakeasy.rc"),
        embed_resource::NONE,
    )
    .manifest_required();
    if let Err(error) = embedded {
        writeln!(
            output,
            "cargo::error=Cannot embed Windows resources: {error}"
        )?;
    }
    Ok(())
}
