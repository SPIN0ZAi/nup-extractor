#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use nup_core::path_safety::validate_and_join_extraction_path;
use nup_core::{
    NupConstants, NupReader, PackOptions, PackProgressInfo, ProgressInfo, VerifyProgressInfo,
    check_disk_space, extract, extract_single_file, get_dir_children, inspect, pack, search_files,
    verify,
};
use proptest::prelude::*;
use sha2::{Digest, Sha256};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn sample_nup_path() -> PathBuf {
    repo_root().join("sample_game.nup")
}

fn pstr(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn test_superblock_parsing_valid_container() {
    let path = sample_nup_path();
    assert!(path.exists());
    let reader = NupReader::open(&path).unwrap();
    assert_eq!(
        NupConstants::HEADER_SIZE as u16,
        reader.superblock.header_size
    );
    assert_eq!(1, reader.superblock.header_version);
    assert!(reader.superblock.file_size > 0);
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        reader.superblock.file_size
    );
    assert_eq!(2, reader.superblock.package_count);
    assert!(!reader.superblock.formatted_uuid().is_empty());
}

#[test]
fn test_superblock_validation_rejects_bad_magic() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("bad.nup");
    let mut data = vec![0u8; NupConstants::HEADER_SIZE];
    data[..4].copy_from_slice(b"BAD!");
    fs::write(&p, data).unwrap();
    assert!(NupReader::open(&p).is_err());
}

#[test]
fn test_toc_parsing_packages_and_nodes() {
    let reader = NupReader::open(sample_nup_path()).unwrap();
    assert_eq!(2, reader.packages.len());
    let base = &reader.packages[0];
    assert_eq!(NupConstants::KIND_BASE, base.kind);
    assert_eq!("base", base.kind_name());
    assert_eq!("0100123456789ABC", base.title_id_hex());

    let upd = &reader.packages[1];
    assert_eq!(NupConstants::KIND_UPDATE, upd.kind);
    assert_eq!("update", upd.kind_name());

    assert!(!reader.dirs.is_empty());
    assert!(!reader.files.is_empty());
    assert!(!reader.children.is_empty());
}

#[test]
fn test_metadata_extraction() {
    let reader = NupReader::open(sample_nup_path()).unwrap();
    let meta = reader.get_package_metadata(&reader.packages[0]);

    assert_eq!(
        std::str::from_utf8(&meta[&NupConstants::TLV_TITLE_NAME]).unwrap(),
        "Sample NX Adventure"
    );
    assert_eq!(
        std::str::from_utf8(&meta[&NupConstants::TLV_PUBLISHER]).unwrap(),
        "Nintendo"
    );
    assert_eq!(
        std::str::from_utf8(&meta[&NupConstants::TLV_DISPLAY_VERSION]).unwrap(),
        "1.1.0"
    );
    assert!(!meta[&NupConstants::TLV_ICON].is_empty());
}

#[test]
fn test_inspect_returns_complete_contract() {
    let data = inspect(sample_nup_path()).unwrap();
    assert_eq!(data["file_name"].as_str().unwrap(), "sample_game.nup");
    assert_eq!(data["packages"].as_array().unwrap().len(), 2);
    assert_eq!(
        data["primary_meta"]["title_name"].as_str().unwrap(),
        "Sample NX Adventure"
    );
    assert_eq!(
        data["primary_meta"]["publisher"].as_str().unwrap(),
        "Nintendo"
    );
}

#[test]
fn test_tree_children_resolution() {
    let reader = NupReader::open(sample_nup_path()).unwrap();
    let tree = get_dir_children(sample_nup_path(), reader.packages[0].root_dir).unwrap();
    let children = tree["children"].as_array().unwrap();
    assert!(children.iter().any(|c| c["name"] == "exefs"));
    assert!(children.iter().any(|c| c["name"] == "romfs"));
}

