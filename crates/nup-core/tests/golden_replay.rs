#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use nup_core::{
    NupReader, ProgressInfo, VerifyProgressInfo, extract, extract_single_file, get_dir_children,
    inspect, search_files, verify,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixture_path(name: &str) -> PathBuf {
    let root = repo_root();
    if name == "sample_game" {
        root.join("sample_game.nup")
    } else {
        root.join("tests_golden")
            .join("fixtures")
            .join(format!("{name}.nup"))
    }
}

fn pstr(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn normalize_path_token(raw: &str, root: &Path) -> String {
    let norm_root = root
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    let norm_raw = raw.replace('\\', "/");
    if norm_raw
        .to_ascii_lowercase()
        .starts_with(&norm_root.to_ascii_lowercase())
    {
        format!("<REPO>{}", &norm_raw[norm_root.len()..])
    } else {
        norm_raw
    }
}

fn assert_json_eq(actual: &Value, expected: &Value, ctx: &str) {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            assert_eq!(
                a.len(),
                e.len(),
                "{ctx}: object key count mismatch.\nactual keys: {:?}\nexpected keys: {:?}",
                a.keys().collect::<Vec<_>>(),
                e.keys().collect::<Vec<_>>()
            );
            for (k, ev) in e {
                let av = a
                    .get(k)
                    .unwrap_or_else(|| panic!("{ctx}: missing key '{k}' in actual"));
                assert_json_eq(av, ev, &format!("{ctx}.{k}"));
            }
        }
        (Value::Array(a), Value::Array(e)) => {
            assert_eq!(
                a.len(),
                e.len(),
                "{ctx}: array length mismatch ({} vs {})",
                a.len(),
                e.len()
            );
            for (i, (av, ev)) in a.iter().zip(e.iter()).enumerate() {
                assert_json_eq(av, ev, &format!("{ctx}[{i}]"));
            }
        }
        (Value::Number(an), Value::Number(en)) => {
            if let (Some(au), Some(eu)) = (an.as_u64(), en.as_u64()) {
                assert_eq!(au, eu, "{ctx}: u64 mismatch");
            } else if let (Some(ai), Some(ei)) = (an.as_i64(), en.as_i64()) {
                assert_eq!(ai, ei, "{ctx}: i64 mismatch");
            } else if let (Some(af), Some(ef)) = (an.as_f64(), en.as_f64()) {
                assert!(
                    (af - ef).abs() <= 1e-6,
                    "{ctx}: f64 mismatch ({af} vs {ef})"
                );
            } else {
                panic!("{ctx}: incomparable numbers {an} vs {en}");
            }
        }
        _ => assert_eq!(actual, expected, "{ctx}: value mismatch"),
    }
}

const VALID_FIXTURES: &[&str] = &["sample_game", "base_only", "multi_pkg_dlc", "edge_cases"];

#[test]
fn golden_replay_inspect_all_fixtures() {
    let root = repo_root();
    for &name in VALID_FIXTURES {
        let nup_path = fixture_path(name);
        let golden_path = root
            .join("tests_golden")
            .join("engine")
            .join(format!("{name}.inspect.json"));
        let expected: Value =
            serde_json::from_str(&fs::read_to_string(&golden_path).unwrap()).unwrap();

        let mut actual = inspect(&nup_path).expect("inspect failed");
        for key in [
            "file_path",
            "filePath",
            "default_dest_dir",
            "defaultDestDir",
        ] {
            if let Some(s) = actual[key].as_str() {
                actual[key] = Value::String(normalize_path_token(s, &root));
            }
        }

        assert_json_eq(&actual, &expected, &format!("inspect({name})"));
    }
}

#[test]
fn golden_replay_tree_all_fixtures() {
    let root = repo_root();
    for &name in VALID_FIXTURES {
        let nup_path = fixture_path(name);
        let golden_path = root
            .join("tests_golden")
            .join("engine")
            .join(format!("{name}.tree.json"));
        let expected: Value =
            serde_json::from_str(&fs::read_to_string(&golden_path).unwrap()).unwrap();

        let reader = NupReader::open(&nup_path).unwrap();
        let expected_dirs = expected["dirs"].as_array().unwrap();
        assert_eq!(reader.dirs.len(), expected_dirs.len(), "dirCount({name})");

        for (i, exp_dir) in expected_dirs.iter().enumerate() {
            let actual = get_dir_children(&nup_path, i as u32).unwrap();
            assert_json_eq(&actual, exp_dir, &format!("tree({name}, dir={i})"));
        }
    }
}

