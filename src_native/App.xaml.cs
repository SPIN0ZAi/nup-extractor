using System;
using System.IO;
using System.Text.Json;
using System.Threading;
using System.Windows;

namespace NupExtractorApp;

public partial class App : Application
{
    public static string? InitialFilePath { get; set; }

    protected override void OnStartup(StartupEventArgs e)
    {
        if (e.Args.Length > 0)
        {
            string first = e.Args[0];

            if (first == "--update-replace" && e.Args.Length >= 3)
            {
                string targetExe = e.Args[1];
                if (int.TryParse(e.Args[2], out int oldPid))
                {
                    PerformInPlaceUpdate(targetExe, oldPid);
                    return;
                }
            }

            if (first == "--extract-here" && e.Args.Length > 1)
            {
                string target = e.Args[1];
                string dest = Path.GetDirectoryName(Path.GetFullPath(target)) ?? Directory.GetCurrentDirectory();
                try
                {
                    var progress = new Progress<ProgressInfo>(_ => { });
                    NupEngine.Extract(target, dest, "all", null, null, progress, CancellationToken.None);
                    NupEngine.OpenFolder(dest);
                }
                catch (Exception ex)
                {
                    MessageBox.Show($"Failed to extract '{Path.GetFileName(target)}':\n{ex.Message}", "Extraction Error", MessageBoxButton.OK, MessageBoxImage.Error);
                }
                Shutdown(0);
                return;
            }

            if (first == "--extract-to-folder" && e.Args.Length > 1)
            {
                string target = e.Args[1];
                string baseDir = Path.GetDirectoryName(Path.GetFullPath(target)) ?? Directory.GetCurrentDirectory();
                string folderName = Path.GetFileNameWithoutExtension(target) + "_extracted";
                string dest = Path.Combine(baseDir, folderName);
                try
                {
                    var progress = new Progress<ProgressInfo>(_ => { });
                    NupEngine.Extract(target, dest, "all", null, null, progress, CancellationToken.None);
                    NupEngine.OpenFolder(dest);
                }
                catch (Exception ex)
                {
                    MessageBox.Show($"Failed to extract '{Path.GetFileName(target)}':\n{ex.Message}", "Extraction Error", MessageBoxButton.OK, MessageBoxImage.Error);
                }
                Shutdown(0);
                return;
            }

            if (first == "--pack" && e.Args.Length > 2)
            {
                string sourceFolder = e.Args[1];
                string outputNup = e.Args[2];
                try
                {
                    var opt = new PackOptions
                    {
                        SourceDirectory = sourceFolder,
                        OutputFilePath = outputNup,
                        TitleName = Path.GetFileName(sourceFolder)
                    };
                    NupWriter.Pack(opt);
                }
                catch (Exception ex)
                {
                    MessageBox.Show($"Failed to pack '{sourceFolder}':\n{ex.Message}", "Packaging Error", MessageBoxButton.OK, MessageBoxImage.Error);
                }
                Shutdown(0);
                return;
            }

            if (first == "--pack-here" && e.Args.Length > 1)
            {
                string sourceFolder = e.Args[1];
                string baseDir = Path.GetDirectoryName(Path.GetFullPath(sourceFolder)) ?? Directory.GetCurrentDirectory();
                string outputNup = Path.Combine(baseDir, Path.GetFileName(sourceFolder) + ".nup");
                try
                {
                    var opt = new PackOptions
                    {
                        SourceDirectory = sourceFolder,
                        OutputFilePath = outputNup,
                        TitleName = Path.GetFileName(sourceFolder)
                    };
                    NupWriter.Pack(opt);
                    NupEngine.OpenFolder(baseDir);
                }
                catch (Exception ex)
                {
                    MessageBox.Show($"Failed to pack '{Path.GetFileName(sourceFolder)}':\n{ex.Message}", "Packaging Error", MessageBoxButton.OK, MessageBoxImage.Error);
                }
                Shutdown(0);
                return;
            }

            if (File.Exists(first) && first.EndsWith(".nup", StringComparison.OrdinalIgnoreCase))
            {
                InitialFilePath = Path.GetFullPath(first);
            }
        }
        if (e.Args.Length > 0 && e.Args[0] == "--test-inspect")
        {
            string target = e.Args.Length > 1 ? e.Args[1] : "sample_game.nup";
            try
            {
                var data = NupEngine.Inspect(target);
                Console.WriteLine("INSPECT_SUCCESS: " + JsonSerializer.Serialize(data));
            }
            catch (Exception ex)
            {
                Console.WriteLine("INSPECT_ERROR: " + ex.Message);
            }
            Shutdown(0);
            return;
        }

        if (e.Args.Length > 0 && e.Args[0] == "--test-extract")
        {
            string target = e.Args.Length > 1 ? e.Args[1] : "sample_game.nup";
            string dest = e.Args.Length > 2 ? e.Args[2] : "test_native_extract";
            try
            {
                var progress = new Progress<ProgressInfo>(p =>
                {
                    if (p.Status == "extracting")
                        Console.WriteLine($"PROGRESS: {p.Current}/{p.Total} - {p.FileName}");
                });
                NupEngine.Extract(target, dest, "all", null, null, progress, CancellationToken.None);
                Console.WriteLine("EXTRACT_SUCCESS");
            }
            catch (Exception ex)
            {
                Console.WriteLine("EXTRACT_ERROR: " + ex.Message);
            }
            Shutdown(0);
            return;
        }

        base.OnStartup(e);
    }

    private void PerformInPlaceUpdate(string targetExe, int oldPid)
    {
        try
        {
            try
            {
                var oldProc = System.Diagnostics.Process.GetProcessById(oldPid);
                oldProc.WaitForExit(15000);
            }
            catch { }

            Thread.Sleep(500);

            string currentExe = Environment.ProcessPath ?? "";
            if (!string.IsNullOrEmpty(currentExe) && File.Exists(currentExe))
            {
                string oldBackup = targetExe + ".old";

                if (File.Exists(oldBackup))
                {
                    try { File.Delete(oldBackup); } catch { }
                }

                if (File.Exists(targetExe))
                {
                    File.Move(targetExe, oldBackup);
                }

                File.Copy(currentExe, targetExe, overwrite: true);

                try { File.Delete(oldBackup); } catch { }

                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
                {
                    FileName = targetExe,
                    UseShellExecute = true
                });
            }
        }
        catch (Exception ex)
        {
            MessageBox.Show($"Failed to apply update to '{targetExe}':\n{ex.Message}", "Update Error", MessageBoxButton.OK, MessageBoxImage.Error);
        }
        finally
        {
            Shutdown(0);
        }
    }
}
