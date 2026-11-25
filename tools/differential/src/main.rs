#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::AtomicBool;

use nup_core::{NupReader, VerifyProgressInfo, verify};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DiffOutcome {
    open_ok: bool,
    open_err_class: Option<String>,
    package_count: usize,
    dir_count: usize,
    file_count: usize,
    verify_valid: bool,
    verify_problem_count: usize,
}

fn eval_rust(path: &Path) -> DiffOutcome {
    let cancel = AtomicBool::new(false);
    match NupReader::open(path) {
        Err(e) => DiffOutcome {
            open_ok: false,
            open_err_class: Some(classify_err(&e.to_string())),
            package_count: 0,
            dir_count: 0,
            file_count: 0,
            verify_valid: false,
            verify_problem_count: 0,
        },
        Ok(reader) => {
            let pkg_count = reader.packages.len();
            let dir_count = reader.dirs.len();
            let file_count = reader.files.len();
            drop(reader);

            let mut final_v: Option<VerifyProgressInfo> = None;
            let _ = verify(
                &path.to_string_lossy(),
                true,
                |p| {
                    if p.status == "done" {
                        final_v = Some(p);
                    }
                },
                &cancel,
            );
            let (valid, probs) = final_v
                .map(|v| (v.valid, v.problems.len()))
                .unwrap_or((false, 1));
            DiffOutcome {
                open_ok: true,
                open_err_class: None,
                package_count: pkg_count,
                dir_count,
                file_count,
                verify_valid: valid,
                verify_problem_count: probs,
            }
        }
    }
}

fn classify_err(msg: &str) -> String {
    let low = msg.to_ascii_lowercase();
    if low.contains("magic") {
        "bad_magic".into()
    } else if low.contains("too small") || low.contains("header") {
        "bad_header".into()
    } else if low.contains("size mismatch") {
        "size_mismatch".into()
    } else if low.contains("sha-256") || low.contains("hash") {
        "bad_hash".into()
    } else if low.contains("aligned") {
        "alignment".into()
    } else if low.contains("overlap") || low.contains("bounds") || low.contains("out of") {
        "bounds".into()
    } else {
        "invalid_data".into()
    }
}

struct XorShift64(u64);
impl XorShift64 {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, max: usize) -> usize {
        if max == 0 {
            0
        } else {
            (self.next() as usize) % max
        }
    }
}

