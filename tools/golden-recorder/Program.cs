using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Threading;
using NupExtractorApp;
using NupExtractorApp.Orb;

namespace NupGoldenRecorder;

public static class Program
{
    private static readonly JsonSerializerOptions PrettyJson = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        WriteIndented = true
    };

    public static int Main(string[] args)
    {
        // Deterministic formatting independent of Windows display locale
        CultureInfo.DefaultThreadCurrentCulture = CultureInfo.InvariantCulture;
        CultureInfo.DefaultThreadCurrentUICulture = CultureInfo.InvariantCulture;
        Thread.CurrentThread.CurrentCulture = CultureInfo.InvariantCulture;
        Thread.CurrentThread.CurrentUICulture = CultureInfo.InvariantCulture;

        if (args.Length > 0 && args[0] == "--diff-worker")
        {
            return RunDiffWorker();
        }

        string repoRoot = FindRepoRoot();
        string goldenRoot = Path.Combine(repoRoot, "tests_golden");
        string fixturesDir = Path.Combine(goldenRoot, "fixtures");
        string corruptDir = Path.Combine(fixturesDir, "corrupt");
        string engineOutDir = Path.Combine(goldenRoot, "engine");
        string hostOutDir = Path.Combine(goldenRoot, "host");
        string ipcOutDir = Path.Combine(goldenRoot, "ipc");

        Directory.CreateDirectory(fixturesDir);
        Directory.CreateDirectory(corruptDir);
        Directory.CreateDirectory(engineOutDir);
        Directory.CreateDirectory(hostOutDir);
        Directory.CreateDirectory(ipcOutDir);

        Console.WriteLine($"[GoldenRecorder] Repo root: {repoRoot}");

        // 1. Generate deterministic synthetic fixtures
        var validFixtures = new Dictionary<string, string>
        {
            ["sample_game"] = Path.Combine(repoRoot, "sample_game.nup"),
            ["base_only"] = Path.Combine(fixturesDir, "base_only.nup"),
            ["multi_pkg_dlc"] = Path.Combine(fixturesDir, "multi_pkg_dlc.nup"),
            ["edge_cases"] = Path.Combine(fixturesDir, "edge_cases.nup")
        };

        GenerateBaseOnlyFixture(validFixtures["base_only"]);
        GenerateMultiPkgDlcFixture(validFixtures["multi_pkg_dlc"]);
        GenerateEdgeCasesFixture(validFixtures["edge_cases"]);

        var corruptFixtures = GenerateCorruptFixtures(validFixtures["base_only"], corruptDir);

        // 2. Record engine goldens for every valid fixture
        foreach (var (name, nupPath) in validFixtures)
        {
            Console.WriteLine($"[GoldenRecorder] Recording engine goldens for '{name}'...");
            RecordInspect(name, nupPath, repoRoot, Path.Combine(engineOutDir, $"{name}.inspect.json"));
            RecordTree(name, nupPath, Path.Combine(engineOutDir, $"{name}.tree.json"));
            RecordSearch(name, nupPath, Path.Combine(engineOutDir, $"{name}.search.json"));
            RecordExtract(name, nupPath, Path.Combine(engineOutDir, $"{name}.extract.json"));
            RecordVerify(name, nupPath, Path.Combine(engineOutDir, $"{name}.verify.json"));
        }

        // 3. Record error goldens
        Console.WriteLine("[GoldenRecorder] Recording error goldens...");
        RecordErrors(validFixtures["sample_game"], corruptFixtures, Path.Combine(engineOutDir, "errors.json"));

        // 4. Record Orb host-logic goldens
        Console.WriteLine("[GoldenRecorder] Recording orb host-logic goldens...");
        RecordOrbHostGoldens(Path.Combine(hostOutDir, "orb_logic.json"));

        // 5. Record IPC contract goldens
        Console.WriteLine("[GoldenRecorder] Recording IPC contract goldens...");
        RecordIpcContractGoldens(validFixtures["sample_game"], repoRoot, Path.Combine(ipcOutDir, "contract.json"));

        Console.WriteLine("[GoldenRecorder] Complete!");
        return 0;
    }

    private static string FindRepoRoot()
    {
        string dir = Directory.GetCurrentDirectory();
        while (!string.IsNullOrEmpty(dir))
        {
            if (File.Exists(Path.Combine(dir, "Cargo.toml")) && File.Exists(Path.Combine(dir, "sample_game.nup")))
                return dir;
            dir = Path.GetDirectoryName(dir) ?? "";
        }
        throw new InvalidOperationException("Could not locate repository root containing NupExtractor.slnx");
    }

    private static void WriteJson(string path, object obj)
    {
        string json = JsonSerializer.Serialize(obj, PrettyJson).Replace("\r\n", "\n") + "\n";
        File.WriteAllText(path, json, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
    }

    private static string NormalizePathToken(string raw, string repoRoot)
    {
        string normRoot = Path.GetFullPath(repoRoot).Replace('\\', '/').TrimEnd('/');
        string normRaw = raw.Replace('\\', '/');
        if (normRaw.StartsWith(normRoot, StringComparison.OrdinalIgnoreCase))
            return "<REPO>" + normRaw[normRoot.Length..];
        return normRaw;
    }

    // =========================================================================
    // Synthetic Fixture Generators
    // =========================================================================

    private static void GenerateBaseOnlyFixture(string outNup)
    {
        string tmp = Path.Combine(Path.GetTempPath(), "nup_gen_base_only");
        if (Directory.Exists(tmp)) Directory.Delete(tmp, true);
        try
        {
            Directory.CreateDirectory(Path.Combine(tmp, "exefs"));
            Directory.CreateDirectory(Path.Combine(tmp, "romfs", "levels"));

            // Deterministic NSO0 header with BuildId at 0x40..0x60
            byte[] mainBin = new byte[512];
            Encoding.ASCII.GetBytes("NSO0").CopyTo(mainBin, 0);
            for (int i = 0; i < 32; i++) mainBin[0x40 + i] = (byte)(0xA0 + i);
            for (int i = 0x60; i < mainBin.Length; i++) mainBin[i] = (byte)((i * 7) & 0xFF);
            File.WriteAllBytes(Path.Combine(tmp, "exefs", "main"), mainBin);

            // Deterministic NPDM with ACI0 program ID
            byte[] npdm = new byte[0x40];
            Encoding.ASCII.GetBytes("META").CopyTo(npdm, 0);
            Encoding.ASCII.GetBytes("ACI0").CopyTo(npdm, 0x10);
            BinaryPrimitives.WriteUInt64LittleEndian(npdm.AsSpan(0x20, 8), 0x0100BEEF00001000UL);
            File.WriteAllBytes(Path.Combine(tmp, "exefs", "main.npdm"), npdm);

            // Highly compressible RomFS file (will choose Zstd)
            byte[] compressible = Encoding.UTF8.GetBytes(string.Concat(Enumerable.Repeat("NUP_COMPRESSIBLE_BLOCK_0123456789\n", 120)));
            File.WriteAllBytes(Path.Combine(tmp, "romfs", "levels", "stage01.txt"), compressible);

            // Incompressible tiny file (will fall back to RAW)
            byte[] tinyRaw = new byte[] { 0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04 };
            File.WriteAllBytes(Path.Combine(tmp, "romfs", "raw_blob.bin"), tinyRaw);

            // Minimal valid PNG icon (1x1)
            string iconPath = Path.Combine(tmp, "_icon.png");
            byte[] png1x1 = Convert.FromBase64String("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==");
            File.WriteAllBytes(iconPath, png1x1);

            string srcOnly = Path.Combine(tmp, "src");
            Directory.CreateDirectory(Path.Combine(srcOnly, "exefs"));
            Directory.CreateDirectory(Path.Combine(srcOnly, "romfs", "levels"));
            File.Copy(Path.Combine(tmp, "exefs", "main"), Path.Combine(srcOnly, "exefs", "main"));
            File.Copy(Path.Combine(tmp, "exefs", "main.npdm"), Path.Combine(srcOnly, "exefs", "main.npdm"));
            File.Copy(Path.Combine(tmp, "romfs", "levels", "stage01.txt"), Path.Combine(srcOnly, "romfs", "levels", "stage01.txt"));
            File.Copy(Path.Combine(tmp, "romfs", "raw_blob.bin"), Path.Combine(srcOnly, "romfs", "raw_blob.bin"));

            NupWriter.Pack(new PackOptions
            {
                SourceDirectory = srcOnly,
                OutputFilePath = outNup,
                TitleName = "Synthetic Base Game",
                Publisher = "CleanRoom QA",
                TitleId = 0x0100BEEF00001000UL,
                VersionString = "1.0.0",
                CompressionLevel = 3,
                IconPath = iconPath
            });
        }
        finally
        {
            if (Directory.Exists(tmp)) Directory.Delete(tmp, true);
        }
    }

    private static void GenerateMultiPkgDlcFixture(string outNup)
    {
        string tmp = Path.Combine(Path.GetTempPath(), "nup_gen_multi_pkg");
        if (Directory.Exists(tmp)) Directory.Delete(tmp, true);
        try
        {
            Directory.CreateDirectory(Path.Combine(tmp, "base", "exefs"));
            Directory.CreateDirectory(Path.Combine(tmp, "base", "romfs", "data"));
            Directory.CreateDirectory(Path.Combine(tmp, "updates", "65536", "romfs", "data"));
            Directory.CreateDirectory(Path.Combine(tmp, "dlc", "0100CAFE00002001", "0", "romfs"));

            File.WriteAllText(Path.Combine(tmp, "base", "exefs", "rtld"), "RTLD_STUB_CONTENT");
            File.WriteAllText(Path.Combine(tmp, "base", "romfs", "data", "world.json"), "{\"world\":1,\"items\":[1,2,3,4,5,6,7,8,9,10]}");
            File.WriteAllText(Path.Combine(tmp, "updates", "65536", "romfs", "data", "patch.txt"), string.Concat(Enumerable.Repeat("PATCH_V1_1_DATA_", 50)));
            File.WriteAllText(Path.Combine(tmp, "dlc", "0100CAFE00002001", "0", "romfs", "costume.bin"), "DLC_COSTUME_PAYLOAD_001");

            NupWriter.Pack(new PackOptions
            {
                SourceDirectory = tmp,
                OutputFilePath = outNup,
                TitleName = "Multi Package Adventure",
                Publisher = "CleanRoom Studios",
                TitleId = 0x0100CAFE00002000UL,
                VersionString = "1.2.3",
                CompressionLevel = 3
            });
        }
        finally
        {
            if (Directory.Exists(tmp)) Directory.Delete(tmp, true);
        }
    }

    private static void GenerateEdgeCasesFixture(string outNup)
    {
        string tmp = Path.Combine(Path.GetTempPath(), "nup_gen_edge_cases");
        if (Directory.Exists(tmp)) Directory.Delete(tmp, true);
        try
        {
            // Empty 0-byte file, Unicode names, long filename, deep 10-level nested tree
            string romfs = Path.Combine(tmp, "romfs");
            Directory.CreateDirectory(romfs);
            File.WriteAllBytes(Path.Combine(romfs, "empty_zero_bytes.dat"), Array.Empty<byte>());
            File.WriteAllText(Path.Combine(romfs, "日本語_café.txt"), "Unicode content: ゼルダ / café / αβγ", Encoding.UTF8);

            string longName = new string('a', 110) + "_long_name_test.bin";
            File.WriteAllText(Path.Combine(romfs, longName), string.Concat(Enumerable.Repeat("LONG_NAME_PAYLOAD_", 20)));

            string deep = romfs;
            for (int d = 1; d <= 10; d++)
            {
                deep = Path.Combine(deep, $"lvl{d:D2}");
                Directory.CreateDirectory(deep);
            }
            File.WriteAllText(Path.Combine(deep, "deep_leaf.txt"), "Reached depth 10!");

            // Also include an empty directory
            Directory.CreateDirectory(Path.Combine(romfs, "empty_folder"));

            NupWriter.Pack(new PackOptions
            {
                SourceDirectory = tmp,
                OutputFilePath = outNup,
                TitleName = "Edge Cases Container",
                Publisher = "QA Lab",
                TitleId = 0x040074A01BF12000UL,
                VersionString = "2.0.0",
                CompressionLevel = 3
            });
        }
        finally
        {
            if (Directory.Exists(tmp)) Directory.Delete(tmp, true);
        }
    }

    private static Dictionary<string, string> GenerateCorruptFixtures(string validBaseNup, string corruptDir)
    {
        byte[] valid = File.ReadAllBytes(validBaseNup);
        var map = new Dictionary<string, string>();

        void WriteCorrupt(string name, byte[] data)
        {
            string p = Path.Combine(corruptDir, name + ".nup");
            File.WriteAllBytes(p, data);
            map[name] = p;
        }

        // 1. Bad magic
        byte[] badMagic = (byte[])valid.Clone();
        badMagic[0] = (byte)'B'; badMagic[1] = (byte)'A'; badMagic[2] = (byte)'D'; badMagic[3] = (byte)'!';
        WriteCorrupt("bad_magic", badMagic);

        // 2. Truncated header (64 bytes < 128)
        WriteCorrupt("truncated_header", valid[..64]);

        // 3. Size mismatch (truncated by 10 bytes while header keeps old FileSize)
        WriteCorrupt("size_mismatch", valid[..^10]);

        // 4. TOC offset before header end (with matching file_size)
        byte[] badTocOff = (byte[])valid.Clone();
        BinaryPrimitives.WriteUInt64LittleEndian(badTocOff.AsSpan(8, 8), 64);
        WriteCorrupt("toc_before_header", badTocOff);

        // 5. Blob offset overlaps TOC
        byte[] tocOverlap = (byte[])valid.Clone();
        BinaryPrimitives.WriteUInt64LittleEndian(tocOverlap.AsSpan(56, 8), 128);
        WriteCorrupt("blob_overlaps_toc", tocOverlap);

        // 6. Unaligned blob offset
        byte[] unaligned = (byte[])valid.Clone();
        ulong origBlob = BinaryPrimitives.ReadUInt64LittleEndian(unaligned.AsSpan(56, 8));
        BinaryPrimitives.WriteUInt64LittleEndian(unaligned.AsSpan(56, 8), origBlob + 3);
        WriteCorrupt("unaligned_blob", unaligned);

        // 7. Corrupted TOC SHA-256 hash
        byte[] badTocHash = (byte[])valid.Clone();
        badTocHash[24] ^= 0xFF;
        WriteCorrupt("bad_toc_hash", badTocHash);

        // 8. Corrupted TOC magic (with recomputed TOC SHA-256 so it reaches TOC magic check!)
        byte[] badTocMagic = (byte[])valid.Clone();
        ulong tocOff = BinaryPrimitives.ReadUInt64LittleEndian(badTocMagic.AsSpan(8, 8));
        ulong tocSz = BinaryPrimitives.ReadUInt64LittleEndian(badTocMagic.AsSpan(16, 8));
        badTocMagic[(int)tocOff] = (byte)'X';
        SHA256.HashData(badTocMagic.AsSpan((int)tocOff, (int)tocSz)).CopyTo(badTocMagic.AsSpan(24, 32));
        WriteCorrupt("bad_toc_magic", badTocMagic);

        // 9. Corrupted extent byte (passes Superblock + TOC SHA-256, fails deep verify!)
        byte[] badExtent = (byte[])valid.Clone();
        badExtent[^1] ^= 0xAA;
        WriteCorrupt("bad_extent_hash", badExtent);

        // 10. Unknown codec (codec = 99 on file 0, with recomputed TOC SHA-256)
        byte[] badCodec = (byte[])valid.Clone();
        int file0CodecOffset = (int)tocOff + 48 + NupConstants.PkgSize * 1 + NupConstants.DirSize * 4 + 64;
        if (file0CodecOffset < (int)(tocOff + tocSz))
        {
            badCodec[file0CodecOffset] = 99;
            SHA256.HashData(badCodec.AsSpan((int)tocOff, (int)tocSz)).CopyTo(badCodec.AsSpan(24, 32));
            WriteCorrupt("unknown_codec", badCodec);
        }

        return map;
    }

    // =========================================================================
    // Engine Golden Recorders
    // =========================================================================

    private static void RecordInspect(string name, string nupPath, string repoRoot, string outPath)
    {
        var data = NupEngine.Inspect(nupPath);
        // Normalize machine-specific path strings to <REPO>/...
        data["file_path"] = NormalizePathToken(data["file_path"].ToString()!, repoRoot);
        data["filePath"] = NormalizePathToken(data["filePath"].ToString()!, repoRoot);
        data["default_dest_dir"] = NormalizePathToken(data["default_dest_dir"].ToString()!, repoRoot);
        data["defaultDestDir"] = NormalizePathToken(data["defaultDestDir"].ToString()!, repoRoot);
        WriteJson(outPath, data);
    }

    private static void RecordTree(string name, string nupPath, string outPath)
    {
        using var reader = new NupReader(nupPath);
        var dirs = new List<object>();
        for (uint i = 0; i < (uint)reader.Dirs.Count; i++)
        {
            dirs.Add(NupEngine.GetDirChildren(nupPath, i));
        }
        WriteJson(outPath, new { fixture = name, dirCount = reader.Dirs.Count, dirs });
    }

    private static void RecordSearch(string name, string nupPath, string outPath)
    {
        string[] queries = { "", "a", "LEVEL", "level", "日", "café", ".npdm", "nonexistent_xyz", "0" };
        var entries = new List<object>();
        foreach (var q in queries)
        {
            entries.Add(new { query = q, results = NupEngine.SearchFiles(nupPath, q) });
        }
        WriteJson(outPath, new { fixture = name, queries = entries });
    }

    private static void RecordExtract(string name, string nupPath, string outPath)
    {
        using var reader = new NupReader(nupPath);
        var modes = new List<object>();

        void RunMode(string label, string mode, int? pkgIdx, string? subtree)
        {
            string tmp = Path.Combine(Path.GetTempPath(), $"nup_golden_ext_{name}_{label}_{Guid.NewGuid():N}");
            try
            {
                var progressEvents = new List<object>();
                var syncProg = new SyncProgress<ProgressInfo>(p =>
                {
                    progressEvents.Add(new
                    {
                        status = p.Status,
                        current = p.Current,
                        total = p.Total,
                        percent = p.Percent,
                        fileName = p.FileName,
                        bytesWritten = p.BytesWritten,
                        totalBytes = p.TotalBytes
                    });
                });

                NupEngine.Extract(nupPath, tmp, mode, pkgIdx, subtree, syncProg, CancellationToken.None);

                var files = Directory.GetFiles(tmp, "*", SearchOption.AllDirectories)
                    .Select(f =>
                    {
                        string rel = Path.GetRelativePath(tmp, f).Replace('\\', '/');
                        byte[] bytes = File.ReadAllBytes(f);
                        return new
                        {
                            relPath = rel,
                            size = (ulong)bytes.Length,
                            sha256 = Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant()
                        };
                    })
                    .OrderBy(x => x.relPath, StringComparer.Ordinal)
                    .ToList();

                // Normalize progress: first ("starting") and last ("completed") always deterministic
                modes.Add(new
                {
                    label,
                    mode,
                    packageIndex = pkgIdx,
                    subtreePath = subtree,
                    fileCount = files.Count,
                    firstStatus = progressEvents.FirstOrDefault(),
                    lastStatus = progressEvents.LastOrDefault(),
                    files
                });
            }
            finally
            {
                if (Directory.Exists(tmp)) Directory.Delete(tmp, true);
            }
        }

        RunMode("all", "all", null, null);
        for (int p = 0; p < reader.Packages.Count; p++)
        {
            RunMode($"package_{p}", "package", p, null);
            if (reader.Packages[p].RomFsDir != NupConstants.NoDir)
                RunMode($"subtree_pkg{p}_romfs", "subtree", p, "romfs");
            if (reader.Packages[p].ExeFsDir != NupConstants.NoDir)
                RunMode($"subtree_pkg{p}_exefs", "subtree", p, "exefs");
        }

        WriteJson(outPath, new { fixture = name, modes });
    }

    private static void RecordVerify(string name, string nupPath, string outPath)
    {
        object RunVerify(bool deep)
        {
            VerifyProgressInfo? final = null;
            int progressCount = 0;
            var syncProg = new SyncProgress<VerifyProgressInfo>(p =>
            {
                progressCount++;
                if (p.Status == "done") final = p;
            });
            NupEngine.Verify(nupPath, deep, syncProg, CancellationToken.None);
            return new
            {
                deep,
                valid = final?.Valid ?? false,
                totalFilesChecked = final?.TotalFilesChecked ?? 0,
                problems = final?.Problems ?? new List<string>()
            };
        }

        WriteJson(outPath, new
        {
            fixture = name,
            fast = RunVerify(false),
            deep = RunVerify(true)
        });
    }

    private static void RecordErrors(string sampleNup, Dictionary<string, string> corruptFixtures, string outPath)
    {
        var cases = new List<object>();

        void Capture(string id, string operation, Action act)
        {
            try
            {
                act();
                cases.Add(new { id, operation, threw = false, exceptionType = (string?)null, message = (string?)null });
            }
            catch (Exception ex)
            {
                cases.Add(new { id, operation, threw = true, exceptionType = ex.GetType().Name, message = ex.Message });
            }
        }

        foreach (var (cname, cpath) in corruptFixtures.OrderBy(kv => kv.Key, StringComparer.Ordinal))
        {
            if (cname == "bad_extent_hash")
            {
                // Passes open/inspect, fails deep verify
                VerifyProgressInfo? final = null;
                NupEngine.Verify(cpath, deep: true, new SyncProgress<VerifyProgressInfo>(p => { if (p.Status == "done") final = p; }), CancellationToken.None);
                cases.Add(new
                {
                    id = $"corrupt_{cname}_deep_verify",
                    operation = "verify_deep",
                    threw = false,
                    valid = final?.Valid ?? false,
                    problems = final?.Problems ?? new List<string>()
                });
            }
            else if (cname == "unknown_codec")
            {
                VerifyProgressInfo? final = null;
                NupEngine.Verify(cpath, deep: true, new SyncProgress<VerifyProgressInfo>(p => { if (p.Status == "done") final = p; }), CancellationToken.None);
                cases.Add(new
                {
                    id = $"corrupt_{cname}_deep_verify",
                    operation = "verify_deep",
                    threw = false,
                    valid = final?.Valid ?? false,
                    problems = final?.Problems ?? new List<string>()
                });
            }
            else
            {
                Capture($"corrupt_{cname}_inspect", "inspect", () => NupEngine.Inspect(cpath));
            }
        }

        Capture("oob_dir_index", "get_tree", () => NupEngine.GetDirChildren(sampleNup, 999999));
        Capture("empty_nup_path", "extract", () => NupEngine.Extract("", "out", "all", null, null, new SyncProgress<ProgressInfo>(_ => { }), CancellationToken.None));
        Capture("empty_dest_path", "extract", () => NupEngine.Extract(sampleNup, "", "all", null, null, new SyncProgress<ProgressInfo>(_ => { }), CancellationToken.None));
        Capture("missing_file_inspect", "inspect", () => NupEngine.Inspect(Path.Combine(Path.GetTempPath(), "does_not_exist_nup_golden.nup")));
        Capture("invalid_package_index", "extract", () => NupEngine.Extract(sampleNup, Path.Combine(Path.GetTempPath(), "nup_err_out"), "package", 99, null, new SyncProgress<ProgressInfo>(_ => { }), CancellationToken.None));
        Capture("missing_subtree", "extract", () => NupEngine.Extract(sampleNup, Path.Combine(Path.GetTempPath(), "nup_err_out"), "subtree", 0, "romfs/does_not_exist", new SyncProgress<ProgressInfo>(_ => { }), CancellationToken.None));
        Capture("single_file_oob", "extract_single_file", () => NupEngine.ExtractSingleFile(sampleNup, 9999, Path.Combine(Path.GetTempPath(), "nup_err_single.bin")));

        WriteJson(outPath, new { cases });
    }

    // =========================================================================
    // Orb Host-Logic Goldens
    // =========================================================================

    private static void RecordOrbHostGoldens(string outPath)
    {
        // 1. State machine expand & collapse trajectories
        var sm = new OrbStateMachine(OrbWindowState.Collapsed, reducedMotion: false);
        sm.RequestExpand(0);
        var expandTrace = new List<object>();
        for (double t = 16; t <= 480; t += 32)
        {
            var s = sm.Tick(t);
            expandTrace.Add(new { tMs = t, state = s.State.ToString(), progress = Math.Round(s.Progress, 6), isAnimating = s.IsAnimating });
        }

        // Interrupt mid-expand at 128ms
        var smInt = new OrbStateMachine(OrbWindowState.Collapsed, reducedMotion: false);
        smInt.RequestExpand(0);
        for (double t = 16; t <= 128; t += 16) smInt.Tick(t);
        double midProg = smInt.Progress;
        smInt.RequestCollapse(128);
        var intTrace = new List<object> { new { tMs = 128.0, state = smInt.State.ToString(), progress = Math.Round(midProg, 6), isAnimating = true } };
        for (double t = 160; t <= 608; t += 32)
        {
            var s = smInt.Tick(t);
            intTrace.Add(new { tMs = t, state = s.State.ToString(), progress = Math.Round(s.Progress, 6), isAnimating = s.IsAnimating });
        }

        // Reduced motion expand
        var smRed = new OrbStateMachine(OrbWindowState.Collapsed, reducedMotion: true);
        smRed.RequestExpand(0);
        var redTrace = new List<object>();
        for (double t = 35; t <= 175; t += 35)
        {
            var s = smRed.Tick(t);
            redTrace.Add(new { tMs = t, state = s.State.ToString(), progress = Math.Round(s.Progress, 6), isAnimating = s.IsAnimating });
        }

        // 2. DPI & Snapping table across 4 taskbar edges & mixed-DPI multi-monitor
        var dpiTable = new[] { 1.0, 1.25, 1.5, 2.0 }.Select(s => new
        {
            dpiScale = s,
            orbPx = OrbGeometry.ComputeOrbPhysicalSize(64.0, s),
            marginPx = OrbGeometry.DipToPhysical(14.0, s),
            expandedRadiusPx = OrbGeometry.DipToPhysical(28.0, s)
        }).ToList();

        var snapCases = new (string label, PixelRect work, PixelRect orb, int margin)[]
        {
            ("bottom_taskbar_left", new(0, 0, 1920, 1032), new(200, 500, 64, 64), 14),
            ("bottom_taskbar_right", new(0, 0, 1920, 1032), new(1700, 500, 64, 64), 14),
            ("bottom_taskbar_top", new(0, 0, 1920, 1032), new(960, 80, 64, 64), 14),
            ("bottom_taskbar_bottom", new(0, 0, 1920, 1032), new(960, 960, 64, 64), 14),
            ("left_taskbar", new(72, 0, 1848, 1080), new(10, 400, 64, 64), 14),
            ("top_taskbar", new(0, 48, 1920, 1032), new(900, 5, 64, 64), 14),
            ("right_taskbar", new(0, 0, 1848, 1080), new(1890, 400, 64, 64), 14),
            ("neg_monitor_150dpi", new(1920, -116, 2560, 1380), new(4400, 100, 96, 96), 21)
        }.Select(c =>
        {
            var (snapped, edge) = OrbGeometry.SnapOrbToNearestEdge(c.orb, c.work, c.margin);
            return new { c.label, c.work, c.orb, c.margin, snapped, edge = edge.ToString() };
        }).ToList();

        var monitors = new List<MonitorWorkArea>
        {
            new(@"\\.\DISPLAY1", new(0, 0, 1920, 1080), new(0, 0, 1920, 1032), 1.0, true),
            new(@"\\.\DISPLAY2", new(1920, -116, 2560, 1440), new(1920, -116, 2560, 1380), 1.5, false),
            new(@"\\.\DISPLAY3", new(-1366, 0, 1366, 768), new(-1366, 0, 1366, 728), 1.25, false)
        };

        var expandNearCases = new[]
        {
            new { label = "mon1_right", orb = new PixelRect(1842, 360, 64, 64), monIdx = 0 },
            new { label = "mon1_left", orb = new PixelRect(14, 480, 64, 64), monIdx = 0 },
            new { label = "mon2_150dpi_negY", orb = new PixelRect(4363, 100, 96, 96), monIdx = 1 },
            new { label = "mon3_small_negX", orb = new PixelRect(-1350, 300, 80, 80), monIdx = 2 }
        }.Select(c => new
        {
            c.label,
            c.orb,
            monitor = monitors[c.monIdx],
            expanded = OrbGeometry.ComputeExpandedRectNearOrb(c.orb, new DipSize(1240, 820), monitors[c.monIdx])
        }).ToList();

        // 3. Settings serialization round-trip
        var sampleSettings = new OrbSettings
        {
            HasSavedPosition = true,
            OrbCenterX = 1874,
            OrbCenterY = 392,
            AnchorFractionX = 0.9760416666666667,
            AnchorFractionY = 0.3798449612403101,
            OrbEdge = ScreenEdge.Right,
            MonitorDeviceName = @"\\.\DISPLAY1",
            LastMode = "Collapsed",
            LaunchMode = OrbLaunchMode.Collapsed,
            ExpandedWidthDip = 1240,
            ExpandedHeightDip = 820,
            Theme = "dark"
        };

        WriteJson(outPath, new
        {
            expandTrace,
            interruptTrace = intTrace,
            reducedMotionTrace = redTrace,
            dpiTable,
            snapCases,
            expandNearCases,
            sampleSettings
        });
    }

    // =========================================================================
    // IPC Contract Goldens
    // =========================================================================

    private static void RecordIpcContractGoldens(string sampleNup, string repoRoot, string outPath)
    {
        var inspectData = NupEngine.Inspect(sampleNup);
        inspectData["file_path"] = "<REPO>/sample_game.nup";
        inspectData["filePath"] = "<REPO>/sample_game.nup";
        inspectData["default_dest_dir"] = "<REPO>/Sample NX Adventure_extracted";
        inspectData["defaultDestDir"] = "<REPO>/Sample NX Adventure_extracted";

        var treeData = NupEngine.GetDirChildren(sampleNup, 0);
        var searchResults = NupEngine.SearchFiles(sampleNup, "level");

        VerifyProgressInfo? verifyFinal = null;
        NupEngine.Verify(sampleNup, true, new SyncProgress<VerifyProgressInfo>(p => { if (p.Status == "done") verifyFinal = p; }), CancellationToken.None);

        var exchanges = new object[]
        {
            new
            {
                name = "get_app_info",
                request = (object)new { id = "req_1", action = "get_app_info" },
                response = (object)new { id = "req_1", action = "app_info_result", success = true, version = AppUpdateManager.CurrentVersion, initialFilePath = (string?)null }
            },
            new
            {
                name = "inspect",
                request = (object)new { id = "req_2", action = "inspect", filePath = "<REPO>/sample_game.nup" },
                response = (object)new { id = "req_2", action = "inspect_result", success = true, data = inspectData }
            },
            new
            {
                name = "get_tree",
                request = (object)new { id = "req_3", action = "get_tree", filePath = "<REPO>/sample_game.nup", dirIndex = 0 },
                response = (object)new { id = "req_3", action = "get_tree_result", success = true, data = treeData }
            },
            new
            {
                name = "search",
                request = (object)new { id = "req_4", action = "search", filePath = "<REPO>/sample_game.nup", query = "level" },
                response = (object)new { id = "req_4", action = "search_result", success = true, query = "level", results = searchResults }
            },
            new
            {
                name = "verify_done",
                request = (object)new { action = "verify", filePath = "<REPO>/sample_game.nup", deep = true },
                response = (object)new { type = "verify_done", info = verifyFinal }
            },
            new
            {
                name = "get_orb_settings",
                request = (object)new { id = "req_5", action = "get_orb_settings" },
                response = (object)new
                {
                    id = "req_5",
                    action = "orb_settings_result",
                    success = true,
                    settings = new { launchMode = "Collapsed", theme = "dark", orbEdge = "Right", state = "collapsed" },
                    appearance = new { reducedMotion = false, highContrast = false, transparencyEnabled = true }
                }
            },
            new
            {
                name = "morph_frame_event",
                request = (object)new { action = "orb_expand" },
                response = (object)new
                {
                    type = "morph_frame",
                    state = "expanded",
                    progress = 1.0,
                    radiusPx = 28.0,
                    shellX = 0.0,
                    shellY = 0.0,
                    orbRelX = 0.0,
                    orbRelY = 0.0,
                    width = 1240.0,
                    height = 820.0,
                    expandedWidth = 1240.0,
                    expandedHeight = 820.0,
                    reducedMotion = false,
                    edge = "right"
                }
            }
        };

        WriteJson(outPath, new { contractVersion = 1, exchanges });
    }

    private static int RunDiffWorker()
    {
        var compactJson = new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase };
        string? line;
        while ((line = Console.ReadLine()) != null)
        {
            string path = line.Trim();
            if (string.IsNullOrEmpty(path)) continue;
            try
            {
                int pkgCount, dirCount, fileCount;
                using (var reader = new NupReader(path))
                {
                    pkgCount = reader.Packages.Count;
                    dirCount = reader.Dirs.Count;
                    fileCount = reader.Files.Count;
                }

                VerifyProgressInfo? finalV = null;
                try
                {
                    NupEngine.Verify(
                        path,
                        deep: true,
                        new SyncProgress<VerifyProgressInfo>(p => { if (p.Status == "done") finalV = p; }),
                        CancellationToken.None);
                }
                catch
                {
                    finalV = null;
                }

                Console.WriteLine(JsonSerializer.Serialize(new
                {
                    openOk = true,
                    openErrClass = (string?)null,
                    packageCount = pkgCount,
                    dirCount,
                    fileCount,
                    verifyValid = finalV?.Valid ?? false,
                    verifyProblemCount = finalV?.Problems?.Count ?? 1
                }, compactJson));
            }
            catch (Exception ex)
            {
                Console.WriteLine(JsonSerializer.Serialize(new
                {
                    openOk = false,
                    openErrClass = ex.GetType().Name,
                    packageCount = 0,
                    dirCount = 0,
                    fileCount = 0,
                    verifyValid = false,
                    verifyProblemCount = 0
                }, compactJson));
            }
            Console.Out.Flush();
        }
        return 0;
    }

    private sealed class SyncProgress<T> : IProgress<T>
    {
        private readonly Action<T> _cb;
        public SyncProgress(Action<T> cb) => _cb = cb;
        public void Report(T value) => _cb(value);
    }
}
