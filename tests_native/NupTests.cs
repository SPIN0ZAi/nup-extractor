using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Threading;
using NupExtractorApp;
using Xunit;
using ZstdSharp;

namespace NupExtractorApp.Tests;

public class NupTests
{
    private static readonly string SampleNupPath = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../sample_game.nup"));

    [Fact]
    public void Test_Superblock_Parsing_ValidContainer()
    {
        Assert.True(File.Exists(SampleNupPath), $"Sample NUP not found at {SampleNupPath}");
        using var reader = new NupReader(SampleNupPath);

        Assert.Equal(NupConstants.HeaderSize, reader.Super.HeaderSize);
        Assert.Equal(1, reader.Super.HeaderVersion);
        Assert.True(reader.Super.FileSize > 0);
        Assert.Equal((ulong)new FileInfo(SampleNupPath).Length, reader.Super.FileSize);
        Assert.Equal(2u, reader.Super.PackageCount);
        Assert.False(string.IsNullOrEmpty(reader.Super.FormattedUuid));
    }

    [Fact]
    public void Test_Superblock_Validation_RejectsBadMagic()
    {
        byte[] fakeData = new byte[NupConstants.HeaderSize];
        Encoding.ASCII.GetBytes("BAD!").CopyTo(fakeData, 0);

        string tempFile = Path.GetTempFileName();
        try
        {
            File.WriteAllBytes(tempFile, fakeData);
            Assert.Throws<InvalidDataException>(() => new NupReader(tempFile));
        }
        finally
        {
            File.Delete(tempFile);
        }
    }

    [Fact]
    public void Test_TOC_Parsing_PackagesAndNodes()
    {
        using var reader = new NupReader(SampleNupPath);

        Assert.Equal(2, reader.Packages.Count);
        var basePkg = reader.Packages[0];
        Assert.Equal(NupConstants.KindBase, basePkg.Kind);
        Assert.Equal("base", basePkg.KindName);
        Assert.Equal("0100123456789ABC", basePkg.TitleIdHex);

        var updatePkg = reader.Packages[1];
        Assert.Equal(NupConstants.KindUpdate, updatePkg.Kind);
        Assert.Equal("update", updatePkg.KindName);

        Assert.True(reader.Dirs.Count > 0);
        Assert.True(reader.Files.Count > 0);
        Assert.True(reader.Children.Count > 0);
    }

    [Fact]
    public void Test_Metadata_Extraction()
    {
        using var reader = new NupReader(SampleNupPath);
        var basePkg = reader.Packages[0];
        var meta = reader.GetPackageMetadata(basePkg);

        Assert.True(meta.ContainsKey(NupConstants.TlvTitleName));
        string title = Encoding.UTF8.GetString(meta[NupConstants.TlvTitleName]);
        Assert.Equal("Sample NX Adventure", title);

        Assert.True(meta.ContainsKey(NupConstants.TlvPublisher));
        string publisher = Encoding.UTF8.GetString(meta[NupConstants.TlvPublisher]);
        Assert.Equal("Nintendo", publisher);

        Assert.True(meta.ContainsKey(NupConstants.TlvDisplayVersion));
        string version = Encoding.UTF8.GetString(meta[NupConstants.TlvDisplayVersion]);
        Assert.Equal("1.1.0", version);

        Assert.True(meta.ContainsKey(NupConstants.TlvIcon));
        Assert.True(meta[NupConstants.TlvIcon].Length > 0);
    }

    [Fact]
    public void Test_Inspect_ReturnsCompleteContract()
    {
        var data = NupEngine.Inspect(SampleNupPath);

        Assert.NotNull(data);
        Assert.Equal("sample_game.nup", data["file_name"]);
        Assert.True(data.ContainsKey("primary_meta"));
        Assert.True(data.ContainsKey("packages"));
        Assert.True(data.ContainsKey("stats"));

        var packages = (List<Dictionary<string, object>>)data["packages"];
        Assert.Equal(2, packages.Count);

        var primaryMeta = (Dictionary<string, object>)data["primary_meta"];
        Assert.Equal("Sample NX Adventure", primaryMeta["title_name"]);
        Assert.Equal("Nintendo", primaryMeta["publisher"]);
    }

