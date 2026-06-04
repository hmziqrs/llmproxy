//! Build script: emit compile-time metadata (target triple, git SHA)
//! via `vergen-gix` for use in the `/version` endpoint.

use vergen_gix::{Emitter, GixBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // VERGEN_GIT_SHA (short)
    let gix = GixBuilder::default().sha(true).build()?;
    Emitter::default().add_instructions(&gix)?.emit()?;

    // TARGET is available in build scripts (but not in the compiled crate).
    println!(
        "cargo:rustc-env=VERGEN_CARGO_TARGET_TRIPLE={}",
        std::env::var("TARGET")?
    );

    Ok(())
}
