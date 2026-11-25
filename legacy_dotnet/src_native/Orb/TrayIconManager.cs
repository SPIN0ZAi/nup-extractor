using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Interop;

namespace NupExtractorApp.Orb;

/// <summary>
/// Lightweight, zero-WinForms Win32 Shell_NotifyIconW system tray icon with native context menu.
/// </summary>
public sealed class TrayIconManager : IDisposable
{
    private const int WM_APP = 0x8000;
    private const int WM_TRAYICON = WM_APP + 42;
    private const int WM_LBUTTONUP = 0x0202;
    private const int WM_LBUTTONDBLCLK = 0x0203;
    private const int WM_RBUTTONUP = 0x0205;
    private const int WM_CONTEXTMENU = 0x007B;

    private const uint NIM_ADD = 0x00000000;
    private const uint NIM_MODIFY = 0x00000001;
    private const uint NIM_DELETE = 0x00000002;

    private const uint NIF_MESSAGE = 0x00000001;
    private const uint NIF_ICON = 0x00000002;
    private const uint NIF_TIP = 0x00000004;

    private const uint MF_STRING = 0x00000000;
    private const uint MF_SEPARATOR = 0x00000800;
    private const uint MF_GRAYED = 0x00000001;
    private const uint TPM_RIGHTBUTTON = 0x0002;
    private const uint TPM_RETURNCMD = 0x0100;

    private const uint CMD_OPEN = 1001;
    private const uint CMD_COLLAPSE = 1002;
    private const uint CMD_QUIT = 1003;

