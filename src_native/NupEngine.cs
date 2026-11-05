using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace NupExtractorApp;

public class ProgressInfo
{
    public string Status { get; set; } = "extracting";
    public int Current { get; set; }
    public int Total { get; set; }
    public double Percent { get; set; }
    public string FileName { get; set; } = "";
    public ulong FileSize { get; set; }
    public ulong BytesWritten { get; set; }
    public ulong TotalBytes { get; set; }
    public string? DestDir { get; set; }
    public string? Error { get; set; }
}

public class VerifyProgressInfo
{
    public string Status { get; set; } = "verifying";
    public int Current { get; set; }
    public int Total { get; set; }
    public double Percent { get; set; }
    public string FileName { get; set; } = "";
    public bool Valid { get; set; }
    public List<string> Problems { get; set; } = new();
    public int TotalFilesChecked { get; set; }
    public string? Error { get; set; }
}

public class DiskSpaceInfo
{
    public bool HasSufficientSpace { get; set; }
    public ulong FreeBytes { get; set; }
    public ulong RequiredBytes { get; set; }
    public ulong TotalBytes { get; set; }
    public string FreeFormatted { get; set; } = "";
    public string RequiredFormatted { get; set; } = "";
    public string DriveName { get; set; } = "";
    public string DriveLetter => DriveName;
}

public static class NupEngine
{
    public static string FormatBytes(ulong size)
    {
        if (size < 1024)
            return $"{size} B";
        if (size < 1024 * 1024)
            return $"{size / 1024.0:F2} KB";
        if (size < 1024 * 1024 * 1024)
            return $"{size / (1024.0 * 1024):F2} MB";
        return $"{size / (1024.0 * 1024 * 1024):F2} GB";
    }