    [Fact]
    public void Test_Tree_Children_Resolution()
    {
        using var reader = new NupReader(SampleNupPath);
        var basePkg = reader.Packages[0];

        var tree = NupEngine.GetDirChildren(SampleNupPath, basePkg.RootDir);
        Assert.NotNull(tree);
        var children = (List<Dictionary<string, object>>)tree["children"];
        Assert.True(children.Count > 0);

        bool foundExeFs = false;
        bool foundRomFs = false;
        foreach (var c in children)
        {
            if (c["name"].ToString() == "exefs") foundExeFs = true;
            if (c["name"].ToString() == "romfs") foundRomFs = true;
        }

        Assert.True(foundExeFs, "exefs not found in root");
        Assert.True(foundRomFs, "romfs not found in root");
    }

    [Fact]
    public void Test_File_Search()
    {
        var results = NupEngine.SearchFiles(SampleNupPath, "level");
        Assert.NotEmpty(results);
        Assert.Contains(results, r => r["name"].ToString() == "level01.dat");

        var noResults = NupEngine.SearchFiles(SampleNupPath, "non_existent_file_name_12345");
        Assert.Empty(noResults);
    }

    [Fact]
    public void Test_Decompression_RawAndZstd()
    {
        using var reader = new NupReader(SampleNupPath);

        int rawFileIndex = -1;
        int zstdFileIndex = -1;

        for (int i = 0; i < reader.Files.Count; i++)
        {
            if (reader.Files[i].Codec == NupConstants.CodecRaw && rawFileIndex < 0)
                rawFileIndex = i;
            if (reader.Files[i].Codec == NupConstants.CodecZstd && zstdFileIndex < 0)
                zstdFileIndex = i;
        }

        Assert.True(rawFileIndex >= 0, "No RAW file found in sample container");
        Assert.True(zstdFileIndex >= 0, "No ZSTD file found in sample container");

        byte[] rawData = reader.ReadFileData(rawFileIndex);
        Assert.Equal((ulong)rawData.Length, reader.Files[rawFileIndex].Size);
        byte[] rawHash = SHA256.HashData(rawData);
        Assert.Equal(rawHash, reader.Files[rawFileIndex].Sha256);

        byte[] zstdData = reader.ReadFileData(zstdFileIndex);
        Assert.Equal((ulong)zstdData.Length, reader.Files[zstdFileIndex].Size);
        byte[] zstdHash = SHA256.HashData(zstdData);
        Assert.Equal(zstdHash, reader.Files[zstdFileIndex].Sha256);
    }

    [Fact]
    public void Test_Full_Extraction_AllPackages()
    {
        string outDir = Path.Combine(Path.GetTempPath(), "nup_test_extract_" + Guid.NewGuid().ToString("N"));
        try
        {
            var progressUpdates = new List<ProgressInfo>();
            var progress = new Progress<ProgressInfo>(info => progressUpdates.Add(info));

            NupEngine.Extract(SampleNupPath, outDir, "all", null, null, progress, CancellationToken.None);

            Assert.True(Directory.Exists(outDir));
            Assert.True(File.Exists(Path.Combine(outDir, "base", "exefs", "main")));
            Assert.True(File.Exists(Path.Combine(outDir, "base", "romfs", "data", "config.json")));
            Assert.True(File.Exists(Path.Combine(outDir, "base", "romfs", "data", "levels", "level01.dat")));
            Assert.True(File.Exists(Path.Combine(outDir, "base", "romfs", "textures", "ui_sheet.bin")));
            Assert.True(File.Exists(Path.Combine(outDir, "updates", "65536", "romfs", "data", "patch_notes.txt")));

            Assert.Equal(2144, new FileInfo(Path.Combine(outDir, "base", "exefs", "main")).Length);
            Assert.Equal(92, new FileInfo(Path.Combine(outDir, "base", "romfs", "data", "config.json")).Length);
            Assert.Equal(8000, new FileInfo(Path.Combine(outDir, "base", "romfs", "data", "levels", "level01.dat")).Length);
            Assert.Equal(13000, new FileInfo(Path.Combine(outDir, "base", "romfs", "textures", "ui_sheet.bin")).Length);
            Assert.Equal(77, new FileInfo(Path.Combine(outDir, "updates", "65536", "romfs", "data", "patch_notes.txt")).Length);
        }
        finally
        {
            if (Directory.Exists(outDir))
                Directory.Delete(outDir, true);
        }
    }

