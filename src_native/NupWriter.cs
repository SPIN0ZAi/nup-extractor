using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using ZstdSharp;

namespace NupExtractorApp;

public class PackOptions
{
    public string SourceDirectory { get; set; } = "";
    public string OutputFilePath { get; set; } = "";
    public string TitleName { get; set; } = "Custom Game";
    public string Publisher { get; set; } = "Homebrew / Mod";
    public ulong TitleId { get; set; } = 0;
    public string VersionString { get; set; } = "1.0.0";
    public int CompressionLevel { get; set; } = 3; // 0 = Raw, 1-19 = Zstd
    public string? IconPath { get; set; }
}

public class PackProgressInfo
{
    public string Status { get; set; } = "packing"; // "scanning", "packing", "finalizing", "done", "error"
    public string CurrentFile { get; set; } = "";
    public int FilesPacked { get; set; }
    public int TotalFiles { get; set; }
    public ulong BytesProcessed { get; set; }
    public ulong TotalBytes { get; set; }
    public ulong BytesWritten { get; set; }
    public double CompressionRatio { get; set; }
    public string SpeedFormatted { get; set; } = "";
    public double Percent { get; set; }
    public string? Error { get; set; }
    public double ElapsedSeconds { get; set; }
}

public static class NupWriter
{
    private class DirEntry
    {
        public int Index { get; set; }
        public string Name { get; set; } = "";
        public string FullPath { get; set; } = "";
        public uint FirstChild { get; set; }
        public uint ChildCount { get; set; }
        public List<int> SubDirIndices { get; } = new();
        public List<int> FileIndices { get; } = new();
    }

    private class FileEntry
    {
        public int Index { get; set; }
        public string Name { get; set; } = "";
        public string FullPath { get; set; } = "";
        public ulong Size { get; set; }
        public ulong ExtentOffset { get; set; }
        public ulong ExtentSize { get; set; }
        public byte[] Sha256 { get; set; } = new byte[32];
        public byte Codec { get; set; } = NupConstants.CodecRaw;
    }

    private class DiscoveredPackage
    {
        public byte Kind { get; set; }
        public string SourcePath { get; set; } = "";
        public ulong TitleId { get; set; }
        public uint Version { get; set; }
        public byte Flags { get; set; }
        public string DisplayName { get; set; } = "";
        public DirEntry? RootDir { get; set; }
        public uint ExeFsDirIndex { get; set; } = NupConstants.NoDir;
        public uint RomFsDirIndex { get; set; } = NupConstants.NoDir;
        public byte[] MetaBytes { get; set; } = Array.Empty<byte>();
    }

    public static void Pack(PackOptions options, IProgress<PackProgressInfo>? progress = null, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(options.SourceDirectory) || !Directory.Exists(options.SourceDirectory))
            throw new DirectoryNotFoundException($"Source directory not found: {options.SourceDirectory}");

        if (string.IsNullOrWhiteSpace(options.OutputFilePath))
            throw new ArgumentException("Output file path cannot be empty.", nameof(options.OutputFilePath));

        string? outDir = Path.GetDirectoryName(options.OutputFilePath);
        if (!string.IsNullOrEmpty(outDir))
            Directory.CreateDirectory(outDir);

        // 1. Discover packages (base, updates, dlc, or single package)
        var discoveredPackages = DiscoverPackages(options);

        string discoveryMsg = discoveredPackages.Count > 1
            ? $"Found {discoveredPackages.Count} packages: {string.Join(", ", discoveredPackages.Select(p => p.Kind == NupConstants.KindBase ? "Base" : p.Kind == NupConstants.KindUpdate ? $"Update (v{p.Version >> 16}.{(p.Version >> 8) & 0xFF}.{p.Version & 0xFF})" : "DLC"))}..."
            : "Scanning directory hierarchy...";

        progress?.Report(new PackProgressInfo
        {
            Status = "scanning",
            CurrentFile = discoveryMsg,
            Percent = 0.0
        });

        var dirs = new List<DirEntry>();
        var files = new List<FileEntry>();
        ulong totalUncompressedBytes = 0;

        foreach (var pkg in discoveredPackages)
        {
            var pkgRoot = new DirEntry
            {
                Index = dirs.Count,
                Name = "",
                FullPath = Path.GetFullPath(pkg.SourcePath)
            };
            dirs.Add(pkgRoot);
            pkg.RootDir = pkgRoot;

            ScanDirectory(pkgRoot, dirs, files, ref totalUncompressedBytes, ct);

            // Locate exefs and romfs among pkgRoot direct subdirectories
            foreach (int childDirIdx in pkgRoot.SubDirIndices)
            {
                var childDir = dirs[childDirIdx];
                if (string.Equals(childDir.Name, "exefs", StringComparison.OrdinalIgnoreCase) && SubtreeHasFiles(childDir, dirs))
                {
                    pkg.ExeFsDirIndex = (uint)childDirIdx;
                }
                else if (string.Equals(childDir.Name, "romfs", StringComparison.OrdinalIgnoreCase) && SubtreeHasFiles(childDir, dirs))
                {
                    pkg.RomFsDirIndex = (uint)childDirIdx;
                }
            }
        }

