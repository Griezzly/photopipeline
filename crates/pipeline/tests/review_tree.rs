use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use image::{ImageBuffer, Rgb};
use pipeline::{
    build_review_tree,
    catalog::{Catalog, DuplicateMember},
    config::{KeeperStrategy, LinkType, OutputConfig},
    defect::DefectFlag,
    ingest::{ExifData, FileFormat, IngestedFile},
};
use tempfile::TempDir;

// ── helpers (this test file is self-contained) ──────────────────────────────

fn make_synthetic_jpg(path: &Path, r: u8, g: u8, b: u8) {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(32, 32, |_, _| Rgb([r, g, b]));
    img.save(path).expect("save test jpg");
}

fn out_cfg(link_type: LinkType) -> OutputConfig {
    OutputConfig {
        review_tree: "<library>/_review".into(),
        link_type,
        keeper_strategy: KeeperStrategy::Iqa,
    }
}

/// Insert a file with a synthetic JPEG on disk; return its file_id.
/// `captured_at` of None -> NULL exif (unknown-date).
fn add_file(
    catalog: &Catalog,
    lib: &Path,
    name: &str,
    hash: u128,
    captured_at: Option<i64>,
) -> (i64, PathBuf) {
    let path = lib.join(name);
    make_synthetic_jpg(&path, (hash & 0xff) as u8, 0, 0);
    let file = IngestedFile {
        path: path.clone(),
        content_hash: hash,
        size: 100,
        mtime_ns: 1,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let exif = captured_at.map(|c| ExifData {
        captured_at: Some(c),
        ..Default::default()
    });
    let ids = catalog.flush_batch(&[(file, exif)]).unwrap();
    (ids[0], path)
}

fn count_originals(lib: &Path) -> usize {
    fs::read_dir(lib)
        .unwrap()
        .filter(|e| {
            let p = e.as_ref().unwrap().path();
            p.is_file() && p.extension().map(|x| x == "jpg").unwrap_or(false)
        })
        .count()
}

/// Create a duplicate group with `keeper_id` as the suggested keeper and the
/// given `others`; returns the new group id (for computing the group folder name).
fn make_group(catalog: &Catalog, keeper_id: i64, others: &[i64]) -> i64 {
    let gid = catalog.insert_duplicate_group("time+embed", 0).unwrap();
    let mut members = vec![DuplicateMember {
        file_id: keeper_id,
        is_suggested_keeper: true,
        quality_score: 1.0,
    }];
    for &o in others {
        members.push(DuplicateMember {
            file_id: o,
            is_suggested_keeper: false,
            quality_score: 0.5,
        });
    }
    catalog.insert_duplicate_members(gid, &members).unwrap();
    gid
}

const JUNE_2023: i64 = 1_686_830_400; // 2023-06-15

// ── tests ───────────────────────────────────────────────────────────────────

#[test]
fn rejected_and_uncertain_and_duplicates_are_built() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (blur_id, blur_path) = add_file(&catalog, lib.path(), "blur.jpg", 1, Some(JUNE_2023));
    let (lowq_id, _) = add_file(&catalog, lib.path(), "lowq.jpg", 2, Some(JUNE_2023));
    let (unc_id, _) = add_file(&catalog, lib.path(), "uncertain.jpg", 3, Some(JUNE_2023));

    catalog
        .upsert_defect_flag(
            blur_id,
            &DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.8,
                reason: "r".into(),
            },
        )
        .unwrap();
    catalog
        .upsert_defect_flag(
            lowq_id,
            &DefectFlag {
                flag_type: "low_iqa".into(),
                confidence: 0.9,
                reason: "r".into(),
            },
        )
        .unwrap();
    catalog
        .upsert_defect_flag(
            unc_id,
            &DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.4,
                reason: "r".into(),
            },
        )
        .unwrap();

    // Duplicate group: keeper + one other.
    let (keeper_id, keeper_path) = add_file(&catalog, lib.path(), "keeper.jpg", 4, Some(JUNE_2023));
    let (other_id, _) = add_file(&catalog, lib.path(), "other.jpg", 5, Some(JUNE_2023));
    let gid = make_group(&catalog, keeper_id, &[other_id]);
    let group_dir = format!("duplicates/group_{gid:05}_2023-06-15");

    let out = lib.path().join("_review");
    let report =
        build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert!(
        report.links_created >= 4,
        "created {}",
        report.links_created
    );
    assert_eq!(report.groups, 1);

    // rejected/blur/2023-06/blur.jpg is a symlink resolving to the original.
    let blur_link = out.join("rejected/blur/2023-06/blur.jpg");
    assert!(fs::symlink_metadata(&blur_link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::canonicalize(&blur_link).unwrap(),
        fs::canonicalize(&blur_path).unwrap()
    );

    // low_iqa -> low_quality.
    assert!(out.join("rejected/low_quality/2023-06/lowq.jpg").exists());

    // low-confidence blur -> uncertain.
    assert!(out.join("uncertain/2023-06/uncertain.jpg").exists());

    // duplicates keeper + others.
    let kdir = out.join(&group_dir).join("_keeper");
    assert!(kdir.join("keeper.jpg").exists());
    assert_eq!(
        fs::canonicalize(kdir.join("keeper.jpg")).unwrap(),
        fs::canonicalize(&keeper_path).unwrap()
    );
    assert!(out.join(&group_dir).join("_others/other.jpg").exists());

    // README present.
    assert!(out.join("README.txt").exists());
}