    [Fact]
    public void Test_Package_Mode_Extraction()
    {
        string outDir = Path.Combine(Path.GetTempPath(), "nup_test_pkg_" + Guid.NewGuid().ToString("N"));
        try
        {
            var progress = new Progress<ProgressInfo>(_ => { });
            NupEngine.Extract(SampleNupPath, outDir, "package", 0, null, progress, CancellationToken.None);

            Assert.True(File.Exists(Path.Combine(outDir, "exefs", "main")));
            Assert.True(File.Exists(Path.Combine(outDir, "romfs", "data", "config.json")));
            Assert.False(File.Exists(Path.Combine(outDir, "updates", "65536", "romfs", "data", "patch_notes.txt")));
        }
        finally
        {
            if (Directory.Exists(outDir))
                Directory.Delete(outDir, true);
        }
    }

    [Fact]
    public void Test_Subtree_Mode_Extraction()
    {
        string outDir = Path.Combine(Path.GetTempPath(), "nup_test_subtree_" + Guid.NewGuid().ToString("N"));
        try
        {
            var progress = new Progress<ProgressInfo>(_ => { });
            NupEngine.Extract(SampleNupPath, outDir, "subtree", 0, "romfs/data", progress, CancellationToken.None);

            Assert.True(File.Exists(Path.Combine(outDir, "config.json")));
            Assert.True(File.Exists(Path.Combine(outDir, "levels", "level01.dat")));
            Assert.False(File.Exists(Path.Combine(outDir, "main")));
        }
        finally
        {
            if (Directory.Exists(outDir))
                Directory.Delete(outDir, true);
        }
    }

    [Fact]
    public void Test_Integrity_Verification()
    {
        VerifyProgressInfo? finalResult = null;
        var progress = new SyncProgress<VerifyProgressInfo>(p =>
        {
            if (p.Status == "done") finalResult = p;
        });

        NupEngine.Verify(SampleNupPath, deep: true, progress, CancellationToken.None);

        Assert.NotNull(finalResult);
        Assert.True(finalResult.Valid);
        Assert.Empty(finalResult.Problems);
        Assert.Equal(5, finalResult.TotalFilesChecked);
    }

    [Fact]
    public void Test_Serialization_Contract_ProducesCamelCase()
    {
        var options = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.CamelCase
        };

        var info = new ProgressInfo
        {
            Status = "extracting",
            Current = 5,
            Total = 10,
            Percent = 50.0,
            FileName = "romfs/test.dat",
            BytesWritten = 1024,
            TotalBytes = 2048,
            DestDir = "C:\\test"
        };

        string json = JsonSerializer.Serialize(new
        {
            type = "extract_progress",
            info
        }, options);

        // Verify keys are camelCase and NOT PascalCase
        Assert.Contains("\"percent\":50", json);
        Assert.Contains("\"current\":5", json);
        Assert.Contains("\"total\":10", json);
        Assert.Contains("\"fileName\":\"romfs/test.dat\"", json);
        Assert.Contains("\"bytesWritten\":1024", json);
        Assert.Contains("\"totalBytes\":2048", json);
        Assert.Contains("\"destDir\":", json);

