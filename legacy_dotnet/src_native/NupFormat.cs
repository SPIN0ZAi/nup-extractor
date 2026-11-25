using System;
using System.Collections.Generic;

namespace NupExtractorApp;

public static class NupConstants
{
    public static readonly byte[] Magic = new byte[] { (byte)'N', (byte)'U', (byte)'P', 1 };
    public static readonly byte[] TocMagic = new byte[] { (byte)'T', (byte)'O', (byte)'C', 1 };
    public const int HeaderSize = 0x80;
    public const int PkgSize = 0x40;
    public const int DirSize = 0x18;
    public const int FileSize = 0x50;
    public const int ExtentAlign = 16;
    public const uint NoDir = 0xFFFFFFFF;

    public const byte CodecRaw = 0;
    public const byte CodecZstd = 1;

    public const byte KindBase = 0;
    public const byte KindUpdate = 1;
    public const byte KindAddon = 2;
    public const byte KindAddonUpdate = 3;
    public const byte KindSystem = 4;

    public const ushort TlvNpdm = 0x0001;
    public const ushort TlvNacp = 0x0002;
    public const ushort TlvIcon = 0x0003;
    public const ushort TlvDisplayVersion = 0x0004;
    public const ushort TlvTitleName = 0x0005;
    public const ushort TlvPublisher = 0x0006;
    public const ushort TlvBuildId = 0x0007;
    public const ushort TlvSourceDump = 0x0008;
    public const ushort TlvLocalizedTitles = 0x0009;
    public const ushort TlvLocalizedIcons = 0x000A;

    public const byte PkgFlagStandaloneUpdate = 0x01;

    public static string GetKindName(byte kind) => kind switch
    {
        KindBase => "base",
        KindUpdate => "update",
        KindAddon => "addon",
        KindAddonUpdate => "addon_update",
        KindSystem => "system",
        _ => $"kind{kind}"
    };
}

public class NupSuperblock
{
    public ushort HeaderSize { get; set; }
    public ushort HeaderVersion { get; set; }
    public ulong TocOffset { get; set; }
    public ulong TocSize { get; set; }
    public byte[] TocSha256 { get; set; } = new byte[32];
    public ulong BlobOffset { get; set; }
    public ulong FileSize { get; set; }
    public uint PackageCount { get; set; }
    public uint Flags { get; set; }
    public byte[] Uuid { get; set; } = new byte[16];

    public string FormattedUuid
    {
        get
        {
            var hex = Convert.ToHexString(Uuid).ToLowerInvariant();
            if (hex.Length >= 32)
                return $"{hex[..8]}-{hex.Substring(8, 4)}-{hex.Substring(12, 4)}-{hex.Substring(16, 4)}-{hex[20..]}";
            return hex;
        }
    }
}

public class NupPackage
{
    public int Index { get; set; }
    public ulong TitleId { get; set; }
    public uint RootDir { get; set; }
    public uint ExeFsDir { get; set; }
    public uint RomFsDir { get; set; }
    public uint Version { get; set; }
    public byte Kind { get; set; }
    public byte Flags { get; set; }
    public ulong MetaOffset { get; set; }
    public uint MetaSize { get; set; }

    public string KindName => NupConstants.GetKindName(Kind);
    public string TitleIdHex => $"{TitleId:X16}";
    public string VersionStr => $"{Version >> 16}.{(Version >> 8) & 0xFF}.{Version & 0xFF}";
}

public class NupDirNode
{
    public string Name { get; set; } = "";
    public uint FirstChild { get; set; }
    public uint ChildCount { get; set; }
}

public class NupFileNode
{
    public int Index { get; set; }
    public string Name { get; set; } = "";
    public ulong Size { get; set; }
    public ulong ExtentOffset { get; set; }
    public ulong ExtentSize { get; set; }
    public byte[] Sha256 { get; set; } = new byte[32];
    public byte Codec { get; set; }
    public byte CodecFlags { get; set; }
}