    public static Dictionary<string, object> Inspect(string path)
    {
        using var reader = new NupReader(path);
        var s = reader.Super;

        string? primaryTitle = null;
        string? primaryPublisher = null;
        string? primaryVersion = null;
        string? primaryIcon = null;

        ulong totalUncompressed = 0;
        ulong totalCompressed = 0;

        var packagesList = new List<Dictionary<string, object>>();

        foreach (var pkg in reader.Packages)
        {
            var meta = reader.GetPackageMetadata(pkg);

            string? titleName = meta.TryGetValue(NupConstants.TlvTitleName, out var tb)
                ? Encoding.UTF8.GetString(tb) : null;
            string? publisher = meta.TryGetValue(NupConstants.TlvPublisher, out var pb)
                ? Encoding.UTF8.GetString(pb) : null;
            string? dispVer = meta.TryGetValue(NupConstants.TlvDisplayVersion, out var vb)
                ? Encoding.UTF8.GetString(vb) : null;
            string? buildId = meta.TryGetValue(NupConstants.TlvBuildId, out var bid)
                ? Convert.ToHexString(bid).ToUpperInvariant() : null;
            string? srcDump = meta.TryGetValue(NupConstants.TlvSourceDump, out var sb)
                ? Encoding.UTF8.GetString(sb) : null;

            string? iconDataUri = null;
            if (meta.TryGetValue(NupConstants.TlvIcon, out var iconBytes) && iconBytes.Length > 0)
            {
                string mime = (iconBytes.Length > 4 && iconBytes[0] == 0x89 && iconBytes[1] == 'P' && iconBytes[2] == 'N' && iconBytes[3] == 'G')
                    ? "image/png" : "image/jpeg";
                iconDataUri = $"data:{mime};base64,{Convert.ToBase64String(iconBytes)}";
            }

            if (pkg.Kind == NupConstants.KindBase || primaryTitle == null)
            {
                if (titleName != null) primaryTitle = titleName;
                if (publisher != null) primaryPublisher = publisher;
                if (dispVer != null) primaryVersion = dispVer;
                if (iconDataUri != null) primaryIcon = iconDataUri;
            }

            var (fileCount, uncomp, comp) = CalculateSubtreeStats(reader, pkg.RootDir);
            totalUncompressed += uncomp;
            totalCompressed += comp;

            packagesList.Add(new Dictionary<string, object>
            {
                ["index"] = pkg.Index,
                ["kind"] = pkg.Kind,
                ["kind_name"] = pkg.KindName,
                ["kindName"] = pkg.KindName,
                ["title_id"] = pkg.TitleIdHex,
                ["titleId"] = pkg.TitleIdHex,
                ["version"] = pkg.Version,
                ["version_str"] = pkg.VersionStr,
                ["versionStr"] = pkg.VersionStr,
                ["display_version"] = dispVer ?? pkg.VersionStr,
                ["displayVersion"] = dispVer ?? pkg.VersionStr,
                ["title_name"] = titleName ?? $"Title {pkg.TitleIdHex}",
                ["titleName"] = titleName ?? $"Title {pkg.TitleIdHex}",
                ["publisher"] = publisher ?? "Unknown Publisher",
                ["build_id"] = (object?)buildId ?? "",
                ["buildId"] = (object?)buildId ?? "",
                ["source_dump"] = (object?)srcDump ?? "",
                ["sourceDump"] = (object?)srcDump ?? "",
                ["has_icon"] = iconDataUri != null,
                ["hasIcon"] = iconDataUri != null,
                ["icon_preview"] = (object?)iconDataUri ?? "",
                ["iconPreview"] = (object?)iconDataUri ?? "",
                ["has_exefs"] = pkg.ExeFsDir != NupConstants.NoDir,
                ["hasExeFs"] = pkg.ExeFsDir != NupConstants.NoDir,
                ["has_romfs"] = pkg.RomFsDir != NupConstants.NoDir,
                ["hasRomFs"] = pkg.RomFsDir != NupConstants.NoDir,
                ["root_dir_index"] = pkg.RootDir,
                ["rootDirIndex"] = pkg.RootDir,
                ["exefs_dir_index"] = pkg.ExeFsDir,
                ["exeFsDirIndex"] = pkg.ExeFsDir,
                ["romfs_dir_index"] = pkg.RomFsDir,
                ["romFsDirIndex"] = pkg.RomFsDir,
                ["file_count"] = fileCount,
                ["fileCount"] = fileCount,
                ["uncompressed_bytes"] = uncomp,
                ["uncompressedBytes"] = uncomp,
                ["uncompressed_formatted"] = FormatBytes(uncomp),
                ["uncompressedFormatted"] = FormatBytes(uncomp),
                ["compressed_bytes"] = comp,
                ["compressedBytes"] = comp,
                ["compressed_formatted"] = FormatBytes(comp),
                ["compressedFormatted"] = FormatBytes(comp)
            });
        }

        double compressionRatio = 0.0;
        if (totalUncompressed > 0 && s.FileSize < totalUncompressed)
        {
            compressionRatio = Math.Round((1.0 - ((double)s.FileSize / totalUncompressed)) * 100.0, 1);
        }

        string fileName = Path.GetFileName(path);
        string baseName = Path.GetFileNameWithoutExtension(fileName);
        string parentDir = Path.GetDirectoryName(Path.GetFullPath(path)) ?? Directory.GetCurrentDirectory();
        string defaultDest = Path.Combine(parentDir, $"{primaryTitle ?? baseName}_extracted");

        var metaDict = new Dictionary<string, object>
        {
            ["title_name"] = primaryTitle ?? baseName,
            ["titleName"] = primaryTitle ?? baseName,
            ["publisher"] = primaryPublisher ?? "Nintendo Switch Dump",
            ["display_version"] = primaryVersion ?? "1.0.0",
            ["displayVersion"] = primaryVersion ?? "1.0.0",
            ["icon"] = (object?)primaryIcon ?? ""
        };

        var superblockDict = new Dictionary<string, object>
        {
            ["magic"] = "NUP1",
            ["header_size"] = s.HeaderSize,
            ["headerSize"] = s.HeaderSize,
            ["header_version"] = s.HeaderVersion,
            ["headerVersion"] = s.HeaderVersion,
            ["toc_offset"] = s.TocOffset,
            ["tocOffset"] = s.TocOffset,
            ["toc_size"] = s.TocSize,
            ["tocSize"] = s.TocSize,
            ["toc_sha256"] = Convert.ToHexString(s.TocSha256).ToLowerInvariant(),
            ["tocSha256"] = Convert.ToHexString(s.TocSha256).ToLowerInvariant(),
            ["blob_offset"] = s.BlobOffset,
            ["blobOffset"] = s.BlobOffset,
            ["package_count"] = s.PackageCount,
            ["packageCount"] = s.PackageCount,
            ["uuid"] = s.FormattedUuid
        };

        var statsDict = new Dictionary<string, object>
        {
            ["total_dirs"] = reader.Dirs.Count,
            ["totalDirs"] = reader.Dirs.Count,
            ["total_files"] = reader.Files.Count,
            ["totalFiles"] = reader.Files.Count,
            ["total_uncompressed_bytes"] = totalUncompressed,
            ["totalUncompressedBytes"] = totalUncompressed,
            ["total_uncompressed_formatted"] = FormatBytes(totalUncompressed),
            ["totalUncompressedFormatted"] = FormatBytes(totalUncompressed),
            ["compression_ratio"] = compressionRatio,
            ["compressionRatio"] = compressionRatio
        };

        return new Dictionary<string, object>
        {
            ["file_path"] = reader.FilePath,
            ["filePath"] = reader.FilePath,
            ["file_name"] = fileName,
            ["fileName"] = fileName,
            ["base_name"] = baseName,
            ["baseName"] = baseName,
            ["file_size"] = s.FileSize,
            ["fileSize"] = s.FileSize,
            ["file_size_formatted"] = FormatBytes(s.FileSize),
            ["fileSizeFormatted"] = FormatBytes(s.FileSize),
            ["default_dest_dir"] = defaultDest,
            ["defaultDestDir"] = defaultDest,
            ["superblock"] = superblockDict,
            ["primary_meta"] = metaDict,
            ["primaryMeta"] = metaDict,
            ["packages"] = packagesList,
            ["stats"] = statsDict
        };
    }

