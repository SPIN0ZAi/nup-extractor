using System;
using System.IO;
using System.Text.Json;

namespace NupExtractorApp.Orb;

public sealed class OrbSettings
{
    public bool HasSavedPosition { get; set; }
    public int OrbCenterX { get; set; }
    public int OrbCenterY { get; set; }
    public double AnchorFractionX { get; set; } = 0.96;
    public double AnchorFractionY { get; set; } = 0.38;
    public ScreenEdge OrbEdge { get; set; } = ScreenEdge.Right;
    public string MonitorDeviceName { get; set; } = "";
    public string LastMode { get; set; } = "Collapsed"; // "Collapsed" | "Expanded"
    public OrbLaunchMode LaunchMode { get; set; } = OrbLaunchMode.Collapsed;
    public double ExpandedWidthDip { get; set; } = OrbGeometry.DefaultExpandedWidthDip;
    public double ExpandedHeightDip { get; set; } = OrbGeometry.DefaultExpandedHeightDip;
    public string Theme { get; set; } = "dark"; // "dark" | "light"

    public OrbWindowState ResolveStartupState(bool hasInitialFileArg)
    {
        if (hasInitialFileArg)
            return OrbWindowState.Expanded;

        return LaunchMode switch
        {
            OrbLaunchMode.Expanded => OrbWindowState.Expanded,
            OrbLaunchMode.RememberLast => string.Equals(LastMode, "Expanded", StringComparison.OrdinalIgnoreCase)
                ? OrbWindowState.Expanded
                : OrbWindowState.Collapsed,
            _ => OrbWindowState.Collapsed
        };
    }
}

public static class OrbSettingsStore
{
    private static readonly JsonSerializerOptions SerializerOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
        WriteIndented = true
    };

    public static string GetDefaultPath()
    {
        string appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        if (string.IsNullOrWhiteSpace(appData))
            appData = AppDomain.CurrentDomain.BaseDirectory;
        return Path.Combine(appData, "NupExtractor", "orb-state.json");
    }

    public static OrbSettings Load(string? filePath = null)
    {
        string path = string.IsNullOrWhiteSpace(filePath) ? GetDefaultPath() : filePath;
        try
        {
            if (!File.Exists(path))
                return new OrbSettings();

            string json = File.ReadAllText(path);
            var loaded = JsonSerializer.Deserialize<OrbSettings>(json, SerializerOptions);
            if (loaded == null)
                return new OrbSettings();

            // Defensive clamping of restored dimensions
            if (loaded.ExpandedWidthDip < OrbGeometry.MinExpandedWidthDip || loaded.ExpandedWidthDip > 6000)
                loaded.ExpandedWidthDip = OrbGeometry.DefaultExpandedWidthDip;
            if (loaded.ExpandedHeightDip < OrbGeometry.MinExpandedHeightDip || loaded.ExpandedHeightDip > 4000)
                loaded.ExpandedHeightDip = OrbGeometry.DefaultExpandedHeightDip;
            if (!string.Equals(loaded.Theme, "light", StringComparison.OrdinalIgnoreCase))
                loaded.Theme = "dark";
            else
                loaded.Theme = "light";

            return loaded;
        }
        catch
        {
            return new OrbSettings();
        }
    }

    public static bool Save(OrbSettings settings, string? filePath = null)
    {
        if (settings == null) return false;
        string path = string.IsNullOrWhiteSpace(filePath) ? GetDefaultPath() : filePath;
        try
        {
            string? dir = Path.GetDirectoryName(path);
            if (!string.IsNullOrEmpty(dir))
                Directory.CreateDirectory(dir);

            string json = JsonSerializer.Serialize(settings, SerializerOptions);
            string tmpPath = path + ".tmp";
            File.WriteAllText(tmpPath, json);
            File.Move(tmpPath, path, overwrite: true);
            return true;
        }
        catch
        {
            return false;
        }
    }
}
