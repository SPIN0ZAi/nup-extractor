using System;
using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Reflection;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace NupExtractorApp;

public class UpdateCheckResult
{
    public bool UpdateAvailable { get; set; }
    public string CurrentVersion { get; set; } = "";
    public string LatestVersion { get; set; } = "";
    public string ReleaseName { get; set; } = "";
    public string ReleaseNotes { get; set; } = "";
    public string ReleaseUrl { get; set; } = "";
    public string DownloadUrl { get; set; } = "";
    public long DownloadSize { get; set; }
    public string? Error { get; set; }
}

public class UpdateProgressInfo
{
    public string Stage { get; set; } = "downloading"; // "downloading", "extracting", "applying", "ready", "error"
    public double Percent { get; set; }
    public long BytesDownloaded { get; set; }
    public long TotalBytes { get; set; }
    public string SpeedFormatted { get; set; } = "";
    public string Message { get; set; } = "";
    public string? Error { get; set; }
}

public static class AppUpdateManager
{
    public static string CurrentVersion { get; } = ResolveVersion();
    public const string GitHubRepo = "hatimbenayad/NupUpgrade";
    private static readonly HttpClient _httpClient = new HttpClient();

    private static string ResolveVersion()
    {
        try
        {
            var asm = typeof(AppUpdateManager).Assembly;
            var infoVer = asm.GetCustomAttribute<System.Reflection.AssemblyInformationalVersionAttribute>()?.InformationalVersion;
            if (!string.IsNullOrWhiteSpace(infoVer))
            {
                int plusIdx = infoVer.IndexOf('+');
                if (plusIdx > 0) infoVer = infoVer[..plusIdx];
                string clean = infoVer.Trim().TrimStart('v', 'V');
                if (!string.IsNullOrEmpty(clean) && clean != "1.0.0")
                    return clean;
            }

            var ver = asm.GetName().Version;
            if (ver != null && ver.Major > 0 && !(ver.Major == 1 && ver.Minor == 0 && ver.Build == 0))
                return $"{ver.Major}.{ver.Minor}.{ver.Build}";
        }
        catch { }

        return "1.0.8";
    }

    static AppUpdateManager()
    {
        _httpClient.DefaultRequestHeaders.UserAgent.Add(new ProductInfoHeaderValue("NUP-Extractor", CurrentVersion));
        _httpClient.DefaultRequestHeaders.Accept.Add(new MediaTypeWithQualityHeaderValue("application/vnd.github.v3+json"));
        _httpClient.Timeout = TimeSpan.FromSeconds(30);
    }

    public static Version ParseVersion(string tag)
    {
        try
        {
            string clean = tag.Trim().TrimStart('v', 'V');
            int dashIdx = clean.IndexOf('-');
            if (dashIdx >= 0) clean = clean[..dashIdx];

            var parts = clean.Split('.');
            int major = parts.Length > 0 && int.TryParse(parts[0], out int maj) ? maj : 1;
            int minor = parts.Length > 1 && int.TryParse(parts[1], out int min) ? min : 0;
            int patch = parts.Length > 2 && int.TryParse(parts[2], out int pat) ? pat : 0;
            return new Version(major, minor, patch);
        }
        catch
        {
            return new Version(1, 0, 0);
        }
    }

    public static async Task<UpdateCheckResult> CheckForUpdatesAsync(CancellationToken ct = default)
    {
        var result = new UpdateCheckResult
        {
            CurrentVersion = CurrentVersion,
            LatestVersion = CurrentVersion
        };

        try
        {
            string apiUrl = $"https://api.github.com/repos/{GitHubRepo}/releases/latest";
            using var response = await _httpClient.GetAsync(apiUrl, ct);

            if (!response.IsSuccessStatusCode)
            {
                result.Error = $"GitHub API returned {response.StatusCode}";
                return result;
            }

            string json = await response.Content.ReadAsStringAsync(ct);
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;

            string tagName = root.TryGetProperty("tag_name", out var tp) ? tp.GetString() ?? "" : "";
            string releaseName = root.TryGetProperty("name", out var np) ? np.GetString() ?? "" : tagName;
            string releaseNotes = root.TryGetProperty("body", out var bp) ? bp.GetString() ?? "" : "";
            string htmlUrl = root.TryGetProperty("html_url", out var hp) ? hp.GetString() ?? "" : "";

            string downloadUrl = "";
            long downloadSize = 0;

            if (root.TryGetProperty("assets", out var assets) && assets.ValueKind == JsonValueKind.Array)
            {
                foreach (var asset in assets.EnumerateArray())
                {
                    string aName = asset.TryGetProperty("name", out var an) ? an.GetString() ?? "" : "";
                    if (aName.EndsWith(".zip", StringComparison.OrdinalIgnoreCase) &&
                        (aName.Contains("NUP_Extractor", StringComparison.OrdinalIgnoreCase) || aName.Contains("windows", StringComparison.OrdinalIgnoreCase)))
                    {
                        downloadUrl = asset.TryGetProperty("browser_download_url", out var bdu) ? bdu.GetString() ?? "" : "";
                        downloadSize = asset.TryGetProperty("size", out var sp) ? sp.GetInt64() : 0;
                        break;
                    }
                }

                // Fallback to any zip asset if specific name pattern not found
                if (string.IsNullOrEmpty(downloadUrl))
                {
                    foreach (var asset in assets.EnumerateArray())
                    {
                        string aName = asset.TryGetProperty("name", out var an) ? an.GetString() ?? "" : "";
                        if (aName.EndsWith(".zip", StringComparison.OrdinalIgnoreCase))
                        {
                            downloadUrl = asset.TryGetProperty("browser_download_url", out var bdu) ? bdu.GetString() ?? "" : "";
                            downloadSize = asset.TryGetProperty("size", out var sp) ? sp.GetInt64() : 0;
                            break;
                        }
                    }
                }
            }

            var currentVer = ParseVersion(CurrentVersion);
            var latestVer = ParseVersion(tagName);

            result.LatestVersion = tagName.TrimStart('v', 'V');
            result.ReleaseName = releaseName;
            result.ReleaseNotes = releaseNotes;
            result.ReleaseUrl = htmlUrl;
            result.DownloadUrl = downloadUrl;
            result.DownloadSize = downloadSize;
            result.UpdateAvailable = latestVer > currentVer && !string.IsNullOrEmpty(downloadUrl);

            return result;
        }
        catch (Exception ex)
        {
            result.Error = ex.Message;
            return result;
        }
    }

