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
    public ulong TitleId { get; set; } = 0x0100000000001000UL;
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

    public static void Pack(PackOptions options, IProgress<PackProgressInfo>? progress = null, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(options.SourceDirectory) || !Directory.Exists(options.SourceDirectory))
            throw new DirectoryNotFoundException($"Source directory not found: {options.SourceDirectory}");

        if (string.IsNullOrWhiteSpace(options.OutputFilePath))
            throw new ArgumentException("Output file path cannot be empty.", nameof(options.OutputFilePath));

        string? outDir = Path.GetDirectoryName(options.OutputFilePath);
        if (!string.IsNullOrEmpty(outDir))
            Directory.CreateDirectory(outDir);

        // 1. Scan directory structure
        progress?.Report(new PackProgressInfo
        {
            Status = "scanning",
            CurrentFile = "Scanning directory hierarchy...",
            Percent = 0.0
        });

        var dirs = new List<DirEntry>();
        var files = new List<FileEntry>();

        var rootDir = new DirEntry
        {
            Index = 0,
            Name = "",
            FullPath = Path.GetFullPath(options.SourceDirectory)
        };
        dirs.Add(rootDir);

        ulong totalUncompressedBytes = 0;
        ScanDirectory(rootDir, dirs, files, ref totalUncompressedBytes, ct);

        uint exeFsDirIndex = NupConstants.NoDir;
        uint romFsDirIndex = NupConstants.NoDir;

        for (int i = 1; i < dirs.Count; i++)
        {
            if (string.Equals(dirs[i].Name, "exefs", StringComparison.OrdinalIgnoreCase))
                exeFsDirIndex = (uint)i;
            else if (string.Equals(dirs[i].Name, "romfs", StringComparison.OrdinalIgnoreCase))
                romFsDirIndex = (uint)i;
        }

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

        // 4. Build Metadata TLV block
        byte[] metaBytes = BuildMetadataBlock(options);

        // 5. Calculate TOC size and layout
        const int superblockSize = NupConstants.HeaderSize; // 0x80 = 128 bytes
        const int tocHeaderSize = 48;
        int packagesSize = 1 * NupConstants.PkgSize; // 1 package
        int dirsTableSize = dirs.Count * NupConstants.DirSize;
        int filesTableSize = files.Count * NupConstants.FileSize;
        int childrenTableSize = childrenList.Count * 4;
        int poolSize = stringPool.Length;
        int metaSize = metaBytes.Length;

        int tocSize = tocHeaderSize + packagesSize + dirsTableSize + filesTableSize + childrenTableSize + poolSize + metaSize;
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
                    Percent = Math.Min(99.0, pct)
                });
            }
        }

        ulong totalFileSize = (ulong)fs.Length;

        // 7. Build TOC payload in memory
        progress?.Report(new PackProgressInfo
        {
            Status = "finalizing",
            CurrentFile = "Generating Table of Contents...",
            Percent = 99.5
        });

        byte[] tocBytes = new byte[tocSize];
        var tocSpan = tocBytes.AsSpan();

        // 7.1 TOC Header (48 bytes)
        tocSpan[0] = (byte)'T';
        tocSpan[1] = (byte)'O';
        tocSpan[2] = (byte)'C';
        tocSpan[3] = 1;

        BinaryPrimitives.WriteUInt32LittleEndian(tocSpan.Slice(4, 4), 1); // 1 package
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(8, 8), (ulong)dirs.Count);
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(16, 8), (ulong)files.Count);

        ulong poolOffsetInToc = (ulong)(tocHeaderSize + packagesSize + dirsTableSize + filesTableSize + childrenTableSize);
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(24, 8), poolOffsetInToc);
        BinaryPrimitives.WriteUInt64LittleEndian(tocSpan.Slice(32, 8), (ulong)poolSize);

        int pos = tocHeaderSize;

        // 7.2 Package Entry (64 bytes)
        var pSpan = tocSpan.Slice(pos, NupConstants.PkgSize);
        pos += NupConstants.PkgSize;

        BinaryPrimitives.WriteUInt64LittleEndian(pSpan.Slice(0, 8), options.TitleId);
        BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(8, 4), 0); // RootDir
        BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(12, 4), exeFsDirIndex);
        BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(16, 4), romFsDirIndex);

        uint versionInt = ParseVersionString(options.VersionString);
        BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(20, 4), versionInt);

        pSpan[24] = NupConstants.KindBase;
        pSpan[25] = 0; // Flags

        ulong metaOffsetInFile = tocOffset + poolOffsetInToc + (ulong)poolSize;
        BinaryPrimitives.WriteUInt64LittleEndian(pSpan.Slice(32, 8), metaOffsetInFile);
        BinaryPrimitives.WriteUInt32LittleEndian(pSpan.Slice(40, 4), (uint)metaSize);

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
        metaBytes.CopyTo(tocSpan.Slice(pos, metaSize));
        pos += metaSize;

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
        BinaryPrimitives.WriteUInt32LittleEndian(hSpan.Slice(72, 4), 1); // 1 package
        BinaryPrimitives.WriteUInt32LittleEndian(hSpan.Slice(76, 4), 0); // Flags

        byte[] uuid = Guid.NewGuid().ToByteArray();
        uuid.CopyTo(hSpan.Slice(80, 16));

        fs.Seek(0, SeekOrigin.Begin);
        fs.Write(headerBytes);
        fs.Flush();

        double finalRatio = totalUncompressedBytes > 0 && writtenBlobBytes < totalUncompressedBytes
            ? Math.Round((1.0 - ((double)writtenBlobBytes / (double)totalUncompressedBytes)) * 100.0, 1)
            : 0.0;

        progress?.Report(new PackProgressInfo
        {
            Status = "done",
            CurrentFile = "Container created successfully!",
            FilesPacked = files.Count,
            TotalFiles = files.Count,
            BytesProcessed = totalUncompressedBytes,
            TotalBytes = totalUncompressedBytes,
            BytesWritten = writtenBlobBytes,
            CompressionRatio = finalRatio,
            Percent = 100.0
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

    private static byte[] BuildMetadataBlock(PackOptions options)
    {
        using var ms = new MemoryStream();

        // TlvTitleName = 0x0005
        WriteTlv(ms, NupConstants.TlvTitleName, Encoding.UTF8.GetBytes(options.TitleName));

        // TlvPublisher = 0x0006
        WriteTlv(ms, NupConstants.TlvPublisher, Encoding.UTF8.GetBytes(options.Publisher));

        // TlvDisplayVersion = 0x0004
        WriteTlv(ms, NupConstants.TlvDisplayVersion, Encoding.UTF8.GetBytes(options.VersionString));

        // TlvIcon = 0x0003
        if (!string.IsNullOrEmpty(options.IconPath) && File.Exists(options.IconPath))
        {
            try
            {
                byte[] iconBytes = File.ReadAllBytes(options.IconPath);
                WriteTlv(ms, NupConstants.TlvIcon, iconBytes);
            }
            catch { }
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
}