#[test]
fn test_file_search() {
    let results = search_files(sample_nup_path(), "level", 500).unwrap();
    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r["name"] == "level01.dat"));

    let empty = search_files(sample_nup_path(), "non_existent_file_name_12345", 500).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn test_decompression_raw_and_zstd() {
    let reader = NupReader::open(sample_nup_path()).unwrap();
    let raw_idx = reader
        .files
        .iter()
        .position(|f| f.codec == NupConstants::CODEC_RAW)
        .expect("raw file");
    let zstd_idx = reader
        .files
        .iter()
        .position(|f| f.codec == NupConstants::CODEC_ZSTD)
        .expect("zstd file");

    for idx in [raw_idx, zstd_idx] {
        let data = reader.read_file_data(idx).unwrap();
        assert_eq!(data.len() as u64, reader.files[idx].size);
        let hash: [u8; 32] = Sha256::digest(&data).into();
        assert_eq!(hash, reader.files[idx].sha256);
    }
}

#[test]
fn test_full_package_and_subtree_extraction() {
    let cancel = AtomicBool::new(false);
    let sample_s = pstr(&sample_nup_path());

    // All
    let tmp = tempfile::tempdir().unwrap();
    extract(
        &sample_s,
        &pstr(tmp.path()),
        "all",
        None,
        None,
        |_| {},
        &cancel,
    )
    .unwrap();
    assert_eq!(
        fs::metadata(tmp.path().join("base/exefs/main"))
            .unwrap()
            .len(),
        2144
    );
    assert_eq!(
        fs::metadata(tmp.path().join("base/romfs/data/config.json"))
            .unwrap()
            .len(),
        92
    );
    assert_eq!(
        fs::metadata(tmp.path().join("base/romfs/data/levels/level01.dat"))
            .unwrap()
            .len(),
        8000
    );
    assert_eq!(
        fs::metadata(tmp.path().join("base/romfs/textures/ui_sheet.bin"))
            .unwrap()
            .len(),
        13000
    );
    assert_eq!(
        fs::metadata(tmp.path().join("updates/65536/romfs/data/patch_notes.txt"))
            .unwrap()
            .len(),
        77
    );

    // Package 0
    let tmp_pkg = tempfile::tempdir().unwrap();
    extract(
        &sample_s,
        &pstr(tmp_pkg.path()),
        "package",
        Some(0),
        None,
        |_| {},
        &cancel,
    )
    .unwrap();
    assert!(tmp_pkg.path().join("exefs/main").exists());
    assert!(
        !tmp_pkg
            .path()
            .join("updates/65536/romfs/data/patch_notes.txt")
            .exists()
    );

    // Subtree
    let tmp_sub = tempfile::tempdir().unwrap();
    extract(
        &sample_s,
        &pstr(tmp_sub.path()),
        "subtree",
        Some(0),
        Some("romfs/data"),
        |_| {},
        &cancel,
    )
    .unwrap();
    assert!(tmp_sub.path().join("config.json").exists());
    assert!(tmp_sub.path().join("levels/level01.dat").exists());
    assert!(!tmp_sub.path().join("main").exists());
}