#[test]
fn golden_replay_search_all_fixtures() {
    let root = repo_root();
    for &name in VALID_FIXTURES {
        let nup_path = fixture_path(name);
        let golden_path = root
            .join("tests_golden")
            .join("engine")
            .join(format!("{name}.search.json"));
        let expected: Value =
            serde_json::from_str(&fs::read_to_string(&golden_path).unwrap()).unwrap();

        for q_entry in expected["queries"].as_array().unwrap() {
            let query = q_entry["query"].as_str().unwrap();
            let actual = search_files(&nup_path, query, 500).unwrap();
            let actual_val = Value::Array(actual);
            assert_json_eq(
                &actual_val,
                &q_entry["results"],
                &format!("search({name}, q={query:?})"),
            );
        }
    }
}

#[test]
fn golden_replay_extract_all_fixtures() {
    let root = repo_root();
    let cancel = AtomicBool::new(false);

    for &name in VALID_FIXTURES {
        let nup_path = fixture_path(name);
        let nup_s = pstr(&nup_path);
        let golden_path = root
            .join("tests_golden")
            .join("engine")
            .join(format!("{name}.extract.json"));
        let expected: Value =
            serde_json::from_str(&fs::read_to_string(&golden_path).unwrap()).unwrap();

        for mode_entry in expected["modes"].as_array().unwrap() {
            let label = mode_entry["label"].as_str().unwrap();
            let mode = mode_entry["mode"].as_str().unwrap();
            let pkg_idx = mode_entry["packageIndex"].as_u64().map(|v| v as usize);
            let subtree = mode_entry["subtreePath"].as_str();

            let tmp = tempfile::tempdir().unwrap();
            let mut events: Vec<ProgressInfo> = Vec::new();
            extract(
                &nup_s,
                &pstr(tmp.path()),
                mode,
                pkg_idx,
                subtree,
                |p| events.push(p),
                &cancel,
            )
            .unwrap_or_else(|e| panic!("extract({name}, {label}) failed: {e}"));

            let mut actual_files: Vec<(String, u64, String)> = Vec::new();
            collect_files_recursive(tmp.path(), tmp.path(), &mut actual_files);
            actual_files.sort_by(|a, b| a.0.cmp(&b.0));

            let exp_files = mode_entry["files"].as_array().unwrap();
            assert_eq!(
                actual_files.len(),
                exp_files.len(),
                "extract({name}, {label}) file count mismatch"
            );

            for ((rel, sz, sha), ef) in actual_files.iter().zip(exp_files.iter()) {
                assert_eq!(rel, ef["relPath"].as_str().unwrap(), "{name}/{label} path");
                assert_eq!(
                    *sz,
                    ef["size"].as_u64().unwrap(),
                    "{name}/{label}/{rel} size"
                );
                assert_eq!(
                    sha,
                    ef["sha256"].as_str().unwrap(),
                    "{name}/{label}/{rel} sha256"
                );
            }

            let first = events.first().expect("expected starting event");
            let exp_first = &mode_entry["firstStatus"];
            assert_eq!(first.status, exp_first["status"].as_str().unwrap());
            assert_eq!(first.current as u64, exp_first["current"].as_u64().unwrap());
            assert_eq!(first.total as u64, exp_first["total"].as_u64().unwrap());
            assert_eq!(
                first.bytes_written,
                exp_first["bytesWritten"].as_u64().unwrap()
            );
            assert_eq!(first.total_bytes, exp_first["totalBytes"].as_u64().unwrap());

            let last = events.last().expect("expected completed event");
            let exp_last = &mode_entry["lastStatus"];
            assert_eq!(last.status, exp_last["status"].as_str().unwrap());
            assert_eq!(last.current as u64, exp_last["current"].as_u64().unwrap());
            assert_eq!(last.total as u64, exp_last["total"].as_u64().unwrap());
            assert_eq!(
                last.bytes_written,
                exp_last["bytesWritten"].as_u64().unwrap()
            );
            assert_eq!(last.total_bytes, exp_last["totalBytes"].as_u64().unwrap());
        }
    }
}

fn collect_files_recursive(base: &Path, dir: &Path, out: &mut Vec<(String, u64, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(base, &path, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(&path).unwrap();
            let digest = Sha256::digest(&bytes);
            let mut hex = String::with_capacity(64);
            for b in digest {
                use std::fmt::Write;
                let _ = write!(&mut hex, "{b:02x}");
            }
            out.push((rel, bytes.len() as u64, hex));
        }
    }
}