        if (files.Count == 0)
            throw new InvalidOperationException($"No packable files found in '{options.SourceDirectory}'.");

        // 2. Build String Pool
        var stringPoolStream = new MemoryStream();
        var dirNameOffsets = new uint[dirs.Count];
        var dirNameLengths = new uint[dirs.Count];

        for (int i = 0; i < dirs.Count; i++)
        {
            byte[] nameBytes = Encoding.UTF8.GetBytes(dirs[i].Name);
            dirNameOffsets[i] = (uint)stringPoolStream.Position;
            dirNameLengths[i] = (uint)nameBytes.Length;
            stringPoolStream.Write(nameBytes);
        }

        var fileNameOffsets = new uint[files.Count];
        var fileNameLengths = new uint[files.Count];

        for (int i = 0; i < files.Count; i++)
        {
            byte[] nameBytes = Encoding.UTF8.GetBytes(files[i].Name);
            fileNameOffsets[i] = (uint)stringPoolStream.Position;
            fileNameLengths[i] = (uint)nameBytes.Length;
            stringPoolStream.Write(nameBytes);
        }

        byte[] stringPool = stringPoolStream.ToArray();

        // 3. Build Children array and set FirstChild / ChildCount on directories
        var childrenList = new List<uint>();
        foreach (var d in dirs)
        {
            d.FirstChild = (uint)childrenList.Count;
            d.ChildCount = (uint)(d.SubDirIndices.Count + d.FileIndices.Count);

            foreach (int subIdx in d.SubDirIndices)
                childrenList.Add((uint)subIdx);

            foreach (int fileIdx in d.FileIndices)
                childrenList.Add((uint)(dirs.Count + fileIdx));
        }

        // 4. Build Metadata TLV block for each package
        int totalMetaSize = 0;
        foreach (var pkg in discoveredPackages)
        {
            pkg.MetaBytes = BuildPackageMetadataBlock(pkg, options);
            totalMetaSize += pkg.MetaBytes.Length;
        }

        // 5. Calculate TOC size and layout
        const int superblockSize = NupConstants.HeaderSize; // 0x80 = 128 bytes
        const int tocHeaderSize = 48;
        int packagesSize = discoveredPackages.Count * NupConstants.PkgSize;
        int dirsTableSize = dirs.Count * NupConstants.DirSize;
        int filesTableSize = files.Count * NupConstants.FileSize;
        int childrenTableSize = childrenList.Count * 4;
        int poolSize = stringPool.Length;

        int tocSize = tocHeaderSize + packagesSize + dirsTableSize + filesTableSize + childrenTableSize + poolSize + totalMetaSize;
        ulong tocOffset = superblockSize; // 128
        ulong blobOffset = AlignUp(tocOffset + (ulong)tocSize, (ulong)NupConstants.ExtentAlign);

        // 6. Open output stream and write extents
        using var fs = new FileStream(options.OutputFilePath, FileMode.Create, FileAccess.ReadWrite, FileShare.None, 1048576);

        // Seek past Superblock & TOC straight to BlobOffset
        fs.Seek((long)blobOffset, SeekOrigin.Begin);

        ulong currentExtentOffset = blobOffset;
        ulong processedBytes = 0;
        ulong writtenBlobBytes = 0;

        var sw = Stopwatch.StartNew();
        long lastReportMs = 0;
        long bytesSinceLast = 0;
        Span<byte> padZeros = stackalloc byte[16];