#[test]
fn test_integrity_verification_and_extent_corruption_detection() {
    let cancel = AtomicBool::new(false);
    let sample_s = pstr(&sample_nup_path());
    let mut final_res: Option<VerifyProgressInfo> = None;
    verify(
        &sample_s,
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
    assert!(res.valid);
    assert!(res.problems.is_empty());
    assert_eq!(res.total_files_checked, 5);

    // Corrupt a byte directly inside file 0's extent and ensure deep verify detects SHA-256 mismatch
    let tmp = tempfile::tempdir().unwrap();
    let corrupt_path = tmp.path().join("corrupt_extent.nup");
    let mut bytes = fs::read(sample_nup_path()).unwrap();
    let reader = NupReader::open(sample_nup_path()).unwrap();
    let target_offset = reader.files[0].extent_offset as usize;
    bytes[target_offset] ^= 0xFF;
    fs::write(&corrupt_path, &bytes).unwrap();

    let mut corrupt_res: Option<VerifyProgressInfo> = None;
    verify(
        &pstr(&corrupt_path),
        true,
        |p| {
            if p.status == "done" {
                corrupt_res = Some(p);
            }
        },
        &cancel,
    )
    .unwrap();
    let cres = corrupt_res.unwrap();
    assert!(!cres.valid);
    assert!(!cres.problems.is_empty());
}

#[test]
fn test_serialization_contract_produces_camel_case() {
    let info = ProgressInfo {
        status: "extracting".into(),
        current: 5,
        total: 10,
        percent: 50.0,
        file_name: "romfs/test.dat".into(),
        file_size: 2048,
        bytes_written: 1024,
        total_bytes: 2048,
        dest_dir: Some(r"C:\test".to_string()),
        error: None,
        elapsed_seconds: 1.25,
    };
    let json = serde_json::to_string(&serde_json::json!({
        "type": "extract_progress",
        "info": info,
    }))
    .unwrap();
    assert!(json.contains("\"percent\":50.0"));
    assert!(json.contains("\"fileName\":\"romfs/test.dat\""));
    assert!(json.contains("\"bytesWritten\":1024"));
    assert!(json.contains("\"totalBytes\":2048"));
    assert!(json.contains("\"destDir\":"));
    assert!(json.contains("\"elapsedSeconds\":1.25"));
}

#[test]
fn test_streaming_read_and_single_file_extract() {
    let sample_s = pstr(&sample_nup_path());
    let reader = NupReader::open(sample_nup_path()).unwrap();
    for (i, f) in reader.files.iter().enumerate() {
        let mut buf = Vec::new();
        reader.extract_file_to_writer(i, &mut buf).unwrap();
        assert_eq!(buf.len() as u64, f.size);
        let hash: [u8; 32] = Sha256::digest(&buf).into();
        assert_eq!(hash, f.sha256);
    }

    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("single.bin");
    extract_single_file(&sample_s, 0, &pstr(&out)).unwrap();
    let bytes = fs::read(&out).unwrap();
    assert_eq!(bytes.len() as u64, reader.files[0].size);
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    assert_eq!(hash, reader.files[0].sha256);
}

#[test]
fn test_check_disk_space() {
    let cur = pstr(&std::env::current_dir().unwrap());
    let info = check_disk_space(&cur, 1024 * 1024);
    assert!(!info.drive_letter.is_empty());
    assert!(info.total_bytes > 0);
    assert!(info.free_bytes > 0);
    assert!(info.has_sufficient_space);
    assert!(!info.free_formatted.is_empty());

    let huge = info.total_bytes.saturating_add(100 * 1024 * 1024 * 1024);
    let low = check_disk_space(&cur, huge);
    assert!(!low.has_sufficient_space);
}

#[test]
fn test_writer_pack_raw_multipackage_and_dlc_round_trip() {
    let cancel = AtomicBool::new(false);
    let tmp = tempfile::tempdir().unwrap();

    // 1. Single package pack + read back
    let src = tmp.path().join("source_game");
    fs::create_dir_all(src.join("exefs")).unwrap();
    fs::create_dir_all(src.join("romfs/data")).unwrap();
    fs::create_dir_all(src.join("romfs/audio")).unwrap();

    let npdm = b"HEADER_NPDM_DATA_1234567890".to_vec();
    let subsdk: Vec<u8> = (0..4096).map(|i| ((i * 37 + 11) % 256) as u8).collect();
    let json_text = r#"{"game":"test","version":"1.0.0"}"#;
    let audio: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();

    fs::write(src.join("exefs/main.npdm"), &npdm).unwrap();
    fs::write(src.join("exefs/subsdk0"), &subsdk).unwrap();
    fs::write(src.join("romfs/data/config.json"), json_text).unwrap();
    fs::write(src.join("romfs/audio/bgm.ogg"), &audio).unwrap();

    let out_nup = tmp.path().join("output_game.nup");
    let mut last_pack: Option<PackProgressInfo> = None;
    pack(
        &PackOptions {
            source_directory: pstr(&src),
            output_file_path: pstr(&out_nup),
            title_name: "Chrono Pack Adventure".into(),
            publisher: "Antigravity Studios".into(),
            title_id: 0x0100_ABCD_1234_EF00,
            version_string: "2.4.1".into(),
            compression_level: 3,
            icon_path: None,
        },
        |p| last_pack = Some(p),
        &cancel,
    )
    .unwrap();
    assert_eq!(last_pack.unwrap().status, "done");

    let reader = NupReader::open(&out_nup).unwrap();
    assert_eq!(reader.files.len(), 4);
    let mut map: HashMap<String, Vec<u8>> = HashMap::new();
    for (i, f) in reader.files.iter().enumerate() {
        map.insert(f.name.clone(), reader.read_file_data(i).unwrap());
    }
    assert_eq!(map["main.npdm"], npdm);
    assert_eq!(map["subsdk0"], subsdk);
    assert_eq!(map["config.json"], json_text.as_bytes());
    assert_eq!(map["bgm.ogg"], audio);

    // 2. Raw mode
    let src_raw = tmp.path().join("raw_src");
    fs::create_dir_all(&src_raw).unwrap();
    fs::write(src_raw.join("test.txt"), b"RAW_UNCOMPRESSED_PAYLOAD_TEST").unwrap();
    let out_raw = tmp.path().join("raw.nup");
    pack(
        &PackOptions {
            source_directory: pstr(&src_raw),
            output_file_path: pstr(&out_raw),
            compression_level: 0,
            ..PackOptions::default()
        },
        |_| {},
        &cancel,
    )
    .unwrap();
    let r_raw = NupReader::open(&out_raw).unwrap();
    assert_eq!(r_raw.files.len(), 1);
    assert_eq!(r_raw.files[0].codec, NupConstants::CODEC_RAW);

    // 3. Multi-package Base + Update + DLC
    let bundle = tmp.path().join("bundle");
    fs::create_dir_all(bundle.join("base/romfs")).unwrap();
    fs::create_dir_all(bundle.join("updates/131072/romfs")).unwrap();
    fs::create_dir_all(bundle.join("dlc/0100123456789001/0/romfs")).unwrap();
    fs::write(bundle.join("base/romfs/main_data.bin"), "Base game data").unwrap();
    fs::write(
        bundle.join("updates/131072/romfs/update_data.bin"),
        "Update 2.0 data",
    )
    .unwrap();
    fs::write(
        bundle.join("dlc/0100123456789001/0/romfs/dlc_costume.bin"),
        "DLC costume data",
    )
    .unwrap();

    let dlc_nup = tmp.path().join("dlc.nup");
    pack(
        &PackOptions {
            source_directory: pstr(&bundle),
            output_file_path: pstr(&dlc_nup),
            title_name: "Adventure Game".into(),
            title_id: 0x0100_1234_5678_9000,
            version_string: "1.0.0".into(),
            compression_level: 3,
            ..PackOptions::default()
        },
        |_| {},
        &cancel,
    )
    .unwrap();
    let r_dlc = NupReader::open(&dlc_nup).unwrap();
    assert_eq!(r_dlc.packages.len(), 3);
    assert_eq!(r_dlc.packages[0].kind, NupConstants::KIND_BASE);
    assert_eq!(r_dlc.packages[1].kind, NupConstants::KIND_UPDATE);
    assert_eq!(r_dlc.packages[1].version, 131072);
    assert_eq!(r_dlc.packages[2].kind, NupConstants::KIND_ADDON);
    assert_eq!(r_dlc.packages[2].title_id_hex(), "0100123456789001");
}

#[test]
fn test_path_safety_rejects_traversal_and_reserved_names() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(validate_and_join_extraction_path(tmp.path(), "romfs/data/ok.bin").is_ok());
    for bad in [
        "..",
        ".",
        "romfs/../escape.bin",
        "C:evil",
        "CON",
        "con.txt",
        "COM1",
        "LPT9.dat",
        "NUL",
        "trailing.",
        "trailing ",
    ] {
        assert!(
            validate_and_join_extraction_path(tmp.path(), bad).is_err(),
            "expected rejection for '{bad}'"
        );
    }
}