    private static (int count, ulong uncomp, ulong comp) CalculateSubtreeStats(NupReader reader, uint dirIndex)
    {
        int count = 0;
        ulong uncomp = 0;
        ulong comp = 0;
        var stack = new Stack<uint>();
        stack.Push(dirIndex);

        while (stack.Count > 0)
        {
            uint di = stack.Pop();
            foreach (var (isDir, idx, _) in reader.GetChildren(di))
            {
                if (isDir)
                {
                    stack.Push((uint)idx);
                }
                else
                {
                    count++;
                    var f = reader.Files[idx];
                    uncomp += f.Size;
                    comp += f.ExtentSize;
                }
            }
        }

        return (count, uncomp, comp);
    }

    public static Dictionary<string, object> GetDirChildren(string path, uint dirIndex)
    {
        using var reader = new NupReader(path);
        if (dirIndex >= (uint)reader.Dirs.Count)
            throw new ArgumentOutOfRangeException(nameof(dirIndex));

        string dname = reader.Dirs[(int)dirIndex].Name;
        var children = new List<Dictionary<string, object>>();

        foreach (var (isDir, idx, name) in reader.GetChildren(dirIndex))
        {
            if (isDir)
            {
                var d = reader.Dirs[idx];
                children.Add(new Dictionary<string, object>
                {
                    ["type"] = "dir",
                    ["index"] = idx,
                    ["name"] = name,
                    ["child_count"] = d.ChildCount,
                    ["childCount"] = d.ChildCount,
                    ["has_children"] = d.ChildCount > 0,
                    ["hasChildren"] = d.ChildCount > 0
                });
            }
            else
            {
                var f = reader.Files[idx];
                children.Add(new Dictionary<string, object>
                {
                    ["type"] = "file",
                    ["index"] = idx,
                    ["name"] = name,
                    ["size"] = f.Size,
                    ["size_formatted"] = FormatBytes(f.Size),
                    ["sizeFormatted"] = FormatBytes(f.Size),
                    ["extent_size"] = f.ExtentSize,
                    ["extentSize"] = f.ExtentSize,
                    ["extent_size_formatted"] = FormatBytes(f.ExtentSize),
                    ["extentSizeFormatted"] = FormatBytes(f.ExtentSize),
                    ["codec"] = f.Codec == NupConstants.CodecZstd ? "zstd" : "raw",
                    ["sha256"] = Convert.ToHexString(f.Sha256).ToLowerInvariant()
                });
            }
        }

        return new Dictionary<string, object>
        {
            ["type"] = "dir",
            ["index"] = dirIndex,
            ["name"] = string.IsNullOrEmpty(dname) ? "root" : dname,
            ["children"] = children
        };
    }