#[test]
fn non_destructive_originals_unchanged() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, orig) = add_file(&catalog, lib.path(), "a.jpg", 1, Some(JUNE_2023));
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.8,
                reason: "r".into(),
            },
        )
        .unwrap();

    let before = count_originals(lib.path());
    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    // _review is a subdir of lib; count only the top-level .jpg originals (none moved).
    assert_eq!(count_originals(lib.path()), before);

    // Delete a symlink -> original survives.
    let link = out.join("rejected/blur/2023-06/a.jpg");
    fs::remove_file(&link).unwrap();
    assert!(
        orig.exists(),
        "deleting a symlink must not delete the original"
    );
}

#[test]
fn regenerate_rebuilds_after_manual_deletion() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    for i in 0..4i64 {
        let (id, _) = add_file(
            &catalog,
            lib.path(),
            &format!("f{i}.jpg"),
            i as u128 + 1,
            Some(JUNE_2023),
        );
        catalog
            .upsert_defect_flag(
                id,
                &DefectFlag {
                    flag_type: "blur".into(),
                    confidence: 0.8,
                    reason: "r".into(),
                },
            )
            .unwrap();
    }
    let out = lib.path().join("_review");
    let r1 = build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert_eq!(r1.links_created, 4);

    // Delete half the links by removing the whole month dir.
    fs::remove_dir_all(out.join("rejected/blur/2023-06")).unwrap();

    // --regenerate rebuilds all.
    let r2 = build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], true).unwrap();
    assert_eq!(
        r2.links_created, 4,
        "regenerate should recreate all 4 links"
    );
    let n = fs::read_dir(out.join("rejected/blur/2023-06"))
        .unwrap()
        .count();
    assert_eq!(n, 4);
}

#[test]
fn incremental_prunes_stale_links() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, _) = add_file(&catalog, lib.path(), "a.jpg", 1, Some(JUNE_2023));
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.8,
                reason: "r".into(),
            },
        )
        .unwrap();
    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();

    // Plant a stale symlink the planner would never produce.
    let stale_dir = out.join("rejected/blur/2099-01");
    fs::create_dir_all(&stale_dir).unwrap();
    std::os::unix::fs::symlink(lib.path().join("a.jpg"), stale_dir.join("ghost.jpg")).unwrap();

    let r = build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert_eq!(r.links_removed, 1, "stale link should be pruned");
    assert!(!stale_dir.join("ghost.jpg").exists());
}

#[test]
fn basename_collisions_get_distinct_links() {
    let lib = TempDir::new().unwrap();
    let sub = lib.path().join("subdir");
    fs::create_dir_all(&sub).unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    // Two different originals with the SAME basename, same month/category.
    let p1 = lib.path().join("IMG_1.jpg");
    make_synthetic_jpg(&p1, 10, 0, 0);
    let p2 = sub.join("IMG_1.jpg");
    make_synthetic_jpg(&p2, 20, 0, 0);
    let id1 = catalog
        .flush_batch(&[(
            IngestedFile {
                path: p1.clone(),
                content_hash: 1,
                size: 1,
                mtime_ns: 1,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            },
            Some(ExifData {
                captured_at: Some(JUNE_2023),
                ..Default::default()
            }),
        )])
        .unwrap()[0];
    let id2 = catalog
        .flush_batch(&[(
            IngestedFile {
                path: p2.clone(),
                content_hash: 2,
                size: 1,
                mtime_ns: 1,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            },
            Some(ExifData {
                captured_at: Some(JUNE_2023),
                ..Default::default()
            }),
        )])
        .unwrap()[0];
    for id in [id1, id2] {
        catalog
            .upsert_defect_flag(
                id,
                &DefectFlag {
                    flag_type: "blur".into(),
                    confidence: 0.8,
                    reason: "r".into(),
                },
            )
            .unwrap();
    }

    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();

    let dir = out.join("rejected/blur/2023-06");
    let names: HashSet<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains("IMG_1.jpg"));
    assert!(names.contains("IMG_1 (2).jpg"), "got {names:?}");
}

#[test]
fn include_filter_limits_categories() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, _) = add_file(&catalog, lib.path(), "a.jpg", 1, Some(JUNE_2023));
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.8,
                reason: "r".into(),
            },
        )
        .unwrap();
    let (kid, _) = add_file(&catalog, lib.path(), "k.jpg", 2, Some(JUNE_2023));
    let (oid, _) = add_file(&catalog, lib.path(), "o.jpg", 3, Some(JUNE_2023));
    let gid = make_group(&catalog, kid, &[oid]);

    let out = lib.path().join("_review");
    build_review_tree(
        &catalog,
        &out,
        &out_cfg(LinkType::Symlink),
        &["duplicates".to_string()],
        false,
    )
    .unwrap();

    assert!(
        !out.join("rejected").exists(),
        "rejected excluded by filter"
    );
    assert!(out
        .join(format!(
            "duplicates/group_{gid:05}_2023-06-15/_keeper/k.jpg"
        ))
        .exists());
}

#[test]
fn unknown_date_file_goes_to_unknown_date_folder() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, _) = add_file(&catalog, lib.path(), "nodate.jpg", 1, None);
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.8,
                reason: "r".into(),
            },
        )
        .unwrap();
    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert!(out.join("rejected/blur/unknown-date/nodate.jpg").exists());
}