fn mutate_case(seed_idx: usize, bases: &[Vec<u8>], rng: &mut XorShift64) -> Vec<u8> {
    let base = &bases[seed_idx % bases.len()];
    let mut data = base.clone();
    let strategy = seed_idx % 10;
    match strategy {
        0 => {
            let pos = rng.range(data.len());
            let bit = 1u8 << (rng.range(8));
            data[pos] ^= bit;
        }
        1 => {
            let pos = rng.range(data.len().min(128));
            data[pos] ^= (rng.range(255) + 1) as u8;
        }
        2 => {
            if data.len() >= 128 {
                let toc_off = u64::from_le_bytes(data[8..16].try_into().unwrap()) as usize;
                let toc_sz = u64::from_le_bytes(data[16..24].try_into().unwrap()) as usize;
                if toc_off + toc_sz <= data.len() && toc_sz > 0 {
                    let pos = toc_off + rng.range(toc_sz);
                    data[pos] ^= (rng.range(255) + 1) as u8;
                    let h: [u8; 32] = Sha256::digest(&data[toc_off..toc_off + toc_sz]).into();
                    data[24..56].copy_from_slice(&h);
                }
            }
        }
        3 => {
            let new_len = rng.range(data.len());
            data.truncate(new_len);
        }
        4 => {
            let extra = rng.range(64) + 1;
            for _ in 0..extra {
                data.push((rng.next() & 0xFF) as u8);
            }
        }
        5 => {
            if data.len() >= 128 {
                let blob_off = u64::from_le_bytes(data[56..64].try_into().unwrap()) as usize;
                if blob_off < data.len() {
                    let pos = blob_off + rng.range(data.len() - blob_off);
                    data[pos] ^= (rng.range(255) + 1) as u8;
                }
            }
        }
        6 => {
            if data.len() >= 128 {
                let toc_off = u64::from_le_bytes(data[8..16].try_into().unwrap()) as usize;
                let toc_sz = u64::from_le_bytes(data[16..24].try_into().unwrap()) as usize;
                if toc_off + toc_sz <= data.len() && toc_sz >= 8 {
                    let pos = toc_off + rng.range(toc_sz - 4);
                    let vals = [0u32, 1u32, 0xFFFF_FFFFu32, 0x7FFF_FFFFu32, 0x0010_0000u32];
                    let v = vals[rng.range(vals.len())];
                    data[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
                    let h: [u8; 32] = Sha256::digest(&data[toc_off..toc_off + toc_sz]).into();
                    data[24..56].copy_from_slice(&h);
                }
            }
        }
        7 => {
            if data.len() >= 128 {
                let fields = [8usize, 16, 56, 64];
                let pos = fields[rng.range(fields.len())];
                let vals = [0u64, 16, 128, data.len() as u64, u64::MAX, u64::MAX - 15];
                let v = vals[rng.range(vals.len())];
                data[pos..pos + 8].copy_from_slice(&v.to_le_bytes());
            }
        }
        8 => {
            let len = rng.range(256);
            data = (0..len).map(|_| (rng.next() & 0xFF) as u8).collect();
        }
        _ => {}
    }
    data
}

struct CSharpWorker {
    exe: PathBuf,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CSharpWorker {
    fn spawn(exe: PathBuf) -> Self {
        let mut child = Command::new(&exe)
            .arg("--diff-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("Failed to launch {}: {e}", exe.display()));
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            exe,
            child,
            stdin,
            stdout,
        }
    }

    /// Returns `Some(outcome)` on normal response, or `None` if the C# reference process crashed (e.g. 0xC0000005).
    fn eval(&mut self, path: &Path) -> Option<DiffOutcome> {
        if writeln!(self.stdin, "{}", path.display()).is_err() || self.stdin.flush().is_err() {
            self.restart();
            return None;
        }
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(n) if n > 0 => match serde_json::from_str::<DiffOutcome>(line.trim()) {
                Ok(out) => Some(out),
                Err(_) => {
                    self.restart();
                    None
                }
            },
            _ => {
                self.restart();
                None
            }
        }
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let exe = self.exe.clone();
        *self = Self::spawn(exe);
    }
}

impl Drop for CSharpWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_repo_root() -> PathBuf {
    let mut cur = std::env::current_dir().unwrap();
    loop {
        if cur.join("NupExtractor.slnx").exists() || cur.join("sample_game.nup").exists() {
            return cur;
        }
        if !cur.pop() {
            panic!("Could not find repo root");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut cases: usize = 10_000;
    let mut rust_only = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--cases" => {
                i += 1;
                cases = args[i].parse().expect("valid --cases count");
            }
            "--rust-only" => rust_only = true,
            _ => {}
        }
        i += 1;
    }

    let root = find_repo_root();
    let bases: Vec<Vec<u8>> = [
        root.join("sample_game.nup"),
        root.join("tests_golden/fixtures/base_only.nup"),
        root.join("tests_golden/fixtures/multi_pkg_dlc.nup"),
        root.join("tests_golden/fixtures/edge_cases.nup"),
    ]
    .iter()
    .map(|p| fs::read(p).unwrap_or_else(|e| panic!("Failed to read {}: {e}", p.display())))
    .collect();

    let tmp = tempfile::tempdir().unwrap();
    let mut rng = XorShift64(0xDEAD_BEEF_CAFE_BABEu64);

    let mut worker = if !rust_only {
        let recorder_proj = root.join("tools/golden-recorder/NupGoldenRecorder.csproj");
        let build = Command::new("dotnet")
            .args([
                "build",
                recorder_proj.to_str().unwrap(),
                "-c",
                "Release",
                "-v",
                "quiet",
            ])
            .status()
            .expect("spawn dotnet build");
        assert!(build.success(), "Failed to build NupGoldenRecorder");

        let exe =
            root.join("tools/golden-recorder/bin/Release/net10.0-windows/NupGoldenRecorder.exe");
        Some(CSharpWorker::spawn(exe))
    } else {
        None
    };

    let mut open_ok_count = 0usize;
    let mut open_err_count = 0usize;
    let mut verify_valid_count = 0usize;
    let mut diffs = 0usize;
    let mut rust_stricter_rejects = 0usize;
    let mut csharp_crashes_rust_safe = 0usize;

    for idx in 0..cases {
        let file_path = tmp.path().join(format!("case_{idx}.nup"));
        if idx >= 128 {
            let _ = fs::remove_file(tmp.path().join(format!("case_{}.nup", idx - 128)));
        }
        let data = mutate_case(idx, &bases, &mut rng);
        fs::write(&file_path, &data).unwrap();

        let rust_out = eval_rust(&file_path);
        if rust_out.open_ok {
            open_ok_count += 1;
        } else {
            open_err_count += 1;
        }
        if rust_out.verify_valid {
            verify_valid_count += 1;
        }

        if let Some(w) = worker.as_mut() {
            match w.eval(&file_path) {
                None => {
                    // C# reference crashed (e.g. 0xC0000005 / OOM on hostile header integer overflow).
                    // Verify Rust safely rejected the hostile input!
                    if !rust_out.open_ok {
                        csharp_crashes_rust_safe += 1;
                    } else {
                        diffs += 1;
                        eprintln!("[DIFF] case={idx} C# crashed while Rust opened: {rust_out:?}");
                    }
                }
                Some(cs_out) => {
                    let same = rust_out.open_ok == cs_out.open_ok
                        && rust_out.package_count == cs_out.package_count
                        && rust_out.dir_count == cs_out.dir_count
                        && rust_out.file_count == cs_out.file_count
                        && rust_out.verify_valid == cs_out.verify_valid;

                    if !same {
                        // Approved DIV-003 security hardening on mutated TOCs (strategies 2 & 6):
                        // - Rust rejects corrupt TOC tables / out-of-bounds string pool slices at open time
                        // - Rust enforces decompression-bomb / declared-size check during deep verify
                        let is_corrupt_toc_strategy = idx % 10 == 2 || idx % 10 == 6;
                        let approved_rust_stricter =
                            (!rust_out.open_ok && cs_out.open_ok && !cs_out.verify_valid)
                                || (!rust_out.open_ok && cs_out.open_ok && is_corrupt_toc_strategy)
                                || (rust_out.open_ok
                                    && cs_out.open_ok
                                    && !rust_out.verify_valid
                                    && cs_out.verify_valid
                                    && is_corrupt_toc_strategy);
                        if approved_rust_stricter {
                            rust_stricter_rejects += 1;
                        } else {
                            diffs += 1;
                            if diffs <= 20 {
                                eprintln!(
                                    "[DIFF] case={idx} strategy={} rust={rust_out:?} csharp={cs_out:?}",
                                    idx % 10
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    println!(
        "[Differential] Ran {cases} cases: open_ok={open_ok_count}, open_err={open_err_count}, verify_valid={verify_valid_count}, approved_rust_stricter={rust_stricter_rejects}, csharp_crashes_rust_safe={csharp_crashes_rust_safe}, unapproved_diffs={diffs}"
    );
    assert_eq!(
        diffs, 0,
        "Found {diffs} unapproved behavioral differences between C# and Rust!"
    );
}