#[test]
fn test_decompression_bomb_guard_rejects_oversized_zstd_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("bomb_src");
    fs::create_dir_all(&src).unwrap();
    let payload = vec![b'A'; 65536];
    fs::write(src.join("bomb.bin"), &payload).unwrap();

    let nup = tmp.path().join("bomb.nup");
    let cancel = AtomicBool::new(false);
    pack(
        &PackOptions {
            source_directory: pstr(&src),
            output_file_path: pstr(&nup),
            compression_level: 3,
            ..PackOptions::default()
        },
        |_| {},
        &cancel,
    )
    .unwrap();

    let mut raw = fs::read(&nup).unwrap();
    let toc_off = u64::from_le_bytes(raw[8..16].try_into().unwrap()) as usize;
    let toc_sz = u64::from_le_bytes(raw[16..24].try_into().unwrap()) as usize;
    let file_entry_off = toc_off + 48 + NupConstants::PKG_SIZE + NupConstants::DIR_SIZE;
    raw[file_entry_off + 8..file_entry_off + 16].copy_from_slice(&16u64.to_le_bytes());
    let new_toc_hash: [u8; 32] = Sha256::digest(&raw[toc_off..toc_off + toc_sz]).into();
    raw[24..56].copy_from_slice(&new_toc_hash);
    fs::write(&nup, &raw).unwrap();

    let reader = NupReader::open(&nup).unwrap();
    let err = reader.read_file_data(0).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.to_ascii_lowercase().contains("exceeded declared size"),
        "unexpected error: {msg}"
    );
}

