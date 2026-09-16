use std::path::PathBuf;

fn main() {
    // The face Slint embeds by default reads as technical for a device that
    // lives in a living room. Slint resolves fonts at compile time, so the
    // choice is made here; both weights are present so font-weight: 600 lands
    // on a real face instead of a synthesised one.
    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fonts");
    println!("cargo:rerun-if-changed={}", fonts.display());
    unsafe {
        std::env::set_var("SLINT_FONT_PATH", &fonts);
        std::env::set_var("SLINT_DEFAULT_FONT", fonts.join("Lato-Regular.ttf"));
    }

    // There is no font system on the device. SDF keeps one scalable glyph set
    // per Lato face instead of a bitmap at every UI size, so runtime Cyrillic
    // coverage does not multiply the font payload across all those sizes.
    // The conversion dependencies run on the build host, not on the remote.
    let cfg = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
        .with_sdf_fonts(true);
    slint_build::compile_with_config("ui/app.slint", cfg).expect("compiling ui/app.slint");
}
