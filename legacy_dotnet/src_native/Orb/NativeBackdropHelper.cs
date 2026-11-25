using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using Microsoft.Win32;

namespace NupExtractorApp.Orb;

public readonly record struct SystemAppearanceState(
    bool ReducedMotion,
    bool HighContrast,
    bool TransparencyEnabled);

public static class NativeBackdropHelper
{
    private const int DWMWA_USE_IMMERSIVE_DARK_MODE = 20;
    private const int DWMWA_SYSTEMBACKDROP_TYPE = 38;
    private const int DWMSBT_NONE = 1;
    private const int DWMSBT_TRANSIENTWINDOW = 3; // Acrylic

    private const int WCA_ACCENT_POLICY = 19;
    private const int ACCENT_DISABLED = 0;
    private const int ACCENT_ENABLE_ACRYLICBLURBEHIND = 4;

    private const uint SPI_GETCLIENTAREAANIMATION = 0x1042;
    private const int GWL_EXSTYLE = -20;
    private const long WS_EX_TOOLWINDOW = 0x00000080L;
    private const long WS_EX_APPWINDOW = 0x00040000L;

    private static readonly IntPtr HWND_TOPMOST = new(-1);
    private static readonly IntPtr HWND_NOTOPMOST = new(-2);
    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_NOMOVE = 0x0002;
    private const uint SWP_NOZORDER = 0x0004;
    private const uint SWP_NOACTIVATE = 0x0010;
    private const uint SWP_FRAMECHANGED = 0x0020;
    private const uint SWP_NOCOPYBITS = 0x0100;

    private const uint MONITOR_DEFAULTTONEAREST = 2;
    private const int MDT_EFFECTIVE_DPI = 0;

    [StructLayout(LayoutKind.Sequential)]
    private struct MARGINS
    {
        public int Left;
        public int Right;
        public int Top;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct AccentPolicy
    {
        public int AccentState;
        public int AccentFlags;
        public uint GradientColor;
        public int AnimationId;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowCompositionAttributeData
    {
        public int Attribute;
        public IntPtr Data;
        public int SizeOfData;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct MONITORINFOEX
    {
        public int cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public uint dwFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
        public string szDevice;
    }

    private delegate bool MonitorEnumProc(IntPtr hMonitor, IntPtr hdcMonitor, ref RECT lprcMonitor, IntPtr dwData);

    [DllImport("dwmapi.dll")]
    private static extern int DwmExtendFrameIntoClientArea(IntPtr hWnd, ref MARGINS pMarInset);

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int attrValue, int attrSize);

    [DllImport("user32.dll")]
    private static extern int SetWindowCompositionAttribute(IntPtr hwnd, ref WindowCompositionAttributeData data);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateRoundRectRgn(int x1, int y1, int x2, int y2, int cx, int cy);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateEllipticRgn(int x1, int y1, int x2, int y2);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(IntPtr hObject);

    [DllImport("user32.dll")]
    private static extern int SetWindowRgn(IntPtr hWnd, IntPtr hRgn, bool bRedraw);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT lpPoint);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    private static extern bool EnumDisplayMonitors(IntPtr hdc, IntPtr lprcClip, MonitorEnumProc lpfnEnum, IntPtr dwData);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFOEX lpmi);

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromPoint(POINT pt, uint dwFlags);