        for (int i = 0; i < files.Count; i++)
        {
            ct.ThrowIfCancellationRequested();

            var fe = files[i];
            fe.ExtentOffset = currentExtentOffset;

            // Align to 16 bytes
            ulong alignedExtent = AlignUp(currentExtentOffset, (ulong)NupConstants.ExtentAlign);
            if (alignedExtent > currentExtentOffset)
            {
                int pad = (int)(alignedExtent - currentExtentOffset);
                fs.Write(padZeros.Slice(0, pad));
                currentExtentOffset = alignedExtent;
                fe.ExtentOffset = currentExtentOffset;
            }

            long extentStartPos = fs.Position;
            byte[] fileSha;

            bool useZstd = options.CompressionLevel > 0;

            if (fe.Size == 0)
            {
                fe.ExtentSize = 0;
                fe.Sha256 = SHA256.HashData(Array.Empty<byte>());
                fe.Codec = NupConstants.CodecRaw;
            }
            else if (!useZstd)
            {
                // Raw uncompressed write
                using var inStream = new FileStream(fe.FullPath, FileMode.Open, FileAccess.Read, FileShare.Read, 131072);
                using var sha = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
                byte[] buf = new byte[131072];

                while (true)
                {
                    int r = inStream.Read(buf, 0, buf.Length);
                    if (r == 0) break;

                    sha.AppendData(buf, 0, r);
                    fs.Write(buf, 0, r);

                    processedBytes += (ulong)r;
                    bytesSinceLast += r;
                    writtenBlobBytes += (ulong)r;
                }

                fe.ExtentSize = (ulong)(fs.Position - extentStartPos);
                fe.Sha256 = sha.GetHashAndReset();
                fe.Codec = NupConstants.CodecRaw;
                currentExtentOffset += fe.ExtentSize;
            }
            else
            {
                // Stream-compress with Zstandard
                using var inStream = new FileStream(fe.FullPath, FileMode.Open, FileAccess.Read, FileShare.Read, 131072);
                using var sha = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);

                long fileExtentStart = fs.Position;
                using (var compressorStream = new CompressionStream(fs, options.CompressionLevel, leaveOpen: true))
                {
                    byte[] buf = new byte[131072];
                    while (true)
                    {
                        int r = inStream.Read(buf, 0, buf.Length);
                        if (r == 0) break;

                        sha.AppendData(buf, 0, r);
                        compressorStream.Write(buf, 0, r);

                        processedBytes += (ulong)r;
                        bytesSinceLast += r;
                    }
                }

                long compressedSize = fs.Position - fileExtentStart;
                fileSha = sha.GetHashAndReset();

                // If compressed size is larger than raw, revert and store raw
                if ((ulong)compressedSize >= fe.Size)
                {
                    fs.Seek(fileExtentStart, SeekOrigin.Begin);
                    inStream.Seek(0, SeekOrigin.Begin);

                    byte[] buf = new byte[131072];
                    while (true)
                    {
                        int r = inStream.Read(buf, 0, buf.Length);
                        if (r == 0) break;
                        fs.Write(buf, 0, r);
                    }

                    fe.ExtentSize = fe.Size;
                    fe.Sha256 = fileSha;
                    fe.Codec = NupConstants.CodecRaw;
                }
                else
                {
                    fe.ExtentSize = (ulong)compressedSize;
                    fe.Sha256 = fileSha;
                    fe.Codec = NupConstants.CodecZstd;
                }

                writtenBlobBytes += fe.ExtentSize;
                currentExtentOffset += fe.ExtentSize;
            }

