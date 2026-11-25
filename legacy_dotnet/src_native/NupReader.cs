using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.IO;
using System.IO.MemoryMappedFiles;
using System.Security.Cryptography;
using System.Text;
using ZstdSharp;

namespace NupExtractorApp;

public unsafe class NupReader : IDisposable
{
    private readonly FileStream _fileStream;
    private readonly MemoryMappedFile _mmf;
    private readonly MemoryMappedViewAccessor _accessor;
    private readonly byte* _ptr = null;
    private readonly long _fileSize;
    private bool _disposed = false;

    public NupSuperblock Super { get; private set; } = new();
    public List<NupPackage> Packages { get; private set; } = new();
    public List<NupDirNode> Dirs { get; private set; } = new();
    public List<NupFileNode> Files { get; private set; } = new();
    public List<uint> Children { get; private set; } = new();
    public byte[] StringPool { get; private set; } = Array.Empty<byte>();

    public string FilePath { get; }

    public unsafe NupReader(string path)
    {
        FilePath = Path.GetFullPath(path);
        _fileStream = new FileStream(FilePath, FileMode.Open, FileAccess.Read, FileShare.Read);
        _fileSize = _fileStream.Length;

        _mmf = MemoryMappedFile.CreateFromFile(_fileStream, null, 0, MemoryMappedFileAccess.Read, HandleInheritability.None, false);
        _accessor = _mmf.CreateViewAccessor(0, 0, MemoryMappedFileAccess.Read);
        byte* p = null;
        _accessor.SafeMemoryMappedViewHandle.AcquirePointer(ref p);
        _ptr = p;

        try
        {
            ParseSuperblock();
            ParseToc();
        }
        catch
        {
            Dispose();
            throw;
        }
    }