    private const uint IMAGE_ICON = 1;
    private const uint LR_LOADFROMFILE = 0x00000010;
    private const uint LR_DEFAULTSIZE = 0x00000040;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NOTIFYICONDATA
    {
        public int cbSize;
        public IntPtr hWnd;
        public uint uID;
        public uint uFlags;
        public uint uCallbackMessage;
        public IntPtr hIcon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)]
        public string szTip;
        public uint dwState;
        public uint dwStateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)]
        public string szInfo;
        public uint uTimeoutOrVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)]
        public string szInfoTitle;
        public uint dwInfoFlags;
        public Guid guidItem;
        public IntPtr hBalloonIcon;
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern bool Shell_NotifyIconW(uint dwMessage, ref NOTIFYICONDATA lpData);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern uint ExtractIconExW(string lpszFile, int nIconIndex, IntPtr[]? phiconLarge, IntPtr[]? phiconSmall, uint nIcons);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadImageW(IntPtr hInst, string name, uint type, int cx, int cy, uint fuLoad);

    [DllImport("user32.dll")]
    private static extern IntPtr LoadIcon(IntPtr hInstance, IntPtr lpIconName);

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr hIcon);

    [DllImport("user32.dll")]
    private static extern IntPtr CreatePopupMenu();

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool AppendMenuW(IntPtr hMenu, uint uFlags, UIntPtr uIDNewItem, string? lpNewItem);

    [DllImport("user32.dll")]
    private static extern bool DestroyMenu(IntPtr hMenu);

    [DllImport("user32.dll")]
    private static extern uint TrackPopupMenuEx(IntPtr hmenu, uint fuFlags, int x, int y, IntPtr hwnd, IntPtr lptpm);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    private readonly IntPtr _hwnd;
    private readonly HwndSource? _hwndSource;
    private readonly Func<bool> _isExpandedProvider;
    private readonly Action _onOpen;
    private readonly Action _onCollapse;
    private readonly Action _onQuit;

    private IntPtr _hIcon;
    private bool _ownsIcon;
    private bool _added;
    private bool _disposed;

    public TrayIconManager(
        IntPtr hwnd,
        Func<bool> isExpandedProvider,
        Action onOpen,
        Action onCollapse,
        Action onQuit)
    {
        _hwnd = hwnd;
        _isExpandedProvider = isExpandedProvider;
        _onOpen = onOpen;
        _onCollapse = onCollapse;
        _onQuit = onQuit;

        _hwndSource = HwndSource.FromHwnd(hwnd);
        _hwndSource?.AddHook(WndProc);

        _hIcon = LoadTrayIcon(out _ownsIcon);

        var nid = CreateBaseData("NUP Extractor — Floating Orb");
        _added = Shell_NotifyIconW(NIM_ADD, ref nid);
    }

    public void UpdateTooltip(string tooltip)
    {
        if (!_added || _disposed) return;
        string trimmed = tooltip.Length > 127 ? tooltip[..127] : tooltip;
        var nid = CreateBaseData(trimmed);
        Shell_NotifyIconW(NIM_MODIFY, ref nid);
    }

    private NOTIFYICONDATA CreateBaseData(string tip) => new()
    {
        cbSize = Marshal.SizeOf<NOTIFYICONDATA>(),
        hWnd = _hwnd,
        uID = 1,
        uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage = WM_TRAYICON,
        hIcon = _hIcon,
        szTip = tip
    };

    private static IntPtr LoadTrayIcon(out bool ownsIcon)
    {
        ownsIcon = false;
        try
        {
            // 1. Try extracting embedded application icon from current executable
            string? procPath = Environment.ProcessPath;
            if (!string.IsNullOrWhiteSpace(procPath) && File.Exists(procPath))
            {
                var smallIcons = new IntPtr[1];
                uint extracted = ExtractIconExW(procPath, 0, null, smallIcons, 1);
                if (extracted > 0 && smallIcons[0] != IntPtr.Zero)
                {
                    ownsIcon = true;
                    return smallIcons[0];
                }
            }

            // 2. Fallback to app_icon.ico next to exe or cwd
            string[] candidates =
            {
                Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "app_icon.ico"),
                Path.Combine(Directory.GetCurrentDirectory(), "app_icon.ico")
            };
            foreach (var c in candidates)
            {
                if (File.Exists(c))
                {
                    IntPtr loaded = LoadImageW(IntPtr.Zero, c, IMAGE_ICON, 16, 16, LR_LOADFROMFILE | LR_DEFAULTSIZE);
                    if (loaded != IntPtr.Zero)
                    {
                        ownsIcon = true;
                        return loaded;
                    }
                }
            }
        }
        catch { }

        // 3. Standard Win32 IDI_APPLICATION fallback
        return LoadIcon(IntPtr.Zero, new IntPtr(32512));
    }

    private IntPtr WndProc(IntPtr hwnd, int msg, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (msg == WM_TRAYICON)
        {
            int evt = unchecked((int)(lParam.ToInt64() & 0xFFFF));
            if (evt == WM_LBUTTONUP || evt == WM_LBUTTONDBLCLK)
            {
                if (_isExpandedProvider())
                    _onOpen(); // bring to front / ensure expanded
                else
                    _onOpen();
                handled = true;
            }
            else if (evt == WM_RBUTTONUP || evt == WM_CONTEXTMENU)
            {
                ShowContextMenu();
                handled = true;
            }
        }
        return IntPtr.Zero;
    }

    private void ShowContextMenu()
    {
        IntPtr menu = CreatePopupMenu();
        if (menu == IntPtr.Zero) return;

        try
        {
            bool expanded = _isExpandedProvider();
            AppendMenuW(menu, MF_STRING | (expanded ? MF_GRAYED : 0), new UIntPtr(CMD_OPEN), "Open NUP Extractor");
            AppendMenuW(menu, MF_STRING | (!expanded ? MF_GRAYED : 0), new UIntPtr(CMD_COLLAPSE), "Collapse to Orb");
            AppendMenuW(menu, MF_SEPARATOR, UIntPtr.Zero, null);
            AppendMenuW(menu, MF_STRING, new UIntPtr(CMD_QUIT), "Quit");

            var pt = NativeBackdropHelper.GetCursorPositionPhysical();
            SetForegroundWindow(_hwnd);
            uint cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.X, pt.Y, _hwnd, IntPtr.Zero);
            PostMessage(_hwnd, 0, IntPtr.Zero, IntPtr.Zero); // dismiss cleanly

            switch (cmd)
            {
                case CMD_OPEN:
                    _onOpen();
                    break;
                case CMD_COLLAPSE:
                    _onCollapse();
                    break;
                case CMD_QUIT:
                    _onQuit();
                    break;
            }
        }
        finally
        {
            DestroyMenu(menu);
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;

        _hwndSource?.RemoveHook(WndProc);

        if (_added)
        {
            var nid = new NOTIFYICONDATA
            {
                cbSize = Marshal.SizeOf<NOTIFYICONDATA>(),
                hWnd = _hwnd,
                uID = 1
            };
            Shell_NotifyIconW(NIM_DELETE, ref nid);
            _added = false;
        }

        if (_ownsIcon && _hIcon != IntPtr.Zero)
        {
            DestroyIcon(_hIcon);
            _hIcon = IntPtr.Zero;
        }
    }
}