    public static List<Dictionary<string, object>> SearchFiles(string path, string query, int limit = 100)
    {
        using var reader = new NupReader(path);
        var results = new List<Dictionary<string, object>>();
        string qLower = query.ToLowerInvariant();

        for (int i = 0; i < reader.Files.Count; i++)
        {
            var f = reader.Files[i];
            if (f.Name.ToLowerInvariant().Contains(qLower))
            {
                results.Add(new Dictionary<string, object>
                {
                    ["index"] = i,
                    ["name"] = f.Name,
                    ["size"] = f.Size,
                    ["size_formatted"] = FormatBytes(f.Size),
                    ["sizeFormatted"] = FormatBytes(f.Size),
                    ["extent_size"] = f.ExtentSize,
                    ["extentSize"] = f.ExtentSize,
                    ["extent_size_formatted"] = FormatBytes(f.ExtentSize),
                    ["extentSizeFormatted"] = FormatBytes(f.ExtentSize),
                    ["codec"] = f.Codec == NupConstants.CodecZstd ? "zstd" : "raw",
                    ["sha256"] = Convert.ToHexString(f.Sha256).ToLowerInvariant()
                });
                if (results.Count >= limit) break;
            }
        }
        return results;
    }

    public static void Extract(
        string nupPath,
        string destDir,
        string mode,
        int? packageIndex,
        string? subtreePath,
        IProgress<ProgressInfo> progress,
        CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(nupPath))
            throw new ArgumentException("NUP file path cannot be empty.", nameof(nupPath));
        if (string.IsNullOrWhiteSpace(destDir))
            throw new ArgumentException("Destination folder cannot be empty.", nameof(destDir));

        string cleanNupPath = nupPath.Trim().Trim('"', '\'');
        string cleanDestDir = destDir.Trim().Trim('"', '\'');

        using var reader = new NupReader(cleanNupPath);
        Directory.CreateDirectory(cleanDestDir);

        var filesToExtract = new List<(int fileIndex, string relPath)>();

        if (mode == "subtree")
        {
            int pIndex = packageIndex ?? 0;
            if (pIndex >= reader.Packages.Count)
                throw new ArgumentException($"Invalid package index {pIndex}");
            var pkg = reader.Packages[pIndex];

            uint targetDir = pkg.RootDir;
            if (!string.IsNullOrEmpty(subtreePath))
            {
                targetDir = ResolvePath(reader, pkg.RootDir, subtreePath);
            }
            GatherFiles(reader, targetDir, "", filesToExtract);
        }
        else if (mode == "package")
        {
            int pIndex = packageIndex ?? 0;
            if (pIndex >= reader.Packages.Count)
                throw new ArgumentException($"Invalid package index {pIndex}");
            var pkg = reader.Packages[pIndex];
            GatherFiles(reader, pkg.RootDir, "", filesToExtract);
        }
        else // "all"
        {
            bool singlePkg = reader.Packages.Count == 1;
            foreach (var pkg in reader.Packages)
            {
                string prefix = singlePkg ? "" :
                    pkg.Kind == NupConstants.KindBase ? "base" :
                    pkg.Kind == NupConstants.KindUpdate ? Path.Combine("updates", pkg.Version.ToString()) :
                    pkg.Kind is NupConstants.KindAddon or NupConstants.KindAddonUpdate ?
                        Path.Combine("dlc", pkg.TitleIdHex.ToLowerInvariant(), pkg.Index.ToString()) :
                        $"pkg_{pkg.Index}_{pkg.KindName}";

                GatherFiles(reader, pkg.RootDir, prefix, filesToExtract);
            }
        }

        int total = filesToExtract.Count;
        ulong totalBytes = 0;
        foreach (var (fi, _) in filesToExtract)
            totalBytes += reader.Files[fi].Size;

        ulong bytesWritten = 0;
        long lastReportTime = 0;

        progress.Report(new ProgressInfo
        {
            Status = "starting",
            Current = 0,
            Total = total,
            Percent = 0.0,
            FileName = "Initializing extraction...",
            BytesWritten = 0,
            TotalBytes = totalBytes,
            DestDir = cleanDestDir
        });