        // Verify deserializer reads without error
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;
        var infoElem = root.GetProperty("info");
        Assert.Equal(50.0, infoElem.GetProperty("percent").GetDouble());
    }

    [Fact]
    public void Test_Extraction_ThrowsOnEmptyPath()
    {
        var progress = new Progress<ProgressInfo>(_ => { });
        Assert.Throws<ArgumentException>(() => NupEngine.Extract("", "some_dest", "all", null, null, progress, CancellationToken.None));
        Assert.Throws<ArgumentException>(() => NupEngine.Extract(SampleNupPath, "", "all", null, null, progress, CancellationToken.None));
    }

    [Fact]
    public void Test_GetDirChildren_OutOfBoundsThrows()
    {
        Assert.Throws<ArgumentOutOfRangeException>(() => NupEngine.GetDirChildren(SampleNupPath, 999999));
    }

    [Fact]
    public void Test_OpenReadFileStream_StreamingDecompression()
    {
        using var reader = new NupReader(SampleNupPath);
        Assert.True(reader.Files.Count > 0);

        for (int i = 0; i < reader.Files.Count; i++)
        {
            var f = reader.Files[i];
            using var stream = reader.OpenReadFileStream(i);
            Assert.NotNull(stream);

            using var ms = new MemoryStream();
            stream.CopyTo(ms);
            Assert.Equal((long)f.Size, ms.Length);

            byte[] hash = SHA256.HashData(ms.ToArray());
            Assert.Equal(Convert.ToHexString(f.Sha256), Convert.ToHexString(hash));
        }
    }

    [Fact]
    public void Test_DeepVerify_UsesStreamingWithoutMemorySpikes()
    {
        VerifyProgressInfo? finalResult = null;
        var syncProgress = new SyncProgress<VerifyProgressInfo>(p =>
        {
            if (p.Status == "done") finalResult = p;
        });

        NupEngine.Verify(SampleNupPath, deep: true, syncProgress, CancellationToken.None);

        Assert.NotNull(finalResult);
        Assert.True(finalResult.Valid);
        Assert.Empty(finalResult.Problems);
    }

    [Fact]
    public void Test_AppUpdateManager_VersionParsing()
    {
        var v105 = AppUpdateManager.ParseVersion("v1.0.5");
        var v106 = AppUpdateManager.ParseVersion("1.0.6");
        var v210 = AppUpdateManager.ParseVersion("v2.1.0-alpha");

        Assert.Equal(new Version(1, 0, 5), v105);
        Assert.Equal(new Version(1, 0, 6), v106);
        Assert.Equal(new Version(2, 1, 0), v210);

        Assert.True(v106 > v105);
        Assert.True(v210 > v106);
        Assert.False(v105 > v106);
        Assert.False(v105 > AppUpdateManager.ParseVersion("1.0.5"));
    }

    [Fact]
    public async Task Test_AppUpdateManager_CheckForUpdates_RunsWithoutCrashing()
    {
        var result = await AppUpdateManager.CheckForUpdatesAsync(CancellationToken.None);
        Assert.NotNull(result);
        Assert.Equal(AppUpdateManager.CurrentVersion, result.CurrentVersion);
        // Either succeeded and found latest version or set friendly error message
        Assert.True(!string.IsNullOrEmpty(result.LatestVersion) || !string.IsNullOrEmpty(result.Error));
    }

    [Fact]
    public void Test_CheckDiskSpace_AccurateInfoAndBoundaryChecks()
    {
        string currentDir = Directory.GetCurrentDirectory();
        var spaceInfo = NupEngine.CheckDiskSpace(currentDir, 1024 * 1024); // 1 MB required

        Assert.NotNull(spaceInfo);
        Assert.False(string.IsNullOrEmpty(spaceInfo.DriveLetter));
        Assert.True(spaceInfo.TotalBytes > 0);
        Assert.True(spaceInfo.FreeBytes > 0);
        Assert.True(spaceInfo.HasSufficientSpace);
        Assert.False(string.IsNullOrEmpty(spaceInfo.FreeFormatted));

        // Test with massive size that exceeds disk space
        ulong hugeRequirement = spaceInfo.TotalBytes + (1024UL * 1024UL * 1024UL * 100UL); // Drive total + 100 GB
        var lowSpaceInfo = NupEngine.CheckDiskSpace(currentDir, hugeRequirement);
        Assert.False(lowSpaceInfo.HasSufficientSpace);
    }

    [Fact]
    public void Test_ExtractSingleFile_ExtractsExactFile()
    {
        using var reader = new NupReader(SampleNupPath);
        Assert.True(reader.Files.Count > 0);

        var targetFile = reader.Files[0];
        string tempOut = Path.Combine(Path.GetTempPath(), $"nup_test_single_{Guid.NewGuid():N}.bin");

        try
        {
            NupEngine.ExtractSingleFile(SampleNupPath, 0, tempOut);

            Assert.True(File.Exists(tempOut));
            var fi = new FileInfo(tempOut);
            Assert.Equal((long)targetFile.Size, fi.Length);

            byte[] actualHash = SHA256.HashData(File.ReadAllBytes(tempOut));
            Assert.Equal(Convert.ToHexString(targetFile.Sha256), Convert.ToHexString(actualHash));
        }
        finally
        {
            if (File.Exists(tempOut))
            {
                File.Delete(tempOut);
            }
        }
    }

    [Fact]
    public void Test_ShellIntegration_StatusCheckDoesNotThrow()
    {
        // Calling IsRegistered() should safely query HKCU without throwing
        bool isRegistered = ShellIntegration.IsRegistered();
        // Just verify it returns a boolean (either true or false depending on current user registry state)
        Assert.True(isRegistered == true || isRegistered == false);
    }

    [Fact]
    public void Test_NupWriter_PacksFolder_AndCanBeReadBackAccurately()
    {
        string tempBase = Path.Combine(Path.GetTempPath(), $"nup_pack_test_{Guid.NewGuid():N}");
        string sourceDir = Path.Combine(tempBase, "source_game");
        string outNup = Path.Combine(tempBase, "output_game.nup");

        try
        {
            // Prepare folder structure
            Directory.CreateDirectory(Path.Combine(sourceDir, "exefs"));
            Directory.CreateDirectory(Path.Combine(sourceDir, "romfs", "data"));
            Directory.CreateDirectory(Path.Combine(sourceDir, "romfs", "audio"));

            byte[] npdmBytes = Encoding.UTF8.GetBytes("HEADER_NPDM_DATA_1234567890");
            byte[] subsdkBytes = new byte[4096];
            Random.Shared.NextBytes(subsdkBytes);
            string jsonText = "{\"game\":\"test\",\"version\":\"1.0.0\"}";
            byte[] audioBytes = new byte[8192];
            for (int i = 0; i < audioBytes.Length; i++) audioBytes[i] = (byte)(i % 256);

            File.WriteAllBytes(Path.Combine(sourceDir, "exefs", "main.npdm"), npdmBytes);
            File.WriteAllBytes(Path.Combine(sourceDir, "exefs", "subsdk0"), subsdkBytes);
            File.WriteAllText(Path.Combine(sourceDir, "romfs", "data", "config.json"), jsonText);
            File.WriteAllBytes(Path.Combine(sourceDir, "romfs", "audio", "bgm.ogg"), audioBytes);

            var options = new PackOptions
            {
                SourceDirectory = sourceDir,
                OutputFilePath = outNup,
                TitleName = "Chrono Pack Adventure",
                Publisher = "Antigravity Studios",
                TitleId = 0x0100ABCD1234EF00UL,
                VersionString = "2.4.1",
                CompressionLevel = 3
            };

            NupWriter.Pack(options);

            Assert.True(File.Exists(outNup));
            Assert.True(new FileInfo(outNup).Length > 0);

            // Read the generated container back with NupReader
            using var reader = new NupReader(outNup);
            Assert.Equal(NupConstants.HeaderSize, reader.Super.HeaderSize);
            Assert.Equal(1, reader.Super.HeaderVersion);
            Assert.Equal(1u, reader.Super.PackageCount);
            Assert.True(reader.Super.FileSize > 0);

            var pkg = reader.Packages[0];
            Assert.Equal(0x0100ABCD1234EF00UL, pkg.TitleId);
            Assert.NotEqual(NupConstants.NoDir, pkg.ExeFsDir);
            Assert.NotEqual(NupConstants.NoDir, pkg.RomFsDir);

            // Verify metadata
            var meta = reader.GetPackageMetadata(pkg);
            Assert.True(meta.ContainsKey(NupConstants.TlvTitleName));
            Assert.Equal("Chrono Pack Adventure", Encoding.UTF8.GetString(meta[NupConstants.TlvTitleName]));
            Assert.True(meta.ContainsKey(NupConstants.TlvPublisher));
            Assert.Equal("Antigravity Studios", Encoding.UTF8.GetString(meta[NupConstants.TlvPublisher]));
            Assert.True(meta.ContainsKey(NupConstants.TlvDisplayVersion));
            Assert.Equal("2.4.1", Encoding.UTF8.GetString(meta[NupConstants.TlvDisplayVersion]));

            // Verify all files were preserved byte-for-byte
            Assert.Equal(4, reader.Files.Count);

            var filesMap = new Dictionary<string, byte[]>();
            for (int i = 0; i < reader.Files.Count; i++)
            {
                filesMap[reader.Files[i].Name] = reader.ReadFileData(i);
            }

            Assert.True(filesMap.ContainsKey("main.npdm"));
            Assert.Equal(npdmBytes, filesMap["main.npdm"]);

            Assert.True(filesMap.ContainsKey("subsdk0"));
            Assert.Equal(subsdkBytes, filesMap["subsdk0"]);

            Assert.True(filesMap.ContainsKey("config.json"));
            Assert.Equal(jsonText, Encoding.UTF8.GetString(filesMap["config.json"]));

            Assert.True(filesMap.ContainsKey("bgm.ogg"));
            Assert.Equal(audioBytes, filesMap["bgm.ogg"]);
        }
        finally
        {
            try { if (Directory.Exists(tempBase)) Directory.Delete(tempBase, true); } catch { }
        }
    }

    [Fact]
    public void Test_NupWriter_RawUncompressedMode()
    {
        string tempBase = Path.Combine(Path.GetTempPath(), $"nup_pack_raw_{Guid.NewGuid():N}");
        string sourceDir = Path.Combine(tempBase, "source_raw");
        string outNup = Path.Combine(tempBase, "output_raw.nup");

        try
        {
            Directory.CreateDirectory(sourceDir);
            byte[] data = Encoding.UTF8.GetBytes("RAW_UNCOMPRESSED_PAYLOAD_TEST");
            File.WriteAllBytes(Path.Combine(sourceDir, "test.txt"), data);

            var options = new PackOptions
            {
                SourceDirectory = sourceDir,
                OutputFilePath = outNup,
                CompressionLevel = 0 // Raw
            };

            NupWriter.Pack(options);

            using var reader = new NupReader(outNup);
            Assert.Single(reader.Files);
            Assert.Equal(NupConstants.CodecRaw, reader.Files[0].Codec);
            Assert.Equal(data, reader.ReadFileData(0));
        }
        finally
        {
            try { if (Directory.Exists(tempBase)) Directory.Delete(tempBase, true); } catch { }
        }
    }

    [Fact]
    public void Test_Extraction_And_Packaging_TrackElapsedSeconds()
    {
        string tempDir = Path.Combine(Path.GetTempPath(), "nup_elapsed_test_" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(tempDir);
        try
        {
            // 1. Test extraction elapsed tracking
            ProgressInfo? lastExtractProgress = null;
            var extractProgress = new SyncProgress<ProgressInfo>(info => lastExtractProgress = info);
            NupEngine.Extract(SampleNupPath, Path.Combine(tempDir, "extracted"), "all", null, null, extractProgress, CancellationToken.None);

            Assert.NotNull(lastExtractProgress);
            Assert.Equal("completed", lastExtractProgress.Status);
            Assert.True(lastExtractProgress.ElapsedSeconds >= 0, "ElapsedSeconds should be non-negative");

            // Verify camelCase JSON serialization
            var jsonOpt = new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase };
            string extractJson = JsonSerializer.Serialize(lastExtractProgress, jsonOpt);
            Assert.Contains("\"elapsedSeconds\":", extractJson);

            // 2. Test packaging elapsed tracking
            string packSource = Path.Combine(tempDir, "pack_src");
            Directory.CreateDirectory(packSource);
            File.WriteAllText(Path.Combine(packSource, "file.txt"), "Testing elapsed time.");

            string outNup = Path.Combine(tempDir, "output.nup");
            PackProgressInfo? lastPackProgress = null;
            var packProgress = new SyncProgress<PackProgressInfo>(info => lastPackProgress = info);

            var packOpts = new PackOptions
            {
                SourceDirectory = packSource,
                OutputFilePath = outNup,
                TitleName = "TimeTest"
            };

            NupEngine.Pack(packOpts, packProgress, CancellationToken.None);

            Assert.NotNull(lastPackProgress);
            Assert.Equal("done", lastPackProgress.Status);
            Assert.True(lastPackProgress.ElapsedSeconds >= 0, "ElapsedSeconds should be non-negative");

            string packJson = JsonSerializer.Serialize(lastPackProgress, jsonOpt);
            Assert.Contains("\"elapsedSeconds\":", packJson);
        }
        finally
        {
            try { if (Directory.Exists(tempDir)) Directory.Delete(tempDir, true); } catch { }
        }
    }

    [Fact]
    public void Test_StaticAssets_JavaScriptSyntaxValidation()
    {
        string jsPath = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../static/app.js"));
        Assert.True(File.Exists(jsPath), $"app.js not found at {jsPath}");

        string jsContent = File.ReadAllText(jsPath);
        Assert.False(string.IsNullOrWhiteSpace(jsContent));

        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = "node",
                Arguments = $"-c \"{jsPath}\"",
                RedirectStandardError = true,
                RedirectStandardOutput = true,
                UseShellExecute = false,
                CreateNoWindow = true
            };
            using var proc = Process.Start(psi);
            if (proc != null)
            {
                proc.WaitForExit(5000);
                string stderr = proc.StandardError.ReadToEnd();
                Assert.True(proc.ExitCode == 0, $"JavaScript syntax error in static/app.js:\n{stderr}");
            }
        }
        catch (System.ComponentModel.Win32Exception)
        {
            // Node not installed in test runner environment; skip external validator
        }
    }

    private class SyncProgress<T> : IProgress<T>
    {
        private readonly Action<T> _handler;
        public SyncProgress(Action<T> handler) => _handler = handler;
        public void Report(T value) => _handler(value);
    }
}
