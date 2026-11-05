using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using Microsoft.Web.WebView2.Core;
using Microsoft.Win32;

namespace NupExtractorApp;

public partial class MainWindow : Window
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
        WriteIndented = false
    };

    private CancellationTokenSource? _extractCts;
    private CancellationTokenSource? _verifyCts;
    private CancellationTokenSource? _updateCts;
    private CancellationTokenSource? _packCts;

    public MainWindow()
    {
        InitializeComponent();
        Loaded += MainWindow_Loaded;
    }

    private async void MainWindow_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            string userDataDir = Path.Combine(Path.GetTempPath(), "NupExtractor_WebView2Data");
            var env = await CoreWebView2Environment.CreateAsync(userDataFolder: userDataDir);
            await WebView.EnsureCoreWebView2Async(env);

            WebView.CoreWebView2.Settings.IsStatusBarEnabled = false;
            WebView.CoreWebView2.Settings.AreDevToolsEnabled = true;

            // Register embedded web resource handler
            WebView.CoreWebView2.AddWebResourceRequestedFilter("https://nup.local/*", CoreWebView2WebResourceContext.All);
            WebView.CoreWebView2.WebResourceRequested += CoreWebView2_WebResourceRequested;

            // Register IPC message handler
            WebView.CoreWebView2.WebMessageReceived += CoreWebView2_WebMessageReceived;

            // Navigate to embedded app
            WebView.CoreWebView2.Navigate("https://nup.local/index.html");
        }
        catch (Exception ex)
        {
            MessageBox.Show($"Failed to initialize WebView2:\n{ex.Message}\n\nPlease ensure WebView2 Runtime is installed.",
                "Initialization Error", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private void CoreWebView2_WebResourceRequested(object? sender, CoreWebView2WebResourceRequestedEventArgs e)
    {
        var uri = new Uri(e.Request.Uri);
        string path = uri.AbsolutePath.TrimStart('/');
        if (string.IsNullOrEmpty(path)) path = "index.html";

        var stream = GetResourceStream(path);
        if (stream != null)
        {
            string mime = GetMimeType(path);
            e.Response = WebView.CoreWebView2.Environment.CreateWebResourceResponse(
                stream, 200, "OK", $"Content-Type: {mime}\r\nAccess-Control-Allow-Origin: *\r\n");
        }
        else
        {
            e.Response = WebView.CoreWebView2.Environment.CreateWebResourceResponse(null, 404, "Not Found", "");
        }
    }

    private Stream? GetResourceStream(string path)
    {
        string localPath = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "static", path);
        if (File.Exists(localPath)) return File.OpenRead(localPath);

        string curPath = Path.Combine(Directory.GetCurrentDirectory(), "static", path);
        if (File.Exists(curPath)) return File.OpenRead(curPath);

        var asm = Assembly.GetExecutingAssembly();
        string clean = path.Replace('/', '.');
        foreach (var resName in asm.GetManifestResourceNames())
        {
            if (resName.EndsWith(clean, StringComparison.OrdinalIgnoreCase))
            {
                return asm.GetManifestResourceStream(resName);
            }
        }
        return null;
    }

    private static string GetMimeType(string path)
    {
        if (path.EndsWith(".html", StringComparison.OrdinalIgnoreCase)) return "text/html; charset=utf-8";
        if (path.EndsWith(".css", StringComparison.OrdinalIgnoreCase)) return "text/css; charset=utf-8";
        if (path.EndsWith(".js", StringComparison.OrdinalIgnoreCase)) return "application/javascript; charset=utf-8";
        if (path.EndsWith(".png", StringComparison.OrdinalIgnoreCase)) return "image/png";
        if (path.EndsWith(".jpg", StringComparison.OrdinalIgnoreCase) || path.EndsWith(".jpeg", StringComparison.OrdinalIgnoreCase)) return "image/jpeg";
        if (path.EndsWith(".svg", StringComparison.OrdinalIgnoreCase)) return "image/svg+xml";
        if (path.EndsWith(".ico", StringComparison.OrdinalIgnoreCase)) return "image/x-icon";
        if (path.EndsWith(".json", StringComparison.OrdinalIgnoreCase)) return "application/json";
        return "application/octet-stream";
    }

    private async void CoreWebView2_WebMessageReceived(object? sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        try
        {
            using var doc = JsonDocument.Parse(e.WebMessageAsJson);
            var root = doc.RootElement;
            string action = GetString(root, "action") ?? "";
            string id = GetString(root, "id") ?? "";

            switch (action)
            {
                case "browse_file":
                    HandleBrowseFile(id);
                    break;

                case "browse_folder":
                    HandleBrowseFolder(id);
                    break;

                case "inspect":
                    await HandleInspectAsync(id, root);
                    break;

                case "get_tree":
                    await HandleGetTreeAsync(id, root);
                    break;

                case "search":
                    await HandleSearchAsync(id, root);
                    break;

                case "extract":
                    await HandleExtractAsync(id, root);
                    break;

                case "verify":
                    await HandleVerifyAsync(id, root);
                    break;

                case "open_folder":
                    string fPath = GetString(root, "path", "folderPath") ?? "";
                    NupEngine.OpenFolder(fPath);
                    break;

                case "get_app_info":
                    SendIpcResponse(new
                    {
                        id,
                        action = "app_info_result",
                        success = true,
                        version = AppUpdateManager.CurrentVersion,
                        initialFilePath = App.InitialFilePath
                    });
                    break;

                case "check_disk_space":
                    HandleCheckDiskSpace(id, root);
                    break;

                case "extract_single_file":
                    await HandleExtractSingleFileAsync(id, root);
                    break;

                case "get_shell_integration_status":
                    SendIpcResponse(new
                    {
                        id,
                        action = "shell_integration_status_result",
                        success = true,
                        registered = ShellIntegration.IsRegistered()
                    });
                    break;

                case "toggle_shell_integration":
                    bool enable = GetBool(root, "enable") ?? false;
                    bool regSuccess = enable ? ShellIntegration.Register() : ShellIntegration.Unregister();
                    SendIpcResponse(new
                    {
                        id,
                        action = "toggle_shell_integration_result",
                        success = regSuccess,
                        registered = ShellIntegration.IsRegistered()
                    });
                    break;

                case "check_update":
                    await HandleCheckUpdateAsync(id, root);
                    break;

                case "start_update":
                    await HandleStartUpdateAsync(id, root);
                    break;

                case "cancel_update":
                    _updateCts?.Cancel();
                    break;

                case "cancel_extract":
                    _extractCts?.Cancel();
                    break;

                case "cancel_verify":
                    _verifyCts?.Cancel();
                    break;

                case "browse_pack_folder":
                    HandleBrowsePackFolder(id);
                    break;

                case "browse_pack_output":
                    HandleBrowsePackOutput(id, root);
                    break;

                case "start_pack":
                    HandleStartPack(id, root);
                    break;

                case "cancel_pack":
                    _packCts?.Cancel();
                    break;
            }
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                type = "error",
                error = ex.Message
            });
        }
    }

    private void HandleBrowseFile(string id)
    {
        var ofd = new OpenFileDialog
        {
            Title = "Select Nintendo Switch NUP Container",
            Filter = "NUP Game Containers (*.nup)|*.nup|All files (*.*)|*.*"
        };
        bool? result = ofd.ShowDialog(this);
        SendIpcResponse(new
        {
            id,
            action = "browse_file_result",
            success = result == true,
            path = result == true ? ofd.FileName : null
        });
    }

    private void HandleBrowseFolder(string id)
    {
        var ofd = new OpenFolderDialog
        {
            Title = "Select Destination Extraction Folder",
            Multiselect = false
        };
        bool? result = ofd.ShowDialog(this);
        SendIpcResponse(new
        {
            id,
            action = "browse_folder_result",
            success = result == true,
            path = result == true ? ofd.FolderName : null
        });
    }

    private async Task HandleInspectAsync(string id, JsonElement root)
    {
        string? filePath = GetString(root, "filePath", "file_path", "path");
        if (string.IsNullOrWhiteSpace(filePath))
        {
            SendIpcResponse(new { id, action = "inspect_result", success = false, error = "File path is required" });
            return;
        }

        try
        {
            var data = await Task.Run(() => NupEngine.Inspect(filePath));
            SendIpcResponse(new
            {
                id,
                action = "inspect_result",
                success = true,
                data
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                id,
                action = "inspect_result",
                success = false,
                error = ex.Message
            });
        }
    }

    private async Task HandleGetTreeAsync(string id, JsonElement root)
    {
        string? filePath = GetString(root, "filePath", "file_path");
        uint dirIndex = (uint)(GetInt(root, "dirIndex", "dir_index") ?? 0);

        if (string.IsNullOrWhiteSpace(filePath))
        {
            SendIpcResponse(new { id, action = "get_tree_result", success = false, error = "File path is required" });
            return;
        }

        try
        {
            var data = await Task.Run(() => NupEngine.GetDirChildren(filePath, dirIndex));
            SendIpcResponse(new
            {
                id,
                action = "get_tree_result",
                success = true,
                data
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                id,
                action = "get_tree_result",
                success = false,
                error = ex.Message
            });
        }
    }

    private async Task HandleSearchAsync(string id, JsonElement root)
    {
        string? filePath = GetString(root, "filePath", "file_path");
        string query = GetString(root, "query", "q") ?? "";

        if (string.IsNullOrWhiteSpace(filePath))
        {
            SendIpcResponse(new { id, action = "search_result", success = false, error = "File path is required" });
            return;
        }

        try
        {
            var results = await Task.Run(() => NupEngine.SearchFiles(filePath, query));
            SendIpcResponse(new
            {
                id,
                action = "search_result",
                success = true,
                query,
                results
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                id,
                action = "search_result",
                success = false,
                error = ex.Message
            });
        }
    }

    private async Task HandleExtractAsync(string id, JsonElement root)
    {
        string? filePath = GetString(root, "filePath", "file_path");
        string? destDir = GetString(root, "destDir", "dest_dir");
        string mode = GetString(root, "mode") ?? "all";
        int? pkgIdx = GetInt(root, "packageIndex", "package_index");
        string? subtreePath = GetString(root, "subtreePath", "subtree_path");
        bool openWhenDone = GetBool(root, "openWhenDone", "open_when_done") ?? true;

        if (string.IsNullOrWhiteSpace(filePath) || !File.Exists(filePath))
        {
            SendIpcResponse(new { type = "extract_error", error = $"NUP container file not found: {filePath}" });
            return;
        }

        if (string.IsNullOrWhiteSpace(destDir))
        {
            SendIpcResponse(new { type = "extract_error", error = "Destination directory cannot be empty." });
            return;
        }

        _extractCts?.Cancel();
        _extractCts = new CancellationTokenSource();
        var token = _extractCts.Token;

        var progress = new Progress<ProgressInfo>(info =>
        {
            SendIpcResponse(new
            {
                type = "extract_progress",
                info
            });
        });

        try
        {
            await Task.Run(() => NupEngine.Extract(filePath, destDir, mode, pkgIdx, subtreePath, progress, token), token);

            if (openWhenDone)
            {
                NupEngine.OpenFolder(destDir);
            }

            SendIpcResponse(new
            {
                type = "extract_done",
                success = true,
                destDir
            });
        }
        catch (OperationCanceledException)
        {
            SendIpcResponse(new
            {
                type = "extract_error",
                error = "Extraction cancelled by user."
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                type = "extract_error",
                error = ex.Message
            });
        }
    }

    private async Task HandleVerifyAsync(string id, JsonElement root)
    {
        string? filePath = GetString(root, "filePath", "file_path");
        bool deep = GetBool(root, "deep") ?? true;

        if (string.IsNullOrWhiteSpace(filePath) || !File.Exists(filePath))
        {
            SendIpcResponse(new { type = "verify_error", error = $"NUP container file not found: {filePath}" });
            return;
        }

        _verifyCts?.Cancel();
        _verifyCts = new CancellationTokenSource();
        var token = _verifyCts.Token;

        var progress = new Progress<VerifyProgressInfo>(info =>
        {
            if (info.Status == "done")
            {
                SendIpcResponse(new
                {
                    type = "verify_done",
                    info
                });
            }
            else
            {
                SendIpcResponse(new
                {
                    type = "verify_progress",
                    info
                });
            }
        });

        try
        {
            await Task.Run(() => NupEngine.Verify(filePath, deep, progress, token), token);
        }
        catch (OperationCanceledException)
        {
            SendIpcResponse(new
            {
                type = "verify_error",
                error = "Verification cancelled."
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                type = "verify_error",
                error = ex.Message
            });
        }
    }

    private async Task HandleCheckUpdateAsync(string id, JsonElement root)
    {
        bool manual = GetBool(root, "manual") ?? false;
        try
        {
            var res = await Task.Run(() => AppUpdateManager.CheckForUpdatesAsync());
            SendIpcResponse(new
            {
                id,
                action = "check_update_result",
                success = string.IsNullOrEmpty(res.Error),
                error = res.Error,
                manual,
                data = res
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                id,
                action = "check_update_result",
                success = false,
                manual,
                error = ex.Message
            });
        }
    }

    private async Task HandleStartUpdateAsync(string id, JsonElement root)
    {
        string? downloadUrl = GetString(root, "downloadUrl", "download_url");
        if (string.IsNullOrWhiteSpace(downloadUrl))
        {
            SendIpcResponse(new { type = "update_error", error = "No valid download URL provided for update." });
            return;
        }

        _updateCts?.Cancel();
        _updateCts = new CancellationTokenSource();
        var token = _updateCts.Token;

        var progress = new Progress<UpdateProgressInfo>(info =>
        {
            SendIpcResponse(new
            {
                type = "update_progress",
                info
            });
        });

        try
        {
            await Task.Run(() => AppUpdateManager.ApplyUpdateAsync(downloadUrl, progress, token), token);
        }
        catch (OperationCanceledException)
        {
            SendIpcResponse(new
            {
                type = "update_error",
                error = "Update cancelled by user."
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                type = "update_error",
                error = ex.Message
            });
        }
    }

    private void HandleCheckDiskSpace(string id, JsonElement root)
    {
        string? destDir = GetString(root, "destDir", "dest_dir", "path");
        ulong requiredBytes = (ulong)(GetInt(root, "requiredBytes", "required_bytes") ?? 0);

        if (root.TryGetProperty("requiredBytes", out var rb) && rb.ValueKind == JsonValueKind.Number)
        {
            if (rb.TryGetUInt64(out ulong rbu)) requiredBytes = rbu;
        }

        var info = NupEngine.CheckDiskSpace(destDir ?? "", requiredBytes);
        SendIpcResponse(new
        {
            id,
            action = "check_disk_space_result",
            success = true,
            data = info
        });
    }

    private async Task HandleExtractSingleFileAsync(string id, JsonElement root)
    {
        string? nupPath = GetString(root, "filePath", "file_path");
        int fileIndex = GetInt(root, "fileIndex", "file_index") ?? -1;
        string suggestedName = GetString(root, "fileName", "file_name", "name") ?? "extracted_file";

        if (string.IsNullOrWhiteSpace(nupPath) || !File.Exists(nupPath) || fileIndex < 0)
        {
            SendIpcResponse(new { id, action = "extract_single_file_result", success = false, error = "Invalid file parameters." });
            return;
        }

        var sfd = new SaveFileDialog
        {
            Title = $"Save {suggestedName} As",
            FileName = suggestedName,
            Filter = "All files (*.*)|*.*"
        };

        bool? result = sfd.ShowDialog(this);
        if (result != true || string.IsNullOrWhiteSpace(sfd.FileName))
        {
            SendIpcResponse(new { id, action = "extract_single_file_result", success = false, cancelled = true });
            return;
        }

        try
        {
            string dest = sfd.FileName;
            await Task.Run(() => NupEngine.ExtractSingleFile(nupPath, fileIndex, dest));
            SendIpcResponse(new
            {
                id,
                action = "extract_single_file_result",
                success = true,
                destPath = dest,
                fileName = Path.GetFileName(dest)
            });
        }
        catch (Exception ex)
        {
            SendIpcResponse(new
            {
                id,
                action = "extract_single_file_result",
                success = false,
                error = ex.Message
            });
        }
    }

    private void HandleBrowsePackFolder(string id)
    {
        var ofd = new OpenFolderDialog
        {
            Title = "Select Game Folder to Pack into .NUP",
            Multiselect = false
        };
        bool? result = ofd.ShowDialog(this);
        SendIpcResponse(new
        {
            id,
            action = "browse_pack_folder_result",
            success = result == true,
            path = result == true ? ofd.FolderName : null,
            name = result == true ? Path.GetFileName(ofd.FolderName) : null
        });
    }

    private void HandleBrowsePackOutput(string id, JsonElement root)
    {
        string defaultName = GetString(root, "defaultName", "name") ?? "game";
        var sfd = new SaveFileDialog
        {
            Title = "Save .NUP Container As",
            FileName = defaultName.EndsWith(".nup", StringComparison.OrdinalIgnoreCase) ? defaultName : defaultName + ".nup",
            Filter = "NUP Container (*.nup)|*.nup|All files (*.*)|*.*"
        };
        bool? result = sfd.ShowDialog(this);
        SendIpcResponse(new
        {
            id,
            action = "browse_pack_output_result",
            success = result == true,
            path = result == true ? sfd.FileName : null
        });
    }

    private void HandleStartPack(string id, JsonElement root)
    {
        string? sourceDir = GetString(root, "sourceDir", "source_dir");
        string? outputPath = GetString(root, "outputPath", "output_path");

        if (string.IsNullOrWhiteSpace(sourceDir) || !Directory.Exists(sourceDir))
        {
            SendIpcResponse(new { type = "pack_error", error = $"Source directory not found: {sourceDir}" });
            return;
        }

        if (string.IsNullOrWhiteSpace(outputPath))
        {
            SendIpcResponse(new { type = "pack_error", error = "Output file path cannot be empty." });
            return;
        }

        string titleName = GetString(root, "titleName", "title_name") ?? Path.GetFileName(sourceDir);
        string publisher = GetString(root, "publisher") ?? "Homebrew / Mod";
        string versionStr = GetString(root, "version") ?? "1.0.0";
        int compressionLevel = GetInt(root, "compressionLevel", "compression_level") ?? 3;

        ulong titleId = 0x0100000000001000UL;
        string? titleIdStr = GetString(root, "titleId", "title_id");
        if (!string.IsNullOrWhiteSpace(titleIdStr))
        {
            string clean = titleIdStr.Trim();
            if (clean.StartsWith("0x", StringComparison.OrdinalIgnoreCase))
                clean = clean[2..];
            if (ulong.TryParse(clean, System.Globalization.NumberStyles.HexNumber, null, out ulong parsedId))
                titleId = parsedId;
        }

        _packCts?.Cancel();
        _packCts = new CancellationTokenSource();
        var ct = _packCts.Token;

        var options = new PackOptions
        {
            SourceDirectory = sourceDir,
            OutputFilePath = outputPath,
            TitleName = titleName,
            Publisher = publisher,
            TitleId = titleId,
            VersionString = versionStr,
            CompressionLevel = compressionLevel
        };

        var progress = new Progress<PackProgressInfo>(info =>
        {
            SendIpcResponse(new { type = "pack_progress", info });
        });

        Task.Run(() =>
        {
            try
            {
                NupEngine.Pack(options, progress, ct);
                SendIpcResponse(new
                {
                    type = "pack_done",
                    outputPath = options.OutputFilePath,
                    sourceDir = options.SourceDirectory
                });
            }
            catch (OperationCanceledException)
            {
                SendIpcResponse(new { type = "pack_error", error = "Packaging cancelled by user." });
            }
            catch (Exception ex)
            {
                SendIpcResponse(new { type = "pack_error", error = ex.Message });
            }
        }, ct);
    }

    private void SendIpcResponse(object payload)
    {
        Dispatcher.InvokeAsync(() =>
        {
            if (WebView.CoreWebView2 != null)
            {
                string json = JsonSerializer.Serialize(payload, JsonOptions);
                WebView.CoreWebView2.PostWebMessageAsJson(json);
            }
        });
    }

    private static string? GetString(JsonElement elem, params string[] names)
    {
        foreach (var name in names)
        {
            if (elem.TryGetProperty(name, out var p) && p.ValueKind == JsonValueKind.String)
                return p.GetString();
        }
        return null;
    }

    private static int? GetInt(JsonElement elem, params string[] names)
    {
        foreach (var name in names)
        {
            if (elem.TryGetProperty(name, out var p) && p.ValueKind == JsonValueKind.Number)
                return p.GetInt32();
        }
        return null;
    }

    private static bool? GetBool(JsonElement elem, params string[] names)
    {
        foreach (var name in names)
        {
            if (elem.TryGetProperty(name, out var p) && (p.ValueKind == JsonValueKind.True || p.ValueKind == JsonValueKind.False))
                return p.GetBoolean();
        }
        return null;
    }
}