        var sw = Stopwatch.StartNew();

        for (int i = 0; i < total; i++)
        {
            ct.ThrowIfCancellationRequested();

            var (fi, relPath) = filesToExtract[i];
            string safeRelPath = SanitizeRelativePath(relPath);
            string fullPath = Path.Combine(cleanDestDir, safeRelPath);
            string? dir = Path.GetDirectoryName(fullPath);
            if (!string.IsNullOrEmpty(dir))
                Directory.CreateDirectory(dir);

            try
            {
                using (var fs = new FileStream(fullPath, FileMode.Create, FileAccess.Write, FileShare.None, 131072)) // 128 KB buffer
                {
                    reader.ExtractFileToStream(fi, fs);
                }
            }
            catch (Exception ex)
            {
                throw new InvalidOperationException($"Failed to extract '{relPath}' ({FormatBytes(reader.Files[fi].Size)}): {ex.Message}", ex);
            }

            ulong fSize = reader.Files[fi].Size;
            bytesWritten += fSize;

            long now = sw.ElapsedMilliseconds;
            if (now - lastReportTime >= 20 || i == total - 1)
            {
                lastReportTime = now;
                double pct = total > 0 ? Math.Round(((i + 1) / (double)total) * 100.0, 1) : 100.0;
                progress.Report(new ProgressInfo
                {
                    Status = "extracting",
                    Current = i + 1,
                    Total = total,
                    Percent = pct,
                    FileName = relPath.Replace('\\', '/'),
                    FileSize = fSize,
                    BytesWritten = bytesWritten,
                    TotalBytes = totalBytes,
                    DestDir = cleanDestDir
                });
            }
        }

