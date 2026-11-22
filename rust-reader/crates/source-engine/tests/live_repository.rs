//! Manual check against the real Aidoku community index.
//!
//! Network tests are not part of the normal suite. Run explicitly:
//! `YEJIAN_LIVE_REPO=1 cargo test -p yejian-sources --test live_repository -- --ignored --nocapture`

use yejian_sources::{SourceCatalog, SourceEngine, SourceInstaller};

#[test]
#[ignore = "network access: set YEJIAN_LIVE_REPO=1"]
fn installs_and_loads_a_community_source() {
    if std::env::var_os("YEJIAN_LIVE_REPO").is_none() {
        return;
    }
    let dir = tempfile::tempdir().expect("temporary catalogue");
    let installer = SourceInstaller::new(SourceCatalog::new(dir.path())).expect("installer");
    let index = installer
        .fetch_index("https://aidoku-community.github.io/sources/index.min.json")
        .expect("fetch index");
    println!("index: {} sources", index.sources.len());
    assert!(index.sources.len() > 100, "community index should be large");

    let installed = installer
        .install(&index, "multi.mangaball")
        .expect("install one source");
    println!(
        "installed: {} v{} at {}",
        installed.manifest.name,
        installed.manifest.version,
        installed.directory.display()
    );
    assert!(installed.wasm_path.is_file());

    let engine = SourceEngine::load_installed(&installed).expect("load wasm engine");
    drop(engine);
}
