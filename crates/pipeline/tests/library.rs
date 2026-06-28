use pipeline::library::{
    find_library_for_file, library_key, list_libraries, open_existing_library,
    open_or_create_library, LibraryRoots,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn temp_roots(d: &TempDir) -> LibraryRoots {
    LibraryRoots {
        data: d.path().join("data"),
        cache: d.path().join("cache"),
    }
}

#[test]
fn key_is_stable_and_path_normalized() {
    let d = TempDir::new().unwrap();
    let lib = d.path().join("Photos");
    std::fs::create_dir_all(&lib).unwrap();
    // Same folder via a trailing slash → same key (both canonicalize).
    let with_slash = PathBuf::from(format!("{}/", lib.display()));
    assert_eq!(library_key(&lib), library_key(&with_slash));
    // A different folder → different key.
    let other = d.path().join("Other");
    std::fs::create_dir_all(&other).unwrap();
    assert_ne!(library_key(&lib), library_key(&other));
}

#[test]
fn open_existing_is_none_until_created() {
    let d = TempDir::new().unwrap();
    let roots = temp_roots(&d);
    let folder = d.path().join("lib");
    std::fs::create_dir_all(&folder).unwrap();
    assert!(open_existing_library(&roots, &folder).unwrap().is_none());

    let lib = open_or_create_library(&roots, &folder).unwrap();
    // Catalog landed under the data root, cache under the cache root.
    assert!(d.path().join("data/libraries").exists());
    assert!(d.path().join("cache/libraries").exists());
    // Meta records the canonical folder path.
    let (fp, _, last) = lib.catalog.library_meta().unwrap().unwrap();
    assert_eq!(
        fp,
        std::fs::canonicalize(&folder).unwrap().to_string_lossy()
    );
    assert_eq!(last, None);
    assert!(open_existing_library(&roots, &folder).unwrap().is_some());
}

#[test]
fn list_and_find() {
    let d = TempDir::new().unwrap();
    let roots = temp_roots(&d);
    let a = d.path().join("a");
    let b = d.path().join("b/inner");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    open_or_create_library(&roots, &a).unwrap();
    open_or_create_library(&roots, &b).unwrap();

    let libs = list_libraries(&roots).unwrap();
    assert_eq!(libs.len(), 2);
    assert!(libs
        .iter()
        .any(|l| l.folder == std::fs::canonicalize(&a).unwrap()));

    // A file under `a` resolves to `a`; the deepest library wins.
    let f = a.join("sub/x.jpg");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"x").unwrap();
    let found = find_library_for_file(&roots, &f).unwrap().unwrap();
    assert_eq!(found, std::fs::canonicalize(&a).unwrap());

    // A file under no library → None.
    let outside = d.path().join("nope/y.jpg");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, b"y").unwrap();
    assert!(find_library_for_file(&roots, &outside).unwrap().is_none());
}