        progress.Report(new ProgressInfo
        {
            Status = "completed",
            Current = total,
            Total = total,
            Percent = 100.0,
            FileName = "Done",
            BytesWritten = bytesWritten,
            TotalBytes = totalBytes,
            DestDir = cleanDestDir
        });
    }

    private static uint ResolvePath(NupReader reader, uint rootDir, string relPath)
    {
        uint current = rootDir;
        foreach (var part in relPath.Split(new[] { '/', '\\' }, StringSplitOptions.RemoveEmptyEntries))
        {
            bool found = false;
            foreach (var (isDir, idx, name) in reader.GetChildren(current))
            {
                if (isDir && string.Equals(name, part, StringComparison.OrdinalIgnoreCase))
                {
                    current = (uint)idx;
                    found = true;
                    break;
                }
            }
            if (!found)
                throw new DirectoryNotFoundException($"Subtree path component '{part}' not found");
        }
        return current;
    }

    private static void GatherFiles(NupReader reader, uint dirIndex, string prefix, List<(int fileIndex, string relPath)> list)
    {
        var stack = new Stack<(uint di, string pref)>();
        stack.Push((dirIndex, prefix));

        while (stack.Count > 0)
        {
            var (di, p) = stack.Pop();
            foreach (var (isDir, idx, name) in reader.GetChildren(di))
            {
                string rel = string.IsNullOrEmpty(p) ? name : Path.Combine(p, name);
                if (isDir)
                {
                    stack.Push(((uint)idx, rel));
                }
                else
                {
                    list.Add((idx, rel));
                }
            }
        }
    }

    private static string SanitizeRelativePath(string relPath)
    {
        var invalid = Path.GetInvalidFileNameChars();
        var parts = relPath.Split(new[] { '/', '\\' }, StringSplitOptions.RemoveEmptyEntries);
        for (int i = 0; i < parts.Length; i++)
        {
            var sb = new StringBuilder(parts[i].Length);
            foreach (char c in parts[i])
            {
                sb.Append(Array.IndexOf(invalid, c) >= 0 ? '_' : c);
            }
            parts[i] = sb.ToString();
        }
        return Path.Combine(parts);
    }

    public static void Verify(string nupPath, bool deep, IProgress<VerifyProgressInfo> progress, CancellationToken ct)
    {
        var problems = new List<string>();
        string cleanNupPath = nupPath.Trim().Trim('"', '\'');
        using var reader = new NupReader(cleanNupPath);

        int total = reader.Files.Count;

        if (deep)
        {
            long lastReport = 0;
            var sw = Stopwatch.StartNew();

            for (int i = 0; i < total; i++)
            {
                ct.ThrowIfCancellationRequested();
                var f = reader.Files[i];

                try
                {
                    using var stream = reader.OpenReadFileStream(i);
                    byte[] hash = SHA256.HashData(stream);
                    if (!hash.AsSpan().SequenceEqual(f.Sha256))
                    {
                        problems.Add($"File #{i} ({f.Name}): SHA-256 hash mismatch");
                    }
                }
                catch (Exception ex)
                {
                    problems.Add($"File #{i} ({f.Name}): Failed to decompress/read: {ex.Message}");
                }

                long now = sw.ElapsedMilliseconds;
                if (now - lastReport >= 25 || i == total - 1)
                {
                    lastReport = now;
                    double pct = Math.Round(((i + 1) / (double)total) * 100.0, 1);
                    progress.Report(new VerifyProgressInfo
                    {
                        Status = "verifying",
                        Current = i + 1,
                        Total = total,
                        Percent = pct,
                        FileName = f.Name
                    });
                }
            }
        }

        progress.Report(new VerifyProgressInfo
        {
            Status = "done",
            Current = total,
            Total = total,
            Percent = 100.0,
            Valid = problems.Count == 0,
            Problems = problems,
            TotalFilesChecked = total
        });
    }

    public static void OpenFolder(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return;
        string clean = path.Trim().Trim('"', '\'');
        if (Directory.Exists(clean) || File.Exists(clean))
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = clean,
                UseShellExecute = true
            });
        }
    }

    public static DiskSpaceInfo CheckDiskSpace(string destinationDir, ulong requiredBytes)
    {
        try
        {
            if (string.IsNullOrWhiteSpace(destinationDir))
            {
                destinationDir = Directory.GetCurrentDirectory();
            }

            string fullPath = Path.GetFullPath(destinationDir);
            string root = Path.GetPathRoot(fullPath) ?? "";
            var drive = new DriveInfo(root);

            ulong freeBytes = (ulong)drive.AvailableFreeSpace;
            ulong totalBytes = (ulong)drive.TotalSize;
            return new DiskSpaceInfo
            {
                HasSufficientSpace = freeBytes >= requiredBytes,
                FreeBytes = freeBytes,
                TotalBytes = totalBytes,
                RequiredBytes = requiredBytes,
                FreeFormatted = FormatBytes(freeBytes),
                RequiredFormatted = FormatBytes(requiredBytes),
                DriveName = drive.Name
            };
        }
        catch
        {
            return new DiskSpaceInfo
            {
                HasSufficientSpace = true,
                FreeBytes = ulong.MaxValue,
                RequiredBytes = requiredBytes,
                FreeFormatted = "Unknown",
                RequiredFormatted = FormatBytes(requiredBytes),
                DriveName = destinationDir
            };
        }
    }

    public static void ExtractSingleFile(string nupPath, int fileIndex, string destFilePath)
    {
        if (string.IsNullOrWhiteSpace(nupPath))
            throw new ArgumentException("NUP path cannot be empty.", nameof(nupPath));
        if (string.IsNullOrWhiteSpace(destFilePath))
            throw new ArgumentException("Destination file path cannot be empty.", nameof(destFilePath));

        string cleanNup = nupPath.Trim().Trim('"', '\'');
        string cleanDest = destFilePath.Trim().Trim('"', '\'');

        using var reader = new NupReader(cleanNup);
        if (fileIndex < 0 || fileIndex >= reader.Files.Count)
            throw new ArgumentOutOfRangeException(nameof(fileIndex));

        string? dir = Path.GetDirectoryName(cleanDest);
        if (!string.IsNullOrEmpty(dir))
            Directory.CreateDirectory(dir);

        using var fs = new FileStream(cleanDest, FileMode.Create, FileAccess.Write, FileShare.None, 131072);
        reader.ExtractFileToStream(fileIndex, fs);
    }

    public static void Pack(PackOptions options, IProgress<PackProgressInfo>? progress = null, CancellationToken ct = default)
    {
        NupWriter.Pack(options, progress, ct);
    }
}