#[test]
fn golden_replay_verify_all_fixtures() {
    let root = repo_root();
    let cancel = AtomicBool::new(false);

    for &name in VALID_FIXTURES {
        let nup_path = fixture_path(name);
        let nup_s = pstr(&nup_path);
        let golden_path = root
            .join("tests_golden")
            .join("engine")
            .join(format!("{name}.verify.json"));
        let expected: Value =
            serde_json::from_str(&fs::read_to_string(&golden_path).unwrap()).unwrap();

        for (key, deep) in [("fast", false), ("deep", true)] {
            let mut final_res: Option<VerifyProgressInfo> = None;
            verify(
                &nup_s,
                deep,
                |p| {
                    if p.status == "done" {
                        final_res = Some(p);
                    }
                },
                &cancel,
            )
            .unwrap();

            let res = final_res.expect("missing done event");
            let exp = &expected[key];
            assert_eq!(res.valid, exp["valid"].as_bool().unwrap(), "{name}/{key}");
            assert_eq!(
                res.total_files_checked as u64,
                exp["totalFilesChecked"].as_u64().unwrap(),
                "{name}/{key}"
            );
            assert_eq!(
                res.problems.len(),
                exp["problems"].as_array().unwrap().len(),
                "{name}/{key}"
            );
        }
    }
}

#[test]
fn golden_replay_errors() {
    let root = repo_root();
    let sample = fixture_path("sample_game");
    let sample_s = pstr(&sample);
    let corrupt_dir = root.join("tests_golden").join("fixtures").join("corrupt");
    let golden_path = root.join("tests_golden").join("engine").join("errors.json");
    let expected: Value = serde_json::from_str(&fs::read_to_string(&golden_path).unwrap()).unwrap();
    let cancel = AtomicBool::new(false);

    for case in expected["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let threw = case["threw"].as_bool().unwrap();

        match id {
            "corrupt_bad_extent_hash_deep_verify" => {
                let mut final_res: Option<VerifyProgressInfo> = None;
                verify(
                    &pstr(&corrupt_dir.join("bad_extent_hash.nup")),
                    true,
                    |p| {
                        if p.status == "done" {
                            final_res = Some(p);
                        }
                    },
                    &cancel,
                )
                .unwrap();
                let res = final_res.unwrap();
                assert_eq!(res.valid, case["valid"].as_bool().unwrap());
                assert_eq!(
                    res.problems.len(),
                    case["problems"].as_array().unwrap().len()
                );
            }
            "corrupt_unknown_codec_deep_verify" => {
                let mut final_res: Option<VerifyProgressInfo> = None;
                verify(
                    &pstr(&corrupt_dir.join("unknown_codec.nup")),
                    true,
                    |p| {
                        if p.status == "done" {
                            final_res = Some(p);
                        }
                    },
                    &cancel,
                )
                .unwrap();
                let res = final_res.unwrap();
                assert_eq!(res.valid, case["valid"].as_bool().unwrap());
                assert_eq!(
                    res.problems.len(),
                    case["problems"].as_array().unwrap().len()
                );
                assert!(res.problems[0].contains("Unknown codec: 99"));
            }
            s if s.starts_with("corrupt_") && s.ends_with("_inspect") => {
                let cname = &s["corrupt_".len()..s.len() - "_inspect".len()];
                let res = inspect(corrupt_dir.join(format!("{cname}.nup")));
                assert_eq!(res.is_err(), threw, "case {id}");
            }
            "oob_dir_index" => {
                assert!(get_dir_children(&sample, 999_999).is_err());
            }
            "empty_nup_path" => {
                assert!(extract("", "out", "all", None, None, |_| {}, &cancel).is_err());
            }
            "empty_dest_path" => {
                assert!(extract(&sample_s, "", "all", None, None, |_| {}, &cancel).is_err());
            }
            "missing_file_inspect" => {
                assert!(inspect(Path::new("does_not_exist_nup_golden_xyz.nup")).is_err());
            }
            "invalid_package_index" => {
                let tmp = tempfile::tempdir().unwrap();
                assert!(
                    extract(
                        &sample_s,
                        &pstr(tmp.path()),
                        "package",
                        Some(99),
                        None,
                        |_| {},
                        &cancel
                    )
                    .is_err()
                );
            }
            "missing_subtree" => {
                let tmp = tempfile::tempdir().unwrap();
                assert!(
                    extract(
                        &sample_s,
                        &pstr(tmp.path()),
                        "subtree",
                        Some(0),
                        Some("romfs/does_not_exist"),
                        |_| {},
                        &cancel
                    )
                    .is_err()
                );
            }
            "single_file_oob" => {
                let tmp = tempfile::tempdir().unwrap();
                assert!(
                    extract_single_file(&sample_s, 9999, &pstr(&tmp.path().join("out.bin")))
                        .is_err()
                );
            }
            other => panic!("Unhandled error golden case: {other}"),
        }
    }
}
