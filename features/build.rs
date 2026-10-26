use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        out.push(path.to_owned());
    } else if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            files(&entry.path(), out);
        }
    }
}

/// The digest of this crate's own source, which keys and stamps what its features produce (the
/// engine's own digest covers the engine).
fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut paths = Vec::new();
    for input in ["Cargo.toml", "build.rs", "src"] {
        let input = root.join(input);
        println!("cargo:rerun-if-changed={}", input.display());
        files(&input, &mut paths);
    }
    let mut paths: Vec<(String, PathBuf)> = paths
        .into_iter()
        .map(|path| (path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), path))
        .collect();
    paths.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    for (relative, path) in paths {
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(fs::read(path).unwrap());
        digest.update([0]);
    }
    let mut features: Vec<String> = env::vars().filter_map(|(name, _)| name.strip_prefix("CARGO_FEATURE_").map(str::to_owned)).collect();
    features.sort();
    for feature in features {
        digest.update(feature.as_bytes());
        digest.update([0]);
    }
    println!("cargo:rustc-env=LEGAL_STRUCTURE_FEATURES_SHA256={:x}", digest.finalize());
}