    private unsafe void ParseSuperblock()
    {
        if (_fileSize < NupConstants.HeaderSize)
            throw new InvalidDataException($"File too small for NUP header: {_fileSize} bytes");

        var magic = new ReadOnlySpan<byte>(_ptr, 4);
        if (magic[0] != 'N' || magic[1] != 'U' || magic[2] != 'P' || magic[3] != 1)
            throw new InvalidDataException("Invalid NUP magic bytes");

        Super.HeaderSize = BinaryPrimitives.ReadUInt16LittleEndian(new ReadOnlySpan<byte>(_ptr + 4, 2));
        Super.HeaderVersion = BinaryPrimitives.ReadUInt16LittleEndian(new ReadOnlySpan<byte>(_ptr + 6, 2));
        Super.TocOffset = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(_ptr + 8, 8));
        Super.TocSize = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(_ptr + 16, 8));

        new ReadOnlySpan<byte>(_ptr + 24, 32).CopyTo(Super.TocSha256);

        Super.BlobOffset = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(_ptr + 56, 8));
        Super.FileSize = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(_ptr + 64, 8));
        Super.PackageCount = BinaryPrimitives.ReadUInt32LittleEndian(new ReadOnlySpan<byte>(_ptr + 72, 4));
        Super.Flags = BinaryPrimitives.ReadUInt32LittleEndian(new ReadOnlySpan<byte>(_ptr + 76, 4));

        new ReadOnlySpan<byte>(_ptr + 80, 16).CopyTo(Super.Uuid);

        if (Super.FileSize != (ulong)_fileSize)
            throw new InvalidDataException($"NUP file size mismatch: header {Super.FileSize} != actual {_fileSize}");

        if (Super.TocOffset < (ulong)NupConstants.HeaderSize)
            throw new InvalidDataException("TOC offset is before header end");

        if (Super.BlobOffset < Super.TocOffset + Super.TocSize)
            throw new InvalidDataException("Blob offset overlaps TOC");

        if (Super.BlobOffset % NupConstants.ExtentAlign != 0)
            throw new InvalidDataException("Blob offset is not 16-byte aligned");

        // Validate TOC SHA-256
        var tocSpan = new ReadOnlySpan<byte>(_ptr + (long)Super.TocOffset, (int)Super.TocSize);
        byte[] computedHash = SHA256.HashData(tocSpan);
        if (!computedHash.AsSpan().SequenceEqual(Super.TocSha256))
            throw new InvalidDataException("TOC SHA-256 mismatch: container is corrupt or modified");
    }

    private unsafe void ParseToc()
    {
        byte* toc = _ptr + (long)Super.TocOffset;
        int tocLen = (int)Super.TocSize;

        if (tocLen < 48)
            throw new InvalidDataException("TOC header too short");

        var tocMagic = new ReadOnlySpan<byte>(toc, 4);
        if (tocMagic[0] != 'T' || tocMagic[1] != 'O' || tocMagic[2] != 'C' || tocMagic[3] != 1)
            throw new InvalidDataException("Invalid TOC magic");

        uint pkgCount = BinaryPrimitives.ReadUInt32LittleEndian(new ReadOnlySpan<byte>(toc + 4, 4));
        ulong dirCount = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(toc + 8, 8));
        ulong fileCount = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(toc + 16, 8));
        ulong poolOff = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(toc + 24, 8));
        ulong poolSize = BinaryPrimitives.ReadUInt64LittleEndian(new ReadOnlySpan<byte>(toc + 32, 8));

        if (pkgCount != Super.PackageCount)
            throw new InvalidDataException("Package count mismatch between Superblock and TOC");

        if (poolOff + poolSize > (ulong)tocLen)
            throw new InvalidDataException("String pool out of TOC bounds");

        StringPool = new byte[poolSize];
        new ReadOnlySpan<byte>(toc + (long)poolOff, (int)poolSize).CopyTo(StringPool);

        int pos = 48; // after TOC header

        // Parse Packages
        for (int i = 0; i < (int)pkgCount; i++)
        {
            var pSpan = new ReadOnlySpan<byte>(toc + pos, NupConstants.PkgSize);
            pos += NupConstants.PkgSize;

            var pkg = new NupPackage
            {
                Index = i,
                TitleId = BinaryPrimitives.ReadUInt64LittleEndian(pSpan.Slice(0, 8)),
                RootDir = BinaryPrimitives.ReadUInt32LittleEndian(pSpan.Slice(8, 4)),
                ExeFsDir = BinaryPrimitives.ReadUInt32LittleEndian(pSpan.Slice(12, 4)),
                RomFsDir = BinaryPrimitives.ReadUInt32LittleEndian(pSpan.Slice(16, 4)),
                Version = BinaryPrimitives.ReadUInt32LittleEndian(pSpan.Slice(20, 4)),
                Kind = pSpan[24],
                Flags = pSpan[25],
                MetaOffset = BinaryPrimitives.ReadUInt64LittleEndian(pSpan.Slice(32, 8)),
                MetaSize = BinaryPrimitives.ReadUInt32LittleEndian(pSpan.Slice(40, 4))
            };
            Packages.Add(pkg);
        }

        // Parse Dirs
        for (int i = 0; i < (int)dirCount; i++)
        {
            var dSpan = new ReadOnlySpan<byte>(toc + pos, NupConstants.DirSize);
            pos += NupConstants.DirSize;

            uint nameOff = BinaryPrimitives.ReadUInt32LittleEndian(dSpan.Slice(0, 4));
            uint nameLen = BinaryPrimitives.ReadUInt32LittleEndian(dSpan.Slice(4, 4));
            uint firstChild = BinaryPrimitives.ReadUInt32LittleEndian(dSpan.Slice(8, 4));
            uint childCount = BinaryPrimitives.ReadUInt32LittleEndian(dSpan.Slice(12, 4));

            string name = GetPoolString(nameOff, nameLen);
            Dirs.Add(new NupDirNode
            {
                Name = name,
                FirstChild = firstChild,
                ChildCount = childCount
            });
        }

        // Parse Files
        for (int i = 0; i < (int)fileCount; i++)
        {
            var fSpan = new ReadOnlySpan<byte>(toc + pos, NupConstants.FileSize);
            pos += NupConstants.FileSize;

            uint nameOff = BinaryPrimitives.ReadUInt32LittleEndian(fSpan.Slice(0, 4));
            uint nameLen = BinaryPrimitives.ReadUInt32LittleEndian(fSpan.Slice(4, 4));
            ulong size = BinaryPrimitives.ReadUInt64LittleEndian(fSpan.Slice(8, 8));
            ulong eoff = BinaryPrimitives.ReadUInt64LittleEndian(fSpan.Slice(16, 8));
            ulong esz = BinaryPrimitives.ReadUInt64LittleEndian(fSpan.Slice(24, 8));

            byte[] sha = new byte[32];
            fSpan.Slice(32, 32).CopyTo(sha);

            byte codec = fSpan[64];
            byte cflags = fSpan[65];

            string name = GetPoolString(nameOff, nameLen);
            Files.Add(new NupFileNode
            {
                Index = i,
                Name = name,
                Size = size,
                ExtentOffset = eoff,
                ExtentSize = esz,
                Sha256 = sha,
                Codec = codec,
                CodecFlags = cflags
            });
        }

        // Parse Children
        int childBytes = (int)poolOff - pos;
        int childCountTotal = childBytes / 4;
        Children = new List<uint>(childCountTotal);
        for (int i = 0; i < childCountTotal; i++)
        {
            uint cid = BinaryPrimitives.ReadUInt32LittleEndian(new ReadOnlySpan<byte>(toc + pos, 4));
            pos += 4;
            Children.Add(cid);
        }
    }

    private string GetPoolString(uint off, uint len)
    {
        if (off + len > (uint)StringPool.Length)
            return "";
        return Encoding.UTF8.GetString(StringPool, (int)off, (int)len);
    }

    public List<(bool isDir, int index, string name)> GetChildren(uint dirIndex)
    {
        var result = new List<(bool isDir, int index, string name)>();
        if (dirIndex >= (uint)Dirs.Count) return result;

        var d = Dirs[(int)dirIndex];
        int start = (int)d.FirstChild;
        int end = start + (int)d.ChildCount;

        for (int i = start; i < end && i < Children.Count; i++)
        {
            uint cid = Children[i];
            if (cid < (uint)Dirs.Count)
            {
                result.Add((true, (int)cid, Dirs[(int)cid].Name));
            }
            else
            {
                int fi = (int)(cid - (uint)Dirs.Count);
                if (fi < Files.Count)
                    result.Add((false, fi, Files[fi].Name));
            }
        }
        return result;
    }

    public unsafe Dictionary<ushort, byte[]> GetPackageMetadata(NupPackage pkg)
    {
        var meta = new Dictionary<ushort, byte[]>();
        if (pkg.MetaSize == 0 || pkg.MetaOffset + pkg.MetaSize > (ulong)_fileSize)
            return meta;

        byte* block = _ptr + (long)pkg.MetaOffset;
        int pos = 0;
        int max = (int)pkg.MetaSize;

        while (pos + 8 <= max)
        {
            ushort type = BinaryPrimitives.ReadUInt16LittleEndian(new ReadOnlySpan<byte>(block + pos, 2));
            uint size = BinaryPrimitives.ReadUInt32LittleEndian(new ReadOnlySpan<byte>(block + pos + 4, 4));
            pos += 8;

            if (pos + size > max) break;

            byte[] payload = new byte[size];
            new ReadOnlySpan<byte>(block + pos, (int)size).CopyTo(payload);
            meta[type] = payload;
            pos += (int)size;
        }

        return meta;
    }

    public unsafe Stream OpenReadFileStream(int fileIndex)
    {
        if (fileIndex < 0 || fileIndex >= Files.Count)
            throw new ArgumentOutOfRangeException(nameof(fileIndex));

        var f = Files[fileIndex];
        if (f.ExtentOffset + f.ExtentSize > (ulong)_fileSize)
            throw new InvalidDataException($"Extent out of bounds for file {f.Name}");

        var memStream = new UnmanagedMemoryStream(_ptr + (long)f.ExtentOffset, (long)f.ExtentSize, (long)f.ExtentSize, FileAccess.Read);

        if (f.Codec == NupConstants.CodecRaw)
        {
            return memStream;
        }
        else if (f.Codec == NupConstants.CodecZstd)
        {
            return new DecompressionStream(memStream);
        }

        memStream.Dispose();
        throw new NotSupportedException($"Unknown codec: {f.Codec}");
    }

    public unsafe byte[] ReadFileData(int fileIndex)
    {
        if (fileIndex < 0 || fileIndex >= Files.Count)
            throw new ArgumentOutOfRangeException(nameof(fileIndex));

        var f = Files[fileIndex];
        if (f.Size == 0) return Array.Empty<byte>();

        if (f.Size > 0x7FFFFFC7) // Array.MaxLength (~2GB)
            throw new InvalidOperationException($"File '{f.Name}' is {f.Size} bytes and exceeds managed byte array size limits (>2GB). Use OpenReadFileStream or ExtractFileToStream instead.");

        using var srcStream = OpenReadFileStream(fileIndex);
        byte[] output = new byte[f.Size];
        int totalRead = 0;
        while (totalRead < (int)f.Size)
        {
            int read = srcStream.Read(output, totalRead, (int)f.Size - totalRead);
            if (read == 0) break;
            totalRead += read;
        }

        if ((ulong)totalRead != f.Size)
            throw new InvalidDataException($"Size mismatch for {f.Name}: read {totalRead} bytes, expected {f.Size}");

        return output;
    }

    public unsafe void ExtractFileToStream(int fileIndex, Stream destStream)
    {
        if (fileIndex < 0 || fileIndex >= Files.Count)
            throw new ArgumentOutOfRangeException(nameof(fileIndex));

        var f = Files[fileIndex];
        if (f.Size == 0) return;

        if (f.ExtentOffset + f.ExtentSize > (ulong)_fileSize)
            throw new InvalidDataException($"Extent out of bounds for file {f.Name}");

        using var memStream = new UnmanagedMemoryStream(_ptr + (long)f.ExtentOffset, (long)f.ExtentSize, (long)f.ExtentSize, FileAccess.Read);

        if (f.Codec == NupConstants.CodecRaw)
        {
            memStream.CopyTo(destStream, 131072);
        }
        else if (f.Codec == NupConstants.CodecZstd)
        {
            using var zstdStream = new DecompressionStream(memStream);
            zstdStream.CopyTo(destStream, 131072);
        }
        else
        {
            throw new NotSupportedException($"Unknown codec: {f.Codec}");
        }
    }

    public void Dispose()
    {
        if (!_disposed)
        {
            _disposed = true;
            _accessor.SafeMemoryMappedViewHandle.ReleasePointer();
            _accessor.Dispose();
            _mmf.Dispose();
            _fileStream.Dispose();
        }
    }
}