#[test]
fn test_large_file_count_10k_tiny_files_stress() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("many_src");
    let romfs = src.join("romfs");
    fs::create_dir_all(&romfs).unwrap();

    for b in 0..20 {
        let dir = romfs.join(format!("b{b:02}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..500 {
            fs::write(dir.join(format!("f{f:04}.txt")), b"x").unwrap();
        }
    }

    let nup = tmp.path().join("many.nup");
    let cancel = AtomicBool::new(false);
    pack(
        &PackOptions {
            source_directory: pstr(&src),
            output_file_path: pstr(&nup),
            compression_level: 0,
            ..PackOptions::default()
        },
        |_| {},
        &cancel,
    )
    .unwrap();

    let reader = NupReader::open(&nup).unwrap();
    assert_eq!(reader.files.len(), 10_000);

    let mut verify_done: Option<VerifyProgressInfo> = None;
    verify(
        &pstr(&nup),
        false,
        |p| {
            if p.status == "done" {
                verify_done = Some(p);
            }
        },
        &cancel,
    )
    .unwrap();
    assert!(verify_done.unwrap().valid);
}

#[test]
fn test_writer_lichii_compat_zero_byte_npdm_nacp_fcs_and_sort_order() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("Switch2Game");
    fs::create_dir_all(src.join("exefs")).unwrap();
    fs::create_dir_all(src.join("romfs/DirUpper")).unwrap();
    fs::create_dir_all(src.join("romfs/dir_lower")).unwrap();

    // Minimal ExeFS main (NSO0 + 32-byte Build ID at 0x40..0x60)
    let mut main_nso = vec![0u8; 0x100];
    main_nso[0..4].copy_from_slice(b"NSO0");
    main_nso[0x40..0x60].fill(0x5A);
    fs::write(src.join("exefs/main"), &main_nso).unwrap();

    // Minimal main.npdm with Switch 2 Program ID at ACI0 + 0x10
    let switch2_pid: u64 = 0x0400_74A0_1BF1_2000;
    let mut npdm = vec![0u8; 0x100];
    npdm[0x80..0x84].copy_from_slice(b"ACI0");
    npdm[0x90..0x98].copy_from_slice(&switch2_pid.to_le_bytes());
    fs::write(src.join("exefs/main.npdm"), &npdm).unwrap();

    // Valid 16 KiB NACP + sidecar icon
    let mut nacp = vec![0u8; 0x4000];
    let title_b = b"Bananza Test";
    nacp[..title_b.len()].copy_from_slice(title_b);
    let pub_b = b"Nintendo";
    nacp[0x200..0x200 + pub_b.len()].copy_from_slice(pub_b);
    let ver_b = b"3.2.0";
    nacp[0x3060..0x3060 + ver_b.len()].copy_from_slice(ver_b);
    fs::write(src.join("control.nacp"), &nacp).unwrap();
    let fake_icon = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    fs::write(src.join("icon_AmericanEnglish.dat"), &fake_icon).unwrap();

    // Mixed children inside romfs/ including empty files, compressible data, incompressible data, duplicate data
    let compressible = vec![b'Z'; 16_384];
    let mut incompressible = vec![0u8; 4096];
    let mut prng: u64 = 0x9E37_79B9_7F4A_7C15;
    let (incomp_chunks, _) = incompressible.as_chunks_mut::<8>();
    for chunk in incomp_chunks {
        prng ^= prng << 13;
        prng ^= prng >> 7;
        prng ^= prng << 17;
        *chunk = prng.to_le_bytes();
    }
    fs::write(src.join("romfs/DirUpper/nested.txt"), b"upper dir").unwrap();
    fs::write(src.join("romfs/dir_lower/nested.txt"), b"lower dir").unwrap();
    fs::write(src.join("romfs/Z_comp.json"), &compressible).unwrap();
    fs::write(src.join("romfs/Z_comp_dup.json"), &compressible).unwrap();
    fs::write(src.join("romfs/_empty_mid.bin"), []).unwrap();
    fs::write(src.join("romfs/a_incomp.zs"), &incompressible).unwrap();
    fs::write(src.join("romfs/z_empty_end.bin"), []).unwrap();

    let out_nup = tmp.path().join("bananza.nup");
    let cancel = AtomicBool::new(false);
    pack(
        &PackOptions {
            source_directory: pstr(&src),
            output_file_path: pstr(&out_nup),
            title_name: String::new(),
            publisher: String::new(),
            // Simulate UI placeholder sent directly: real NPDM ID must still win!
            title_id: 0x0100_0000_0000_1000,
            version_string: String::new(),
            compression_level: 3,
            icon_path: None,
        },
        |_| {},
        &cancel,
    )
    .unwrap();

    let reader = NupReader::open(&out_nup).unwrap();
    assert_eq!(reader.packages.len(), 1);
    let pkg = &reader.packages[0];
    assert_eq!(pkg.title_id, switch2_pid);
    assert_ne!(pkg.exe_fs_dir, NupConstants::NO_DIR);
    assert_ne!(pkg.rom_fs_dir, NupConstants::NO_DIR);

    // Metadata block sits after blob region (compatible with both SPEC §5 and Lichii UpdateMetadata)
    let toc_end = reader.superblock.toc_offset + reader.superblock.toc_size;
    assert!(pkg.meta_offset >= toc_end);
    assert_eq!(
        pkg.meta_offset + u64::from(pkg.meta_size),
        reader.superblock.file_size
    );

    let meta = reader.get_package_metadata(pkg);
    assert_eq!(meta[&NupConstants::TLV_NPDM], npdm);
    assert_eq!(meta[&NupConstants::TLV_NACP], nacp);
    assert_eq!(meta[&NupConstants::TLV_ICON], fake_icon);
    assert_eq!(meta[&NupConstants::TLV_TITLE_NAME], title_b);
    assert_eq!(meta[&NupConstants::TLV_PUBLISHER], pub_b);
    assert_eq!(meta[&NupConstants::TLV_DISPLAY_VERSION], ver_b);
    assert_eq!(meta[&NupConstants::TLV_BUILD_ID], vec![0x5Au8; 32]);

    // SPEC §3.3: every directory's children sorted in bytewise UTF-8 lexicographic order (dirs & files mixed!)
    let romfs_children: Vec<String> = reader
        .get_children(pkg.rom_fs_dir)
        .into_iter()
        .map(|(_, _, name)| name)
        .collect();
    let mut expected_sorted = romfs_children.clone();
    expected_sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(romfs_children, expected_sorted);
    assert_eq!(
        romfs_children,
        vec![
            "DirUpper",
            "Z_comp.json",
            "Z_comp_dup.json",
            "_empty_mid.bin",
            "a_incomp.zs",
            "dir_lower",
            "z_empty_end.bin",
        ]
    );

    // Zero-byte rule (SPEC §3.6 & Lichii Parse): extent_offset == 0, extent_size == 0, codec == RAW
    for empty_name in ["_empty_mid.bin", "z_empty_end.bin"] {
        let f = reader.files.iter().find(|f| f.name == empty_name).unwrap();
        assert_eq!(f.size, 0);
        assert_eq!(
            f.extent_offset, 0,
            "{empty_name} must have extent_offset == 0"
        );
        assert_eq!(f.extent_size, 0);
        assert_eq!(f.codec, NupConstants::CODEC_RAW);
    }

    // Zstd frame MUST embed FrameContentSize matching uncompressed size (SPEC §4.1)
    let comp_idx = reader
        .files
        .iter()
        .position(|f| f.name == "Z_comp.json")
        .unwrap();
    let dup_idx = reader
        .files
        .iter()
        .position(|f| f.name == "Z_comp_dup.json")
        .unwrap();
    assert_eq!(reader.files[comp_idx].codec, NupConstants::CODEC_ZSTD);
    assert_eq!(
        reader.files[comp_idx].extent_offset,
        reader.files[dup_idx].extent_offset
    );
    let (_, zstd_extent) = reader.extent_slice(comp_idx).unwrap();
    let fcs = zstd::zstd_safe::get_frame_content_size(zstd_extent).unwrap();
    assert_eq!(fcs, Some(compressible.len() as u64));

    // Incompressible .zs falls back to RAW without corrupting anything
    let zs_f = reader
        .files
        .iter()
        .find(|f| f.name == "a_incomp.zs")
        .unwrap();
    assert_eq!(zs_f.codec, NupConstants::CODEC_RAW);
    assert_eq!(zs_f.extent_size, incompressible.len() as u64);

    // Deep verify passes
    let mut verify_done: Option<VerifyProgressInfo> = None;
    verify(
        &pstr(&out_nup),
        true,
        |p| {
            if p.status == "done" {
                verify_done = Some(p);
            }
        },
        &cancel,
    )
    .unwrap();
    assert!(verify_done.unwrap().valid);
}

