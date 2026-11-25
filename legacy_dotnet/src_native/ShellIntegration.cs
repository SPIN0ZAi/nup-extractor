using System;
using System.Diagnostics;
using System.IO;
using Microsoft.Win32;

namespace NupExtractorApp;

public static class ShellIntegration
{
    private const string ProgId = "NupExtractor.Package";
    private const string Extension = ".nup";

    public static bool IsRegistered()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\Classes\NupExtractor.Package\shell\ExtractHere");
            return key != null;
        }
        catch
        {
            return false;
        }
    }

    public static bool Register()
    {
        try
        {
            string exePath = Environment.ProcessPath ?? Process.GetCurrentProcess().MainModule?.FileName ?? "";
            if (string.IsNullOrEmpty(exePath) || !File.Exists(exePath))
                return false;

            // 1. Associate .nup extension
            using (var extKey = Registry.CurrentUser.CreateSubKey(@"Software\Classes\" + Extension))
            {
                extKey.SetValue("", ProgId);
            }

            // 2. Configure ProgId
            using (var progKey = Registry.CurrentUser.CreateSubKey(@"Software\Classes\" + ProgId))
            {
                progKey.SetValue("", "Nintendo Switch NX Package");
                
                using (var iconKey = progKey.CreateSubKey("DefaultIcon"))
                {
                    iconKey.SetValue("", $"\"{exePath}\",0");
                }

                // Default Open
                using (var openCmd = progKey.CreateSubKey(@"shell\open\command"))
                {
                    openCmd.SetValue("", $"\"{exePath}\" \"%1\"");
                }

                // Context Menu: "Extract to folder"
                using (var toKey = progKey.CreateSubKey(@"shell\ExtractToFolder"))
                {
                    toKey.SetValue("", "Extract to folder with NUP Extractor");
                    toKey.SetValue("Icon", $"\"{exePath}\"");
                    using var toCmd = toKey.CreateSubKey("command");
                    toCmd.SetValue("", $"\"{exePath}\" --extract-to-folder \"%1\"");
                }

                // Context Menu: "Extract Here"
                using (var hereKey = progKey.CreateSubKey(@"shell\ExtractHere"))
                {
                    hereKey.SetValue("", "Extract Here with NUP Extractor");
                    hereKey.SetValue("Icon", $"\"{exePath}\"");
                    using var hereCmd = hereKey.CreateSubKey("command");
                    hereCmd.SetValue("", $"\"{exePath}\" --extract-here \"%1\"");
                }
            }

            // 3. Folder Context Menu: "Pack to .NUP"
            using (var dirKey = Registry.CurrentUser.CreateSubKey(@"Software\Classes\Directory\shell\NupExtractor.Pack"))
            {
                dirKey.SetValue("", "Pack to .NUP with NUP Extractor");
                dirKey.SetValue("Icon", $"\"{exePath}\"");
                using var dirCmd = dirKey.CreateSubKey("command");
                dirCmd.SetValue("", $"\"{exePath}\" --pack-here \"%1\"");
            }

            return true;
        }
        catch
        {
            return false;
        }
    }

    public static bool Unregister()
    {
        try
        {
            Registry.CurrentUser.DeleteSubKeyTree(@"Software\Classes\" + Extension, false);
            Registry.CurrentUser.DeleteSubKeyTree(@"Software\Classes\" + ProgId, false);
            Registry.CurrentUser.DeleteSubKeyTree(@"Software\Classes\Directory\shell\NupExtractor.Pack", false);
            return true;
        }
        catch
        {
            return false;
        }
    }
}

