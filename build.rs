use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.file_name().is_some_and(|name| name == "target") {
        return;
    }
    if path.is_file() {
        out.push(path.to_owned());
    } else if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            files(&entry.path(), out);
        }
    }
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut paths = Vec::new();
    let mut inputs = vec![
        "Cargo.toml",
        "build.rs",
        "src",
        "layout/Cargo.toml",
        "layout/src",
        "model/Cargo.toml",
        "model/src",
    ];
    // The analysis service also executes the citation grammar and its compiled registries.
    if env::var_os("CARGO_FEATURE_STRUCTURE_INFERENCE").is_some()
        || env::var_os("CARGO_FEATURE_CITATOR").is_some()
    {
        inputs.extend([
            "../common-law-cite/Cargo.toml",
            "../common-law-cite/crates/legal-citations/Cargo.toml",
            "../common-law-cite/crates/legal-citations/build.rs",
            "../common-law-cite/crates/legal-citations/build_registry.rs",
            "../common-law-cite/crates/legal-citations/src",
            "../common-law-cite/crates/legal-citations/registry",
            "../common-law-cite/crates/legal-citations/vendor",
            "../common-law-cite/crates/legal-grammar/Cargo.toml",
            "../common-law-cite/crates/legal-grammar/build.rs",
            "../common-law-cite/crates/legal-grammar/src",
            "../common-law-cite/crates/legal-grammar/data",
        ]);
    }
    for input in inputs {
        let input = root.join(input);
        println!("cargo:rerun-if-changed={}", input.display());
        files(&input, &mut paths);
    }
    let mut paths = paths
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (relative, path)
        })
        .collect::<Vec<_>>();
    paths.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    for (relative, path) in paths {
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(fs::read(path).unwrap());
        digest.update([0]);
    }
    let mut features = env::vars_os()
        .filter_map(|(name, _)| {
            name.to_str()?
                .strip_prefix("CARGO_FEATURE_")
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    features.sort();
    for feature in features {
        digest.update(feature.as_bytes());
        digest.update([0]);
    }
    println!(
        "cargo:rustc-env=LEGAL_STRUCTURE_ENGINE_SHA256={:x}",
        digest.finalize()
    );
}