#[test]
fn test_writer_base_at_root_with_updates_and_dlc_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("RootGameWithDlc");
    fs::create_dir_all(src.join("exefs")).unwrap();
    fs::create_dir_all(src.join("romfs")).unwrap();
    fs::create_dir_all(src.join("updates/65536/romfs")).unwrap();
    fs::create_dir_all(src.join("dlc/0100ABCDEF123401/0/romfs")).unwrap();

    let mut npdm = vec![0u8; 0x100];
    npdm[0x80..0x84].copy_from_slice(b"ACI0");
    npdm[0x90..0x98].copy_from_slice(&0x0100_ABCD_EF12_3000u64.to_le_bytes());
    fs::write(src.join("exefs/main"), b"main_exec").unwrap();
    fs::write(src.join("exefs/main.npdm"), &npdm).unwrap();
    fs::write(src.join("romfs/base.dat"), b"base payload").unwrap();
    fs::write(src.join("updates/65536/romfs/upd.dat"), b"upd payload").unwrap();
    fs::write(
        src.join("dlc/0100ABCDEF123401/0/romfs/dlc.dat"),
        b"dlc payload",
    )
    .unwrap();

    let out = tmp.path().join("combined.nup");
    let cancel = AtomicBool::new(false);
    pack(
        &PackOptions {
            source_directory: pstr(&src),
            output_file_path: pstr(&out),
            ..PackOptions::default()
        },
        |_| {},
        &cancel,
    )
    .unwrap();

    let reader = NupReader::open(&out).unwrap();
    assert_eq!(reader.packages.len(), 3);
    assert_eq!(reader.packages[0].kind, NupConstants::KIND_BASE);
    assert_eq!(reader.packages[0].title_id_hex(), "0100ABCDEF123000");
    assert_eq!(reader.packages[1].kind, NupConstants::KIND_UPDATE);
    assert_eq!(reader.packages[1].flags, 0);
    assert_eq!(reader.packages[2].kind, NupConstants::KIND_ADDON);
    assert_eq!(reader.packages[2].title_id_hex(), "0100ABCDEF123401");

    // Base root must NOT contain updates/ or dlc/ subdirectories
    let base_children: Vec<String> = reader
        .get_children(reader.packages[0].root_dir)
        .into_iter()
        .map(|(_, _, n)| n)
        .collect();
    assert_eq!(base_children, vec!["exefs", "romfs"]);
}