    public static async Task ApplyUpdateAsync(string downloadUrl, IProgress<UpdateProgressInfo> progress, CancellationToken ct)
    {
        string currentExePath = Environment.ProcessPath ?? Process.GetCurrentProcess().MainModule?.FileName ?? "";
        if (string.IsNullOrEmpty(currentExePath) || !File.Exists(currentExePath))
            throw new InvalidOperationException("Could not determine current executable path for update.");

        string tempBase = Path.Combine(Path.GetTempPath(), "NupExtractor_Update_" + Guid.NewGuid().ToString("N")[..8]);
        Directory.CreateDirectory(tempBase);
        string zipPath = Path.Combine(tempBase, "update.zip");
        string extractDir = Path.Combine(tempBase, "extracted");

        try
        {
            // 1. Download
            progress.Report(new UpdateProgressInfo
            {
                Stage = "downloading",
                Percent = 0.0,
                Message = "Connecting to GitHub Releases server..."
            });

            using (var response = await _httpClient.GetAsync(downloadUrl, HttpCompletionOption.ResponseHeadersRead, ct))
            {
                response.EnsureSuccessStatusCode();
                long totalBytes = response.Content.Headers.ContentLength ?? -1;

                using var contentStream = await response.Content.ReadAsStreamAsync(ct);
                using var fs = new FileStream(zipPath, FileMode.Create, FileAccess.Write, FileShare.None, 131072);

                byte[] buffer = new byte[65536];
                long totalDownloaded = 0;
                var sw = Stopwatch.StartNew();
                long lastReport = 0;
                long bytesSinceLast = 0;
                double speed = 0.0;

                while (true)
                {
                    ct.ThrowIfCancellationRequested();
                    int read = await contentStream.ReadAsync(buffer, 0, buffer.Length, ct);
                    if (read == 0) break;

                    await fs.WriteAsync(buffer, 0, read, ct);
                    totalDownloaded += read;
                    bytesSinceLast += read;

                    long now = sw.ElapsedMilliseconds;
                    if (now - lastReport >= 100)
                    {
                        double secs = (now - lastReport) / 1000.0;
                        if (secs > 0) speed = (bytesSinceLast / (1024.0 * 1024.0)) / secs;
                        lastReport = now;
                        bytesSinceLast = 0;

                        double pct = totalBytes > 0 ? Math.Round((totalDownloaded / (double)totalBytes) * 100.0, 1) : 0.0;
                        progress.Report(new UpdateProgressInfo
                        {
                            Stage = "downloading",
                            Percent = pct,
                            BytesDownloaded = totalDownloaded,
                            TotalBytes = totalBytes > 0 ? totalBytes : totalDownloaded,
                            SpeedFormatted = $"{speed:F1} MB/s",
                            Message = $"Downloading update package ({FormatBytes((ulong)totalDownloaded)} / {(totalBytes > 0 ? FormatBytes((ulong)totalBytes) : "...")})"
                        });
                    }
                }
            }

            // 2. Extract
            progress.Report(new UpdateProgressInfo
            {
                Stage = "extracting",
                Percent = 100.0,
                Message = "Extracting update package..."
            });

            Directory.CreateDirectory(extractDir);
            ZipFile.ExtractToDirectory(zipPath, extractDir, overwriteFiles: true);

            // Find new executable inside extractDir
            string[] exeFiles = Directory.GetFiles(extractDir, "*.exe", SearchOption.AllDirectories);
            if (exeFiles.Length == 0)
                throw new InvalidDataException("No executable found inside the update package.");

            string newExePath = exeFiles[0];
            foreach (var ef in exeFiles)
            {
                if (Path.GetFileName(ef).Contains("NUP", StringComparison.OrdinalIgnoreCase))
                {
                    newExePath = ef;
                    break;
                }
            }

            // 3. Launch newly extracted executable to replace the current binary in-place
            progress.Report(new UpdateProgressInfo
            {
                Stage = "ready",
                Percent = 100.0,
                Message = "Restarting application to apply update..."
            });

            int pid = Environment.ProcessId;

            var psi = new ProcessStartInfo
            {
                FileName = newExePath,
                Arguments = $"--update-replace \"{currentExePath}\" {pid}",
                UseShellExecute = true
            };

            Process.Start(psi);

            // Give the new updater process 500ms to start up before terminating this process
            await Task.Delay(500, ct);

            // Terminate current process cleanly
            Environment.Exit(0);
        }
        catch
        {
            // Clean up temp dir if download was cancelled or failed
            try { if (Directory.Exists(tempBase)) Directory.Delete(tempBase, true); } catch { }
            throw;
        }
    }

    private static string FormatBytes(ulong size)
    {
        if (size < 1024) return $"{size} B";
        if (size < 1024 * 1024) return $"{size / 1024.0:F2} KB";
        if (size < 1024 * 1024 * 1024) return $"{size / (1024.0 * 1024):F2} MB";
        return $"{size / (1024.0 * 1024 * 1024):F2} GB";
    }
}
