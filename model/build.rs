// Dependents' build scripts read where legal-layout's sources are (DEP_LEGAL_STRUCTURE_MODEL_LAYOUT), wherever Cargo
// has this repository (a checkout or a git checkout): legal-pdf-parser keys its extraction stage on them. legal-layout
// is this crate's sibling in the repository, the path dependency above.
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let model = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo::metadata=layout={}", model.parent().unwrap().join("layout").display());
}