    [DllImport("shcore.dll")]
    private static extern int GetDpiForMonitor(IntPtr hmonitor, int dpiType, out uint dpiX, out uint dpiY);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SystemParametersInfo(uint uiAction, uint uiParam, [MarshalAs(UnmanagedType.Bool)] out bool pvParam, uint fWinIni);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr64(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern IntPtr SetWindowLongPtr64(IntPtr hWnd, int nIndex, IntPtr dwNewLong);

    [DllImport("user32.dll")]
    private static extern bool ReleaseCapture();

    [DllImport("user32.dll")]
    private static extern IntPtr SendMessage(IntPtr hWnd, int msg, IntPtr wParam, IntPtr lParam);

    private const int WM_NCLBUTTONDOWN = 0x00A1;
    private static readonly IntPtr HTCAPTION = new(2);

    public static bool IsWindows11_22H2OrGreater() =>
        Environment.OSVersion.Version.Major >= 10 && Environment.OSVersion.Version.Build >= 22621;

    public static bool IsReducedMotionEnabled()
    {
        try
        {
            if (SystemParametersInfo(SPI_GETCLIENTAREAANIMATION, 0, out bool animationsEnabled, 0))
                return !animationsEnabled;
        }
        catch { }
        return false;
    }

    public static bool IsTransparencyEnabled()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", false);
            if (key != null)
            {
                object? val = key.GetValue("EnableTransparency");
                if (val is int intVal)
                    return intVal != 0;
            }
        }
        catch { }
        return true;
    }

    public static SystemAppearanceState QuerySystemAppearance()
    {
        bool highContrast = false;
        try
        {
            highContrast = System.Windows.SystemParameters.HighContrast;
        }
        catch { }

        return new SystemAppearanceState(
            ReducedMotion: IsReducedMotionEnabled(),
            HighContrast: highContrast,
            TransparencyEnabled: IsTransparencyEnabled() && !highContrast);
    }

    public static void ApplyBackdrop(IntPtr hwnd, bool isDarkTheme, bool transparencyEnabled, bool highContrast)
    {
        if (hwnd == IntPtr.Zero) return;

        var margins = new MARGINS { Left = -1, Right = -1, Top = -1, Bottom = -1 };
        DwmExtendFrameIntoClientArea(hwnd, ref margins);

        int darkFlag = isDarkTheme ? 1 : 0;
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, ref darkFlag, sizeof(int));

        bool enableBlur = transparencyEnabled && !highContrast;

        if (IsWindows11_22H2OrGreater())
        {
            int backdropType = enableBlur ? DWMSBT_TRANSIENTWINDOW : DWMSBT_NONE;
            int hr = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, ref backdropType, sizeof(int));
            if (hr == 0)
                return;
        }

        // Windows 10 or fallback Acrylic blur via SetWindowCompositionAttribute
        try
        {
            var accent = new AccentPolicy
            {
                AccentState = enableBlur ? ACCENT_ENABLE_ACRYLICBLURBEHIND : ACCENT_DISABLED,
                AccentFlags = enableBlur ? 2 : 0,
                // ABGR format: subtle translucent tint behind Liquid Glass CSS layers
                GradientColor = isDarkTheme ? 0x66121622u : 0x66F0F4FAu
            };
            int size = Marshal.SizeOf(accent);
            IntPtr ptr = Marshal.AllocHGlobal(size);
            try
            {
                Marshal.StructureToPtr(accent, ptr, false);
                var data = new WindowCompositionAttributeData
                {
                    Attribute = WCA_ACCENT_POLICY,
                    Data = ptr,
                    SizeOfData = size
                };
                SetWindowCompositionAttribute(hwnd, ref data);
            }
            finally
            {
                Marshal.FreeHGlobal(ptr);
            }
        }
        catch { }
    }

    public static void ApplyRoundedRegion(IntPtr hwnd, int widthPx, int heightPx, int cornerRadiusPx, bool isCircle)
    {
        ApplyRoundedRegionAt(hwnd, 0, 0, widthPx, heightPx, cornerRadiusPx, isCircle);
    }

    public static void ApplyRoundedRegionAt(IntPtr hwnd, int x, int y, int widthPx, int heightPx, int cornerRadiusPx, bool isCircle)
    {
        if (hwnd == IntPtr.Zero || widthPx <= 0 || heightPx <= 0) return;

        IntPtr hRgn;
        if (isCircle)
        {
            hRgn = CreateEllipticRgn(x, y, x + widthPx + 1, y + heightPx + 1);
        }
        else
        {
            int diam = Math.Clamp(cornerRadiusPx * 2, 0, Math.Min(widthPx, heightPx));
            hRgn = CreateRoundRectRgn(x, y, x + widthPx + 1, y + heightPx + 1, diam, diam);
        }

        if (hRgn == IntPtr.Zero) return;

        // Per Win32 docs: once SetWindowRgn succeeds (non-zero), OS owns the region handle.
        if (SetWindowRgn(hwnd, hRgn, true) == 0)
        {
            DeleteObject(hRgn);
        }
    }

    public static void ClearWindowRegion(IntPtr hwnd)
    {
        if (hwnd != IntPtr.Zero)
        {
            SetWindowRgn(hwnd, IntPtr.Zero, true);
        }
    }

    public static void SetWindowBoundsPhysical(IntPtr hwnd, PixelRect rect, bool? topmost = null, bool activate = false, bool noCopyBits = false)
    {
        if (hwnd == IntPtr.Zero) return;

        uint flags = 0;
        if (!activate) flags |= SWP_NOACTIVATE;
        if (noCopyBits) flags |= SWP_NOCOPYBITS;

        IntPtr insertAfter = IntPtr.Zero;
        if (topmost.HasValue)
        {
            insertAfter = topmost.Value ? HWND_TOPMOST : HWND_NOTOPMOST;
        }
        else
        {
            flags |= SWP_NOZORDER;
        }

        SetWindowPos(hwnd, insertAfter, rect.X, rect.Y, Math.Max(1, rect.Width), Math.Max(1, rect.Height), flags);
    }

    public static void SetTopmost(IntPtr hwnd, bool topmost)
    {
        if (hwnd == IntPtr.Zero) return;
        SetWindowPos(
            hwnd,
            topmost ? HWND_TOPMOST : HWND_NOTOPMOST,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }

    public static void SetToolWindowStyle(IntPtr hwnd, bool isToolWindow)
    {
        if (hwnd == IntPtr.Zero) return;
        long exStyle = GetWindowLongPtr64(hwnd, GWL_EXSTYLE).ToInt64();
        if (isToolWindow)
        {
            exStyle |= WS_EX_TOOLWINDOW;
            exStyle &= ~WS_EX_APPWINDOW;
        }
        else
        {
            exStyle &= ~WS_EX_TOOLWINDOW;
            exStyle |= WS_EX_APPWINDOW;
        }
        SetWindowLongPtr64(hwnd, GWL_EXSTYLE, new IntPtr(exStyle));
        SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    }

    public static void BeginNativeWindowDrag(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero) return;
        ReleaseCapture();
        SendMessage(hwnd, WM_NCLBUTTONDOWN, HTCAPTION, IntPtr.Zero);
    }

    public static PixelPoint GetCursorPositionPhysical()
    {
        return GetCursorPos(out var pt) ? new PixelPoint(pt.X, pt.Y) : new PixelPoint(0, 0);
    }

    public static PixelRect GetWindowBoundsPhysical(IntPtr hwnd)
    {
        if (hwnd != IntPtr.Zero && GetWindowRect(hwnd, out var rc))
        {
            return new PixelRect(rc.Left, rc.Top, rc.Right - rc.Left, rc.Bottom - rc.Top);
        }
        return new PixelRect(0, 0, 0, 0);
    }

    public static List<MonitorWorkArea> GetAllMonitors()
    {
        var result = new List<MonitorWorkArea>();
        try
        {
            EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (IntPtr hMonitor, IntPtr _, ref RECT _, IntPtr _) =>
            {
                var info = new MONITORINFOEX { cbSize = Marshal.SizeOf<MONITORINFOEX>() };
                if (GetMonitorInfo(hMonitor, ref info))
                {
                    double dpiScale = 1.0;
                    try
                    {
                        if (GetDpiForMonitor(hMonitor, MDT_EFFECTIVE_DPI, out uint dpiX, out _) == 0 && dpiX >= 48)
                        {
                            dpiScale = dpiX / 96.0;
                        }
                    }
                    catch { }

                    var bounds = new PixelRect(
                        info.rcMonitor.Left,
                        info.rcMonitor.Top,
                        info.rcMonitor.Right - info.rcMonitor.Left,
                        info.rcMonitor.Bottom - info.rcMonitor.Top);

                    var work = new PixelRect(
                        info.rcWork.Left,
                        info.rcWork.Top,
                        info.rcWork.Right - info.rcWork.Left,
                        info.rcWork.Bottom - info.rcWork.Top);

                    bool isPrimary = (info.dwFlags & 1) != 0; // MONITORINFOF_PRIMARY
                    result.Add(new MonitorWorkArea(info.szDevice ?? "", bounds, work, dpiScale, isPrimary));
                }
                return true;
            }, IntPtr.Zero);
        }
        catch { }

        if (result.Count == 0)
        {
            result.Add(new MonitorWorkArea("PRIMARY", new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 0, 1920, 1040), 1.0, true));
        }

        return result;
    }

    public static double GetDpiScaleForPoint(PixelPoint pt)
    {
        try
        {
            IntPtr hMon = MonitorFromPoint(new POINT { X = pt.X, Y = pt.Y }, MONITOR_DEFAULTTONEAREST);
            if (hMon != IntPtr.Zero && GetDpiForMonitor(hMon, MDT_EFFECTIVE_DPI, out uint dpiX, out _) == 0 && dpiX >= 48)
            {
                return dpiX / 96.0;
            }
        }
        catch { }
        return 1.0;
    }
}