#[test]
fn test_writer_streaming_large_file_path_compressible_and_incompressible() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("large_src");
    fs::create_dir_all(src.join("romfs")).unwrap();

    // > 16 MiB compressible and > 16 MiB incompressible (`.zs` simulation)
    let large_size = 17 * 1024 * 1024usize;
    let large_comp = vec![0x42u8; large_size];
    let mut large_incomp = vec![0u8; large_size];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let (chunks, _) = large_incomp.as_chunks_mut::<8>();
    for chunk in chunks {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *chunk = state.to_le_bytes();
    }
    fs::write(src.join("romfs/big_comp.dat"), &large_comp).unwrap();
    fs::write(src.join("romfs/big_incomp.zs"), &large_incomp).unwrap();

    let out = tmp.path().join("large.nup");
    let cancel = AtomicBool::new(false);
    pack(
        &PackOptions {
            source_directory: pstr(&src),
            output_file_path: pstr(&out),
            compression_level: 3,
            ..PackOptions::default()
        },
        |_| {},
        &cancel,
    )
    .unwrap();

    let reader = NupReader::open(&out).unwrap();
    assert_eq!(reader.files.len(), 2);

    let comp_idx = reader
        .files
        .iter()
        .position(|f| f.name == "big_comp.dat")
        .unwrap();
    let incomp_idx = reader
        .files
        .iter()
        .position(|f| f.name == "big_incomp.zs")
        .unwrap();
    assert_eq!(reader.files[comp_idx].codec, NupConstants::CODEC_ZSTD);
    assert_eq!(reader.files[incomp_idx].codec, NupConstants::CODEC_RAW);
    assert_eq!(reader.files[incomp_idx].extent_size, large_size as u64);

    // Streaming Zstd path MUST also embed exact FrameContentSize
    let (_, z_ext) = reader.extent_slice(comp_idx).unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(z_ext).unwrap(),
        Some(large_size as u64)
    );

    let mut verify_done: Option<VerifyProgressInfo> = None;
    verify(
        &pstr(&out),
        true,
        |p| {
            if p.status == "done" {
                verify_done = Some(p);
            }
        },
        &cancel,
    )
    .unwrap();
    assert!(verify_done.unwrap().valid);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn fuzz_random_bytes_never_panic_on_open(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("fuzz.nup");
        fs::write(&p, &data).unwrap();
        let _ = NupReader::open(&p);
    }

    #[test]
    fn fuzz_mutated_valid_nup_never_panics(
        offset in 0usize..3570,
        xor_mask in 1u8..=255u8
    ) {
        let mut bytes = fs::read(sample_nup_path()).unwrap();
        if offset < bytes.len() {
            bytes[offset] ^= xor_mask;
        }
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("mut.nup");
        fs::write(&p, &bytes).unwrap();
        if let Ok(reader) = NupReader::open(&p) {
            let _ = inspect(&p);
            for i in 0..reader.files.len().min(8) {
                let _ = reader.read_file_data(i);
            }
        }
    }
}