            long nowMs = sw.ElapsedMilliseconds;
            if (nowMs - lastReportMs >= 150 || i == files.Count - 1)
            {
                double secs = (nowMs - lastReportMs) / 1000.0;
                string speed = secs > 0 ? $"{((bytesSinceLast / (1024.0 * 1024.0)) / secs):F1} MB/s" : "0.0 MB/s";
                lastReportMs = nowMs;
                bytesSinceLast = 0;

                double ratio = totalUncompressedBytes > 0 && writtenBlobBytes < totalUncompressedBytes
                    ? Math.Round((1.0 - ((double)writtenBlobBytes / (double)totalUncompressedBytes)) * 100.0, 1)
                    : 0.0;

                double pct = totalUncompressedBytes > 0
                    ? Math.Round((processedBytes / (double)totalUncompressedBytes) * 100.0, 1)
                    : 100.0;

                progress?.Report(new PackProgressInfo
                {
                    Status = "packing",
                    CurrentFile = fe.Name,
                    FilesPacked = i + 1,
                    TotalFiles = files.Count,
                    BytesProcessed = processedBytes,
                    TotalBytes = totalUncompressedBytes,
                    BytesWritten = writtenBlobBytes,
                    CompressionRatio = ratio,
                    SpeedFormatted = speed,
                    Percent = Math.Min(99.0, pct),
                    ElapsedSeconds = sw.Elapsed.TotalSeconds
                });
            }
        }

        ulong totalFileSize = (ulong)fs.Length;

        // 7. Build TOC payload in memory
        progress?.Report(new PackProgressInfo
        {
            Status = "finalizing",
            CurrentFile = "Generating Table of Contents...",
            Percent = 99.5,
            ElapsedSeconds = sw.Elapsed.TotalSeconds
        });

        byte[] tocBytes = new byte[tocSize];
        var tocSpan = tocBytes.AsSpan();

        // 7.1 TOC Header (48 bytes)
        tocSpan[0] = (byte)'T';
        tocSpan[1] = (byte)'O';
        tocSpan[2] = (byte)'C';
        tocSpan[3] = 1;

        BinaryPrimitives.WriteUInt32LittleEndian(tocSpan.Slice(4, 4), (uint)discoveredPackages.Count);
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(8, 8), (ulong)dirs.Count);
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(16, 8), (ulong)files.Count);

        ulong poolOffsetInToc = (ulong)(tocHeaderSize + packagesSize + dirsTableSize + filesTableSize + childrenTableSize);
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(24, 8), poolOffsetInToc);
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(32, 8), (ulong)poolSize);

        int pos = tocHeaderSize;

        // 7.2 Packages Table (64 bytes each)
        ulong currentMetaOffsetInFile = tocOffset + poolOffsetInToc + (ulong)poolSize;

        for (int p = 0; p < discoveredPackages.Count; p++)
        {
            var pkg = discoveredPackages[p];
            var pSpan = tocSpan.Slice(pos, NupConstants.PkgSize);
            pos += NupConstants.PkgSize;

            BinaryPrimitives.WriteUInt64LittleEndian(pSpan.Slice(0, 8), pkg.TitleId);
            BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(8, 4), (uint)pkg.RootDir!.Index);
            BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(12, 4), pkg.ExeFsDirIndex);
            BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(16, 4), pkg.RomFsDirIndex);
            BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(20, 4), pkg.Version);

            pSpan[24] = pkg.Kind;
            pSpan[25] = pkg.Flags;

            if (pkg.MetaBytes.Length > 0)
            {
                BinaryPrimitives.WriteUInt64LittleEndian(pSpan.Slice(32, 8), currentMetaOffsetInFile);
                BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(40, 4), (uint)pkg.MetaBytes.Length);
                currentMetaOffsetInFile += (ulong)pkg.MetaBytes.Length;
            }
            else
            {
                BinaryPrimitives.WriteUInt64LittleEndian(pSpan.Slice(32, 8), 0);
                BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(40, 4), 0);
            }
        }

        // 7.3 Directories Table
        for (int i = 0; i < dirs.Count; i++)
        {
            var dSpan = tocSpan.Slice(pos, NupConstants.DirSize);
            pos += NupConstants.DirSize;

            BinaryPrimitives.WriteUInt32LittleEndian(dSpan.Slice(0, 4), dirNameOffsets[i]);
            BinaryPrimitives.WriteUInt32LittleEndian(dSpan.Slice(4, 4), dirNameLengths[i]);
            BinaryPrimitives.WriteUInt32LittleEndian(dSpan.Slice(8, 4), dirs[i].FirstChild);
            BinaryPrimitives.WriteUInt32LittleEndian(dSpan.Slice(12, 4), dirs[i].ChildCount);
        }

        // 7.4 Files Table
        for (int i = 0; i < files.Count; i++)
        {
            var fSpan = tocSpan.Slice(pos, NupConstants.FileSize);
            pos += NupConstants.FileSize;

            BinaryPrimitives.WriteUInt32LittleEndian(fSpan.Slice(0, 4), fileNameOffsets[i]);
            BinaryPrimitives.WriteUInt32LittleEndian(fSpan.Slice(4, 4), fileNameLengths[i]);
            BinaryPrimitives.WriteUInt64LittleEndian(fSpan.Slice(8, 8), files[i].Size);
            BinaryPrimitives.WriteUInt64LittleEndian(fSpan.Slice(16, 8), files[i].ExtentOffset);
            BinaryPrimitives.WriteUInt64LittleEndian(fSpan.Slice(24, 8), files[i].ExtentSize);

            files[i].Sha256.CopyTo(fSpan.Slice(32, 32));

            fSpan[64] = files[i].Codec;
            fSpan[65] = 0; // CodecFlags
        }

        // 7.5 Children Table
        for (int i = 0; i < childrenList.Count; i++)
        {
            BinaryPrimitives.WriteUInt32LittleEndian(tocSpan.Slice(pos, 4), childrenList[i]);
            pos += 4;
        }

        // 7.6 String Pool
        stringPool.CopyTo(tocSpan.Slice(pos, poolSize));
        pos += poolSize;

        // 7.7 Metadata TLVs
        foreach (var pkg in discoveredPackages)
        {
            if (pkg.MetaBytes.Length > 0)
            {
                pkg.MetaBytes.CopyTo(tocSpan.Slice(pos, pkg.MetaBytes.Length));
                pos += pkg.MetaBytes.Length;
            }
        }

        // Calculate TOC SHA-256
        byte[] tocSha256 = SHA256.HashData(tocBytes);

        // 8. Write TOC to file at TocOffset (0x80)
        fs.Seek((long)tocOffset, SeekOrigin.Begin);
        fs.Write(tocBytes);

        // 9. Write Superblock at 0x00
        byte[] headerBytes = new byte[superblockSize];
        var hSpan = headerBytes.AsSpan();

        hSpan[0] = (byte)'N';
        hSpan[1] = (byte)'U';
        hSpan[2] = (byte)'P';
        hSpan[3] = 1;

        BinaryPrimitives.WriteUInt16LittleEndian(hSpan.Slice(4, 2), (ushort)superblockSize);
        BinaryPrimitives.WriteUInt16LittleEndian(hSpan.Slice(6, 2), 1); // HeaderVersion
        BinaryPrimitives.WriteUInt64LittleEndian(hSpan.Slice(8, 8), tocOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(hSpan.Slice(16, 8), (ulong)tocSize);

        tocSha256.CopyTo(hSpan.Slice(24, 32));

        BinaryPrimitives.WriteUInt64LittleEndian(hSpan.Slice(56, 8), blobOffset);
        BinaryPrimitives.WriteUInt64LittleEndian(hSpan.Slice(64, 8), totalFileSize);
        BinaryPrimitives.WriteUInt32LittleEndian(hSpan.Slice(72, 4), (uint)discoveredPackages.Count);
        BinaryPrimitives.WriteUInt32LittleEndian(hSpan.Slice(76, 4), 0); // Flags

        byte[] uuid = DeriveUuid(discoveredPackages);
        uuid.CopyTo(hSpan.Slice(80, 16));

        fs.Seek(0, SeekOrigin.Begin);
        fs.Write(headerBytes);
        fs.Flush();

        double finalRatio = totalUncompressedBytes > 0 && writtenBlobBytes < totalUncompressedBytes
            ? Math.Round((1.0 - ((double)writtenBlobBytes / (double)totalUncompressedBytes)) * 100.0, 1)
            : 0.0;

        sw.Stop();
        progress?.Report(new PackProgressInfo
        {
            Status = "done",
            CurrentFile = discoveredPackages.Count > 1
                ? $"Container created with {discoveredPackages.Count} packages!"
                : "Container created successfully!",
            FilesPacked = files.Count,
            TotalFiles = files.Count,
            BytesProcessed = totalUncompressedBytes,
            TotalBytes = totalUncompressedBytes,
            BytesWritten = writtenBlobBytes,
            CompressionRatio = finalRatio,
            Percent = 100.0,
            ElapsedSeconds = sw.Elapsed.TotalSeconds
        });
    }

    private static void ScanDirectory(DirEntry currentDir, List<DirEntry> dirs, List<FileEntry> files, ref ulong totalBytes, CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();

        // 1. Enumerate subdirectories
        string[] subDirs = Directory.GetDirectories(currentDir.FullPath);
        Array.Sort(subDirs, StringComparer.OrdinalIgnoreCase);

        foreach (var sd in subDirs)
        {
            var de = new DirEntry
            {
                Index = dirs.Count,
                Name = Path.GetFileName(sd),
                FullPath = sd
            };
            dirs.Add(de);
            currentDir.SubDirIndices.Add(de.Index);

            ScanDirectory(de, dirs, files, ref totalBytes, ct);
        }

        // 2. Enumerate files in this directory
        string[] localFiles = Directory.GetFiles(currentDir.FullPath);
        Array.Sort(localFiles, StringComparer.OrdinalIgnoreCase);

        foreach (var lf in localFiles)
        {
            var fi = new FileInfo(lf);
            if (string.Equals(fi.Name, ".listing", StringComparison.OrdinalIgnoreCase))
                continue;

            var fe = new FileEntry
            {
                Index = files.Count,
                Name = fi.Name,
                FullPath = lf,
                Size = (ulong)fi.Length
            };
            files.Add(fe);
            currentDir.FileIndices.Add(fe.Index);
            totalBytes += fe.Size;
        }
    }

    private static bool SubtreeHasFiles(DirEntry d, List<DirEntry> dirs)
    {
        if (d.FileIndices.Count > 0) return true;
        foreach (int subIdx in d.SubDirIndices)
        {
            if (SubtreeHasFiles(dirs[subIdx], dirs))
                return true;
        }
        return false;
    }

    private static bool DirectoryHasFiles(string dir)
    {
        try
        {
            foreach (var file in Directory.EnumerateFiles(dir, "*", SearchOption.AllDirectories))
            {
                string name = Path.GetFileName(file);
                if (!string.Equals(name, ".listing", StringComparison.OrdinalIgnoreCase))
                    return true;
            }
        }
        catch { }
        return false;
    }

    private static ulong TryParseNpdmProgramId(string packageDir)
    {
        string[] candidates = {
            Path.Combine(packageDir, "exefs", "main.npdm"),
            Path.Combine(packageDir, "main.npdm")
        };
        foreach (var cand in candidates)
        {
            if (File.Exists(cand))
            {
                try
                {
                    byte[] data = File.ReadAllBytes(cand);
                    ReadOnlySpan<byte> aci0 = "ACI0"u8;
                    int off = data.AsSpan().IndexOf(aci0);
                    if (off >= 0 && off + 0x18 <= data.Length)
                    {
                        ulong pid = BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan().Slice(off + 0x10, 8));
                        if (pid != 0) return pid;
                    }
                }
                catch { }
            }
        }
        return 0;
    }

    private static byte[]? TryReadBuildId(string packageDir)
    {
        string[] candidates = {
            Path.Combine(packageDir, "exefs", "main"),
            Path.Combine(packageDir, "main")
        };
        foreach (var cand in candidates)
        {
            if (File.Exists(cand))
            {
                try
                {
                    using var s = File.OpenRead(cand);
                    if (s.Length >= 0x60)
                    {
                        byte[] head = new byte[0x60];
                        s.ReadExactly(head);
                        if ((head[0] == 'N' && head[1] == 'S' && head[2] == 'O' && head[3] == '0') ||
                            (head[0x10] == 'N' && head[0x11] == 'R' && head[0x12] == 'O' && head[0x13] == '0'))
                        {
                            byte[] buildId = new byte[32];
                            Array.Copy(head, 0x40, buildId, 0, 32);
                            return buildId;
                        }
                    }
                }
                catch { }
            }
        }
        return null;
    }

    private static byte[] DeriveUuid(List<DiscoveredPackage> packages)
    {
        using var sha = SHA256.Create();
        byte[] buffer = new byte[13];
        foreach (var pkg in packages)
        {
            BinaryPrimitives.WriteUInt64LittleEndian(buffer.AsSpan(0, 8), pkg.TitleId);
            BinaryPrimitives.WriteUInt32LittleEndian(buffer.AsSpan(8, 4), pkg.Version);
            buffer[12] = pkg.Kind;
            sha.TransformBlock(buffer, 0, 13, null, 0);
        }
        sha.TransformFinalBlock(Array.Empty<byte>(), 0, 0);
        byte[] uuid = new byte[16];
        Array.Copy(sha.Hash!, 0, uuid, 0, 16);
        return uuid;
    }

    private static List<DiscoveredPackage> DiscoverPackages(PackOptions options)
    {
        var list = new List<DiscoveredPackage>();
        string src = Path.GetFullPath(options.SourceDirectory);

        string baseDir = Path.Combine(src, "base");
        string updatesDir = Path.Combine(src, "updates");
        string dlcDir = Path.Combine(src, "dlc");

        bool hasBase = Directory.Exists(baseDir) && DirectoryHasFiles(baseDir);
        bool hasUpdates = Directory.Exists(updatesDir) && DirectoryHasFiles(updatesDir);
        bool hasDlc = Directory.Exists(dlcDir) && DirectoryHasFiles(dlcDir);

        ulong baseTitleId = options.TitleId;

        if (hasBase)
        {
            ulong npdmId = TryParseNpdmProgramId(baseDir);
            if (npdmId != 0 && (baseTitleId == 0 || baseTitleId == 0x0100000000001000UL))
                baseTitleId = npdmId;

            if (baseTitleId == 0)
                baseTitleId = 0x0100000000001000UL;

            uint baseVer = ParseVersionString(options.VersionString);

            list.Add(new DiscoveredPackage
            {
                Kind = NupConstants.KindBase,
                SourcePath = baseDir,
                TitleId = baseTitleId,
                Version = baseVer,
                Flags = 0,
                DisplayName = options.TitleName
            });
        }

        if (hasUpdates)
        {
            byte updateFlags = hasBase ? (byte)0 : NupConstants.PkgFlagStandaloneUpdate;

            string[] subDirs = Directory.GetDirectories(updatesDir);
            Array.Sort(subDirs, StringComparer.OrdinalIgnoreCase);

            var validUpdateDirs = new List<string>();
            foreach (var d in subDirs)
            {
                if (DirectoryHasFiles(d))
                    validUpdateDirs.Add(d);
            }

            if (validUpdateDirs.Count > 0)
            {
                foreach (var updSub in validUpdateDirs)
                {
                    string folderName = Path.GetFileName(updSub);
                    uint ver = 0;
                    if (uint.TryParse(folderName, out uint parsedVer))
                    {
                        ver = parsedVer;
                    }
                    else
                    {
                        ver = ParseVersionString(folderName);
                        if (ver == 0) ver = 65536;
                    }

                    ulong updTitleId = TryParseNpdmProgramId(updSub);
                    if (updTitleId == 0)
                    {
                        if (baseTitleId != 0 && (baseTitleId & 0xFFF) == 0)
                            updTitleId = baseTitleId | 0x800;
                        else if (baseTitleId != 0)
                            updTitleId = baseTitleId;
                        else
                            updTitleId = options.TitleId;
                    }

                    list.Add(new DiscoveredPackage
                    {
                        Kind = NupConstants.KindUpdate,
                        SourcePath = updSub,
                        TitleId = updTitleId,
                        Version = ver,
                        Flags = updateFlags,
                        DisplayName = $"{options.TitleName} (Update v{folderName})"
                    });
                }
            }
            else if (DirectoryHasFiles(updatesDir))
            {
                ulong updTitleId = TryParseNpdmProgramId(updatesDir);
                if (updTitleId == 0)
                {
                    if (baseTitleId != 0 && (baseTitleId & 0xFFF) == 0)
                        updTitleId = baseTitleId | 0x800;
                    else
                        updTitleId = baseTitleId != 0 ? baseTitleId : options.TitleId;
                }

                list.Add(new DiscoveredPackage
                {
                    Kind = NupConstants.KindUpdate,
                    SourcePath = updatesDir,
                    TitleId = updTitleId,
                    Version = 65536,
                    Flags = updateFlags,
                    DisplayName = $"{options.TitleName} (Update)"
                });
            }
        }

        if (hasDlc)
        {
            string[] dlcSubs = Directory.GetDirectories(dlcDir);
            Array.Sort(dlcSubs, StringComparer.OrdinalIgnoreCase);

            int dlcCounter = 1;
            foreach (var sub in dlcSubs)
            {
                string name = Path.GetFileName(sub);
                if (name.Length == 16 && ulong.TryParse(name, System.Globalization.NumberStyles.HexNumber, null, out ulong hexId))
                {
                    string[] idxDirs = Directory.GetDirectories(sub);
                    Array.Sort(idxDirs, StringComparer.OrdinalIgnoreCase);
                    var validIdxDirs = new List<string>();
                    foreach (var idir in idxDirs)
                    {
                        if (DirectoryHasFiles(idir))
                            validIdxDirs.Add(idir);
                    }

                    if (validIdxDirs.Count > 0)
                    {
                        foreach (var idxDir in validIdxDirs)
                        {
                            list.Add(new DiscoveredPackage
                            {
                                Kind = NupConstants.KindAddon,
                                SourcePath = idxDir,
                                TitleId = hexId,
                                Version = 0,
                                Flags = 0,
                                DisplayName = $"{options.TitleName} (DLC {name})"
                            });
                        }
                    }
                    else if (DirectoryHasFiles(sub))
                    {
                        list.Add(new DiscoveredPackage
                        {
                            Kind = NupConstants.KindAddon,
                            SourcePath = sub,
                            TitleId = hexId,
                            Version = 0,
                            Flags = 0,
                            DisplayName = $"{options.TitleName} (DLC {name})"
                        });
                    }
                }
                else if (DirectoryHasFiles(sub))
                {
                    ulong dlcTitleId = baseTitleId != 0 ? (baseTitleId + (ulong)dlcCounter) : (options.TitleId + (ulong)dlcCounter);
                    list.Add(new DiscoveredPackage
                    {
                        Kind = NupConstants.KindAddon,
                        SourcePath = sub,
                        TitleId = dlcTitleId,
                        Version = 0,
                        Flags = 0,
                        DisplayName = $"{options.TitleName} (DLC {name})"
                    });
                    dlcCounter++;
                }
            }
        }

        // Fallback to single content directory
        if (list.Count == 0)
        {
            ulong npdmId = TryParseNpdmProgramId(src);
            ulong titleId = options.TitleId != 0 ? options.TitleId : (npdmId != 0 ? npdmId : 0x0100000000001000UL);

            list.Add(new DiscoveredPackage
            {
                Kind = NupConstants.KindBase,
                SourcePath = src,
                TitleId = titleId,
                Version = ParseVersionString(options.VersionString),
                Flags = 0,
                DisplayName = options.TitleName
            });
        }

        return list;
    }

    private static byte[] BuildPackageMetadataBlock(DiscoveredPackage pkg, PackOptions options)
    {
        using var ms = new MemoryStream();

        if (pkg.Kind == NupConstants.KindBase)
        {
            if (!string.IsNullOrEmpty(options.TitleName))
                WriteTlv(ms, NupConstants.TlvTitleName, Encoding.UTF8.GetBytes(options.TitleName));

            if (!string.IsNullOrEmpty(options.Publisher))
                WriteTlv(ms, NupConstants.TlvPublisher, Encoding.UTF8.GetBytes(options.Publisher));

            if (!string.IsNullOrEmpty(options.VersionString))
                WriteTlv(ms, NupConstants.TlvDisplayVersion, Encoding.UTF8.GetBytes(options.VersionString));

            if (!string.IsNullOrEmpty(options.IconPath) && File.Exists(options.IconPath))
            {
                try
                {
                    byte[] iconBytes = File.ReadAllBytes(options.IconPath);
                    WriteTlv(ms, NupConstants.TlvIcon, iconBytes);
                }
                catch { }
            }
        }
        else if (pkg.Kind == NupConstants.KindUpdate)
        {
            string updateVerStr = $"{pkg.Version >> 16}.{(pkg.Version >> 8) & 0xFF}.{pkg.Version & 0xFF}";
            WriteTlv(ms, NupConstants.TlvDisplayVersion, Encoding.UTF8.GetBytes(updateVerStr));
        }

        // TlvSourceDump = 0x0008 (provenance)
        WriteTlv(ms, NupConstants.TlvSourceDump, Encoding.UTF8.GetBytes("NUP Extractor Native"));

        // TlvNpdm = 0x0001
        string[] npdmCandidates = {
            Path.Combine(pkg.SourcePath, "exefs", "main.npdm"),
            Path.Combine(pkg.SourcePath, "main.npdm")
        };
        foreach (var nc in npdmCandidates)
        {
            if (File.Exists(nc))
            {
                try
                {
                    byte[] npdm = File.ReadAllBytes(nc);
                    WriteTlv(ms, NupConstants.TlvNpdm, npdm);
                    break;
                }
                catch { }
            }
        }

        // TlvBuildId = 0x0007
        byte[]? buildId = TryReadBuildId(pkg.SourcePath);
        if (buildId != null)
        {
            WriteTlv(ms, NupConstants.TlvBuildId, buildId);
        }

        return ms.ToArray();
    }

    private static void WriteTlv(Stream stream, ushort type, byte[] payload)
    {
        Span<byte> header = stackalloc byte[8];
        BinaryPrimitives.WriteUInt16LittleEndian(header.Slice(0, 2), type);
        BinaryPrimitives.WriteUInt16LittleEndian(header.Slice(2, 2), 0); // reserved
        BinaryPrimitives.WriteUInt32LittleEndian(header.Slice(4, 4), (uint)payload.Length);

        stream.Write(header);
        stream.Write(payload);
    }

    private static uint ParseVersionString(string ver)
    {
        try
        {
            string clean = ver.Trim().TrimStart('v', 'V');
            int dash = clean.IndexOf('-');
            if (dash >= 0) clean = clean[..dash];

            var parts = clean.Split('.');
            uint major = parts.Length > 0 && uint.TryParse(parts[0], out uint maj) ? maj : 1;
            uint minor = parts.Length > 1 && uint.TryParse(parts[1], out uint min) ? min : 0;
            uint patch = parts.Length > 2 && uint.TryParse(parts[2], out uint pat) ? pat : 0;

            return (major << 16) | (minor << 8) | patch;
        }
        catch
        {
            return 0x00010000; // 1.0.0
        }
    }

    private static ulong AlignUp(ulong value, ulong alignment)
    {
        ulong remainder = value % alignment;
        return remainder == 0 ? value : value + (alignment - remainder);
    }

    public static List<Dictionary<string, object>> DetectPackageSummary(string sourceDirectory)
    {
        var result = new List<Dictionary<string, object>>();
        try
        {
            var pkgs = DiscoverPackages(new PackOptions { SourceDirectory = sourceDirectory });
            foreach (var p in pkgs)
            {
                result.Add(new Dictionary<string, object>
                {
                    ["kind"] = (int)p.Kind,
                    ["kindName"] = NupConstants.GetKindName(p.Kind),
                    ["displayName"] = p.DisplayName,
                    ["titleIdHex"] = $"{p.TitleId:X16}",
                    ["versionStr"] = $"{p.Version >> 16}.{(p.Version >> 8) & 0xFF}.{p.Version & 0xFF}"
                });
            }
        }
        catch { }
        return result;
    }
}
