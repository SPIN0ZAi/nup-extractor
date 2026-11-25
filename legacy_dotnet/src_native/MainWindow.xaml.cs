using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using Microsoft.Web.WebView2.Core;
using Microsoft.Win32;
using NupExtractorApp.Orb;

namespace NupExtractorApp;

public partial class MainWindow : Window
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
        WriteIndented = false
    };

    private const int WM_GETMINMAXINFO = 0x0024;
    private const int WM_SIZE = 0x0005;
    private const int WM_SYSCOMMAND = 0x0112;
    private const int WM_EXITSIZEMOVE = 0x0232;
    private const int WM_DPICHANGED = 0x02E0;
    private const int SC_MINIMIZE = 0xF020;

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }

    [StructLayout(LayoutKind.Sequential)]
    private struct MINMAXINFO
    {
        public POINT ptReserved;
        public POINT ptMaxSize;
        public POINT ptMaxPosition;
        public POINT ptMinTrackSize;
        public POINT ptMaxTrackSize;
    }

    private CancellationTokenSource? _extractCts;
    private CancellationTokenSource? _verifyCts;
    private CancellationTokenSource? _updateCts;
    private CancellationTokenSource? _packCts;

    // Floating Orb & Liquid Glass state
    private readonly Stopwatch _clock = Stopwatch.StartNew();
    private readonly OrbSettings _settings;
    private readonly OrbStateMachine _stateMachine;
    private SystemAppearanceState _appearance;
    private IntPtr _hwnd;
    private HwndSource? _hwndSource;
    private TrayIconManager? _trayIcon;
    private bool _webViewReady;
    private bool _isAnimHooked;

    private PixelRect _orbRect;
    private PixelRect _expandedRect;
    private PixelRect _morphHostRect;
    private bool _morphHostActive;
    private int _morphWarmupFrames;
    private MonitorWorkArea _orbMonitor;
    private ScreenEdge _orbEdge;

    // Orb drag & edge-snap animation state
    private bool _isDraggingOrb;
    private double _dragGrabFracX = 0.5;
    private double _dragGrabFracY = 0.5;
    private int _lastOrbRegionSize;

    private bool _isSnappingOrb;
    private PixelRect _snapStartRect;
    private PixelRect _snapTargetRect;
    private double _snapStartMs;
    private const double SnapDurationMs = 190.0;

    public MainWindow()
    {
        InitializeComponent();

        _settings = OrbSettingsStore.Load();
        _appearance = NativeBackdropHelper.QuerySystemAppearance();

        var startupState = ResolveInitialStartupState();
        _stateMachine = new OrbStateMachine(startupState, _appearance.ReducedMotion);

        var monitors = NativeBackdropHelper.GetAllMonitors();
        (_orbRect, _orbMonitor, _orbEdge) = OrbGeometry.ResolveInitialOrbPosition(_settings, monitors);
        _expandedRect = OrbGeometry.ComputeExpandedRectNearOrb(
            _orbRect,
            new DipSize(_settings.ExpandedWidthDip, _settings.ExpandedHeightDip),
            _orbMonitor);

        ShowInTaskbar = startupState == OrbWindowState.Expanded;
        Topmost = startupState == OrbWindowState.Collapsed;

        // Pre-seed WPF logical bounds so window starts cleanly without jump
        var initRect = startupState == OrbWindowState.Expanded ? _expandedRect : _orbRect;
        double initScale = _orbMonitor.DpiScale > 0.25 ? _orbMonitor.DpiScale : 1.0;
        Left = initRect.X / initScale;
        Top = initRect.Y / initScale;
        Width = initRect.Width / initScale;
        Height = initRect.Height / initScale;

        SourceInitialized += MainWindow_SourceInitialized;
        Loaded += MainWindow_Loaded;
        Closing += MainWindow_Closing;
        Closed += MainWindow_Closed;
        StateChanged += MainWindow_StateChanged;
    }

    private OrbWindowState ResolveInitialStartupState()
    {
        if (!string.IsNullOrWhiteSpace(App.StartupModeOverride))
        {
            if (string.Equals(App.StartupModeOverride, "expanded", StringComparison.OrdinalIgnoreCase))
                return OrbWindowState.Expanded;
            if (string.Equals(App.StartupModeOverride, "collapsed", StringComparison.OrdinalIgnoreCase))
                return OrbWindowState.Collapsed;
        }
        return _settings.ResolveStartupState(!string.IsNullOrWhiteSpace(App.InitialFilePath));
    }

    private void MainWindow_SourceInitialized(object? sender, EventArgs e)
    {
        _hwnd = new WindowInteropHelper(this).Handle;
        _hwndSource = HwndSource.FromHwnd(_hwnd);
        if (_hwndSource != null)
        {
            if (_hwndSource.CompositionTarget != null)
                _hwndSource.CompositionTarget.BackgroundColor = Colors.Transparent;
            _hwndSource.AddHook(WndProc);
        }

        ApplyNativeBackdrop();

        bool startExpanded = _stateMachine.State == OrbWindowState.Expanded;
        var initialRect = startExpanded ? _expandedRect : _orbRect;
        NativeBackdropHelper.SetToolWindowStyle(_hwnd, !startExpanded);
        NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, initialRect, topmost: !startExpanded, activate: startExpanded);
        ApplyRegionForCurrentState(initialRect, startExpanded ? 1.0 : 0.0);
        AppWindowChrome.ResizeBorderThickness = startExpanded ? new Thickness(6) : new Thickness(0);

        _trayIcon = new TrayIconManager(
            _hwnd,
            isExpandedProvider: () => _stateMachine.State is OrbWindowState.Expanded or OrbWindowState.Expanding,
            onOpen: () => Dispatcher.InvokeAsync(() =>
            {
                if (_stateMachine.State is OrbWindowState.Collapsed or OrbWindowState.Collapsing)
                    StartExpand();
                else
                {
                    if (WindowState == WindowState.Minimized)
                        WindowState = WindowState.Normal;
                    Activate();
                }
            }),
            onCollapse: () => Dispatcher.InvokeAsync(() =>
            {
                if (_stateMachine.State is OrbWindowState.Expanded or OrbWindowState.Expanding)
                    StartCollapse();
            }),
            onQuit: () => Dispatcher.InvokeAsync(() =>
            {
                Close();
            }));

        SystemParameters.StaticPropertyChanged += SystemParameters_StaticPropertyChanged;
        SystemEvents.UserPreferenceChanged += SystemEvents_UserPreferenceChanged;
        SystemEvents.DisplaySettingsChanged += SystemEvents_DisplaySettingsChanged;
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
            WebView.CoreWebView2.NavigationCompleted += CoreWebView2_NavigationCompleted;

            // Navigate to embedded app
            WebView.CoreWebView2.Navigate("https://nup.local/index.html");
        }
        catch (Exception ex)
        {
            MessageBox.Show($"Failed to initialize WebView2:\n{ex.Message}\n\nPlease ensure WebView2 Runtime is installed.",
                "Initialization Error", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private void CoreWebView2_NavigationCompleted(object? sender, CoreWebView2NavigationCompletedEventArgs e)
    {
        _webViewReady = true;
        PushSystemAppearance();
        SendMorphFrame(_stateMachine.State, _stateMachine.Progress,
            _stateMachine.State == OrbWindowState.Expanded ? _expandedRect : _orbRect);
    }

    private void MainWindow_StateChanged(object? sender, EventArgs e)
    {
        if (_stateMachine.State == OrbWindowState.Expanded && !_stateMachine.IsAnimating)
        {
            if (WindowState == WindowState.Minimized)
            {
                // Intercept OS minimize and collapse to orb instead
                WindowState = WindowState.Normal;
                StartCollapse();
            }
            else if (WindowState == WindowState.Maximized)
            {
                AppWindowChrome.ResizeBorderThickness = new Thickness(0);
                NativeBackdropHelper.ClearWindowRegion(_hwnd);
                SendIpcResponse(new { type = "window_maximized_changed", maximized = true });
            }
            else if (WindowState == WindowState.Normal)
            {
                AppWindowChrome.ResizeBorderThickness = new Thickness(6);
                var rect = NativeBackdropHelper.GetWindowBoundsPhysical(_hwnd);
                if (rect.Width > 0 && rect.Height > 0)
                {
                    ApplyRegionForCurrentState(rect, 1.0);
                }
                SendIpcResponse(new { type = "window_maximized_changed", maximized = false });
            }
        }
    }

    private void MainWindow_Closing(object? sender, CancelEventArgs e)
    {
        PersistCurrentState();
    }

    private void MainWindow_Closed(object? sender, EventArgs e)
    {
        DetachAnimationLoop();
        SystemParameters.StaticPropertyChanged -= SystemParameters_StaticPropertyChanged;
        SystemEvents.UserPreferenceChanged -= SystemEvents_UserPreferenceChanged;
        SystemEvents.DisplaySettingsChanged -= SystemEvents_DisplaySettingsChanged;
        _hwndSource?.RemoveHook(WndProc);
        _trayIcon?.Dispose();
        _trayIcon = null;
        _extractCts?.Cancel();
        _verifyCts?.Cancel();
        _updateCts?.Cancel();
        _packCts?.Cancel();
    }

    private void SystemParameters_StaticPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(SystemParameters.HighContrast) ||
            e.PropertyName == nameof(SystemParameters.ClientAreaAnimation))
        {
            Dispatcher.InvokeAsync(RefreshSystemAppearance);
        }
    }

    private void SystemEvents_UserPreferenceChanged(object? sender, UserPreferenceChangedEventArgs e)
    {
        Dispatcher.InvokeAsync(RefreshSystemAppearance);
    }

    private void SystemEvents_DisplaySettingsChanged(object? sender, EventArgs e)
    {
        Dispatcher.InvokeAsync(() =>
        {
            var monitors = NativeBackdropHelper.GetAllMonitors();
            (_orbRect, _orbMonitor, _orbEdge) = OrbGeometry.ResolveInitialOrbPosition(_settings, monitors);
            _expandedRect = OrbGeometry.ComputeExpandedRectNearOrb(
                _orbRect,
                new DipSize(_settings.ExpandedWidthDip, _settings.ExpandedHeightDip),
                _orbMonitor);

            if (_stateMachine.State == OrbWindowState.Collapsed && !_isDraggingOrb && !_isSnappingOrb)
            {
                NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _orbRect, topmost: true, activate: false);
                ApplyRegionForCurrentState(_orbRect, 0.0);
                SendMorphFrame(OrbWindowState.Collapsed, 0.0, _orbRect);
            }
        });
    }

    private void RefreshSystemAppearance()
    {
        _appearance = NativeBackdropHelper.QuerySystemAppearance();
        _stateMachine.ReducedMotion = _appearance.ReducedMotion;
        ApplyNativeBackdrop();
        PushSystemAppearance();
    }

    private void ApplyNativeBackdrop()
    {
        bool isDark = !string.Equals(_settings.Theme, "light", StringComparison.OrdinalIgnoreCase);
        NativeBackdropHelper.ApplyBackdrop(_hwnd, isDark, _appearance.TransparencyEnabled, _appearance.HighContrast);
    }

    private void PushSystemAppearance()
    {
        if (!_webViewReady) return;
        SendIpcResponse(new
        {
            type = "system_appearance",
            reducedMotion = _appearance.ReducedMotion,
            highContrast = _appearance.HighContrast,
            transparencyEnabled = _appearance.TransparencyEnabled,
            theme = _settings.Theme
        });
    }

    private IntPtr WndProc(IntPtr hwnd, int msg, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        switch (msg)
        {
            case WM_SYSCOMMAND:
                int cmd = (int)(wParam.ToInt64() & 0xFFF0);
                if (cmd == SC_MINIMIZE)
                {
                    handled = true;
                    Dispatcher.InvokeAsync(StartCollapse);
                    return IntPtr.Zero;
                }
                break;

            case WM_GETMINMAXINFO:
                HandleGetMinMaxInfo(lParam);
                handled = true;
                return IntPtr.Zero;

            case WM_SIZE:
                if (_stateMachine.State == OrbWindowState.Expanded && !_stateMachine.IsAnimating && WindowState == WindowState.Normal)
                {
                    int w = (int)(lParam.ToInt64() & 0xFFFF);
                    int h = (int)((lParam.ToInt64() >> 16) & 0xFFFF);
                    if (w > 100 && h > 100)
                    {
                        var bounds = NativeBackdropHelper.GetWindowBoundsPhysical(_hwnd);
                        ApplyRegionForCurrentState(new PixelRect(bounds.X, bounds.Y, w, h), 1.0);
                    }
                }
                break;

            case WM_EXITSIZEMOVE:
                if (_stateMachine.State == OrbWindowState.Expanded && !_stateMachine.IsAnimating && WindowState == WindowState.Normal)
                {
                    var rect = NativeBackdropHelper.GetWindowBoundsPhysical(_hwnd);
                    if (rect.Width > 200 && rect.Height > 200)
                    {
                        _expandedRect = rect;
                        double scale = NativeBackdropHelper.GetDpiScaleForPoint(new PixelPoint(rect.CenterX, rect.CenterY));
                        _settings.ExpandedWidthDip = Math.Max(OrbGeometry.MinExpandedWidthDip, OrbGeometry.PhysicalToDip(rect.Width, scale));
                        _settings.ExpandedHeightDip = Math.Max(OrbGeometry.MinExpandedHeightDip, OrbGeometry.PhysicalToDip(rect.Height, scale));
                        ApplyRegionForCurrentState(rect, 1.0);
                        OrbSettingsStore.Save(_settings);
                    }
                }
                break;

            case WM_DPICHANGED:
                if (_stateMachine.State == OrbWindowState.Collapsed && !_isDraggingOrb && !_stateMachine.IsAnimating)
                {
                    Dispatcher.InvokeAsync(() =>
                    {
                        var monitors = NativeBackdropHelper.GetAllMonitors();
                        var mon = OrbGeometry.FindMonitorForPoint(new PixelPoint(_orbRect.CenterX, _orbRect.CenterY), monitors);
                        int orbPx = OrbGeometry.ComputeOrbPhysicalSize(OrbGeometry.DefaultOrbDipSize, mon.DpiScale);
                        int marginPx = OrbGeometry.DipToPhysical(OrbGeometry.DefaultEdgeMarginDip, mon.DpiScale);
                        var resized = new PixelRect(_orbRect.CenterX - orbPx / 2, _orbRect.CenterY - orbPx / 2, orbPx, orbPx);
                        (_orbRect, _orbEdge) = OrbGeometry.SnapOrbToNearestEdge(resized, mon.WorkArea, marginPx);
                        _orbMonitor = mon;
                        NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _orbRect, topmost: true, activate: false);
                        ApplyRegionForCurrentState(_orbRect, 0.0);
                    });
                }
                break;
        }
        return IntPtr.Zero;
    }

    private void HandleGetMinMaxInfo(IntPtr lParam)
    {
        var mmi = Marshal.PtrToStructure<MINMAXINFO>(lParam);

        var bounds = NativeBackdropHelper.GetWindowBoundsPhysical(_hwnd);
        var monitors = NativeBackdropHelper.GetAllMonitors();
        var mon = OrbGeometry.FindMonitorForPoint(new PixelPoint(bounds.CenterX, bounds.CenterY), monitors);

        mmi.ptMaxPosition.X = mon.WorkArea.Left - mon.Bounds.Left;
        mmi.ptMaxPosition.Y = mon.WorkArea.Top - mon.Bounds.Top;
        mmi.ptMaxSize.X = mon.WorkArea.Width;
        mmi.ptMaxSize.Y = mon.WorkArea.Height;

        if (_stateMachine.State == OrbWindowState.Expanded && !_stateMachine.IsAnimating)
        {
            mmi.ptMinTrackSize.X = OrbGeometry.DipToPhysical(OrbGeometry.MinExpandedWidthDip, mon.DpiScale);
            mmi.ptMinTrackSize.Y = OrbGeometry.DipToPhysical(OrbGeometry.MinExpandedHeightDip, mon.DpiScale);
        }
        else
        {
            mmi.ptMinTrackSize.X = 1;
            mmi.ptMinTrackSize.Y = 1;
        }

        Marshal.StructureToPtr(mmi, lParam, true);
    }

    // =========================================================
    // Floating Orb Morph & Drag Engine
    // =========================================================

    private void StartExpand()
    {
        if (_stateMachine.State is OrbWindowState.Expanded or OrbWindowState.Expanding)
            return;

        _isSnappingOrb = false;
        _isDraggingOrb = false;

        if (_stateMachine.State == OrbWindowState.Collapsed)
        {
            var monitors = NativeBackdropHelper.GetAllMonitors();
            _orbMonitor = OrbGeometry.FindMonitorForPoint(new PixelPoint(_orbRect.CenterX, _orbRect.CenterY), monitors);
            _expandedRect = OrbGeometry.ComputeExpandedRectNearOrb(
                _orbRect,
                new DipSize(_settings.ExpandedWidthDip, _settings.ExpandedHeightDip),
                _orbMonitor);
        }

        ShowInTaskbar = true;
        NativeBackdropHelper.SetToolWindowStyle(_hwnd, false);
        AppWindowChrome.ResizeBorderThickness = new Thickness(0);

        _stateMachine.RequestExpand(_clock.Elapsed.TotalMilliseconds);

        if (_appearance.ReducedMotion)
        {
            // Swap morph for quick fade/scale at final bounds
            _morphHostActive = false;
            NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _expandedRect, topmost: false, activate: true);
            ApplyRegionForCurrentState(_expandedRect, 1.0);
        }
        else
        {
            // Render WebView2 ONCE at the bounding host rect and clip with animated SetWindowRgn
            // to eliminate per-frame WebView2 swapchain reallocation / blank frames.
            EnterMorphHost(_orbRect, _stateMachine.Progress);
        }

        AttachAnimationLoop();
    }

    private void StartCollapse()
    {
        if (_stateMachine.State is OrbWindowState.Collapsed or OrbWindowState.Collapsing)
            return;

        _isSnappingOrb = false;

        if (WindowState == WindowState.Maximized)
        {
            WindowState = WindowState.Normal;
        }

        if (_stateMachine.State == OrbWindowState.Expanded)
        {
            var currentBounds = NativeBackdropHelper.GetWindowBoundsPhysical(_hwnd);
            if (currentBounds.Width > 200 && currentBounds.Height > 200)
            {
                _expandedRect = currentBounds;
                double scale = NativeBackdropHelper.GetDpiScaleForPoint(new PixelPoint(currentBounds.CenterX, currentBounds.CenterY));
                _settings.ExpandedWidthDip = Math.Max(OrbGeometry.MinExpandedWidthDip, OrbGeometry.PhysicalToDip(currentBounds.Width, scale));
                _settings.ExpandedHeightDip = Math.Max(OrbGeometry.MinExpandedHeightDip, OrbGeometry.PhysicalToDip(currentBounds.Height, scale));
            }
        }

        // Ensure orb destination is still valid on current displays
        var monitors = NativeBackdropHelper.GetAllMonitors();
        _orbMonitor = OrbGeometry.FindMonitorForPoint(new PixelPoint(_orbRect.CenterX, _orbRect.CenterY), monitors);
        int orbPx = OrbGeometry.ComputeOrbPhysicalSize(OrbGeometry.DefaultOrbDipSize, _orbMonitor.DpiScale);
        int marginPx = OrbGeometry.DipToPhysical(OrbGeometry.DefaultEdgeMarginDip, _orbMonitor.DpiScale);
        var candidateOrb = new PixelRect(_orbRect.CenterX - orbPx / 2, _orbRect.CenterY - orbPx / 2, orbPx, orbPx);
        (_orbRect, _orbEdge) = OrbGeometry.SnapOrbToNearestEdge(candidateOrb, _orbMonitor.WorkArea, marginPx);

        AppWindowChrome.ResizeBorderThickness = new Thickness(0);
        Topmost = true;
        NativeBackdropHelper.SetTopmost(_hwnd, true);

        _stateMachine.RequestCollapse(_clock.Elapsed.TotalMilliseconds);

        // On collapse, shrink the actual HWND directly (never keep a full-size _morphHostRect)
        // so Windows 11 DWM Acrylic shrinks pixel-for-pixel with the window with zero ghost backdrop.
        _morphHostActive = false;
        _morphWarmupFrames = 0;

        if (!_appearance.ReducedMotion)
        {
            var currentRect = OrbGeometry.InterpolateRect(_orbRect, _expandedRect, _stateMachine.Progress);
            NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, currentRect, topmost: true, activate: false, noCopyBits: true);
            ApplyRegionForCurrentState(currentRect, _stateMachine.Progress);
            SendMorphFrame(_stateMachine.State, _stateMachine.Progress, currentRect);
        }

        AttachAnimationLoop();
    }

    private void EnterMorphHost(PixelRect initialVisibleRect, double initialProgress)
    {
        _morphHostRect = OrbGeometry.Union(_orbRect, _expandedRect);
        if (!_morphHostActive)
        {
            _morphHostActive = true;
            // Allow 2 composition frames for WebView2 to present the host-sized swapchain
            // while still clipped to the exact starting orb circle.
            _morphWarmupFrames = initialProgress < 0.05 ? 2 : 0;
            NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _morphHostRect, topmost: true, activate: false);
        }
        ApplyClippedMorphRegion(initialVisibleRect, initialProgress);
        SendMorphFrame(_stateMachine.State, initialProgress, initialVisibleRect);
    }

    private void AttachAnimationLoop()
    {
        if (_isAnimHooked) return;
        _isAnimHooked = true;
        CompositionTarget.Rendering += OnAnimationFrame;
    }

    private void DetachAnimationLoop()
    {
        if (!_isAnimHooked) return;
        _isAnimHooked = false;
        CompositionTarget.Rendering -= OnAnimationFrame;
    }

    private void OnAnimationFrame(object? sender, EventArgs e)
    {
        double nowMs = _clock.Elapsed.TotalMilliseconds;
        bool stillActive = false;

        if (_stateMachine.IsAnimating)
        {
            if (_morphWarmupFrames > 0)
            {
                _morphWarmupFrames--;
                if (_morphWarmupFrames == 0)
                {
                    // Re-anchor state machine start time after warmup so spring runs full duration
                    if (_stateMachine.State == OrbWindowState.Expanding)
                    {
                        _stateMachine.SetImmediateState(OrbWindowState.Collapsed);
                        _stateMachine.RequestExpand(nowMs);
                    }
                }
                return;
            }

            var snap = _stateMachine.Tick(nowMs);
            PixelRect frameRect;

            if (_appearance.ReducedMotion)
            {
                frameRect = snap.State == OrbWindowState.Collapsed ? _orbRect : _expandedRect;
                if (!snap.IsAnimating)
                {
                    NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, frameRect, topmost: snap.State == OrbWindowState.Collapsed, activate: snap.State == OrbWindowState.Expanded);
                    ApplyRegionForCurrentState(frameRect, snap.Progress);
                }
            }
            else if (_morphHostActive)
            {
                frameRect = OrbGeometry.InterpolateRect(_orbRect, _expandedRect, snap.Progress);
                ApplyClippedMorphRegion(frameRect, snap.Progress);
            }
            else
            {
                frameRect = OrbGeometry.InterpolateRect(_orbRect, _expandedRect, snap.Progress);
                NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, frameRect, topmost: null, activate: false, noCopyBits: true);
                ApplyRegionForCurrentState(frameRect, snap.Progress);
            }

            SendMorphFrame(snap.State, snap.Progress, frameRect);

            if (!snap.IsAnimating)
            {
                OnMorphSettled(snap.State);
            }
            else
            {
                stillActive = true;
            }
        }
        else if (_isSnappingOrb)
        {
            double elapsed = nowMs - _snapStartMs;
            double rawT = _appearance.ReducedMotion ? 1.0 : Math.Clamp(elapsed / SnapDurationMs, 0.0, 1.0);
            // Smooth ease-out cubic for AssistiveTouch edge snap
            double easedT = 1.0 - Math.Pow(1.0 - rawT, 3.0);
            _orbRect = OrbGeometry.InterpolateRect(_snapStartRect, _snapTargetRect, easedT);
            NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _orbRect, topmost: true, activate: false);
            ApplyRegionForCurrentState(_orbRect, 0.0);

            if (rawT >= 1.0)
            {
                _isSnappingOrb = false;
                _orbRect = _snapTargetRect;
                PersistCurrentState();
                SendMorphFrame(OrbWindowState.Collapsed, 0.0, _orbRect);
            }
            else
            {
                stillActive = true;
            }
        }

        if (!stillActive)
        {
            DetachAnimationLoop();
        }
    }

    private void OnMorphSettled(OrbWindowState finalState)
    {
        _morphHostActive = false;
        if (finalState == OrbWindowState.Expanded)
        {
            Topmost = false;
            NativeBackdropHelper.SetTopmost(_hwnd, false);
            ShowInTaskbar = true;
            NativeBackdropHelper.SetToolWindowStyle(_hwnd, false);
            AppWindowChrome.ResizeBorderThickness = new Thickness(6);
            NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _expandedRect, topmost: false, activate: true);
            ApplyRegionForCurrentState(_expandedRect, 1.0);
            SendMorphFrame(OrbWindowState.Expanded, 1.0, _expandedRect);
            Activate();
            _settings.LastMode = "Expanded";
            PersistCurrentState();
        }
        else if (finalState == OrbWindowState.Collapsed)
        {
            Topmost = true;
            NativeBackdropHelper.SetTopmost(_hwnd, true);
            ShowInTaskbar = false;
            NativeBackdropHelper.SetToolWindowStyle(_hwnd, true);
            AppWindowChrome.ResizeBorderThickness = new Thickness(0);
            NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _orbRect, topmost: true, activate: false);
            ApplyRegionForCurrentState(_orbRect, 0.0);
            SendMorphFrame(OrbWindowState.Collapsed, 0.0, _orbRect);
            _settings.LastMode = "Collapsed";
            PersistCurrentState();
        }
    }

    private void ApplyClippedMorphRegion(PixelRect visibleScreenRect, double progress)
    {
        if (_hwnd == IntPtr.Zero || visibleScreenRect.Width <= 0 || visibleScreenRect.Height <= 0) return;

        int relX = visibleScreenRect.X - _morphHostRect.X;
        int relY = visibleScreenRect.Y - _morphHostRect.Y;

        double dpiScale = NativeBackdropHelper.GetDpiScaleForPoint(new PixelPoint(visibleScreenRect.CenterX, visibleScreenRect.CenterY));
        // Interpolate cleanly from orb circle radius (32 DIP) -> expanded window radius (28 DIP)
        double orbRadiusPx = _orbRect.Width / 2.0;
        double expandedRadiusPx = OrbGeometry.DipToPhysical(OrbGeometry.DefaultExpandedRadiusDip, dpiScale);
        double radiusPx = OrbGeometry.InterpolateCornerRadius(orbRadiusPx, expandedRadiusPx, progress);

        bool isCircle = progress <= 0.02 && Math.Abs(visibleScreenRect.Width - visibleScreenRect.Height) <= 2;
        NativeBackdropHelper.ApplyRoundedRegionAt(_hwnd, relX, relY, visibleScreenRect.Width, visibleScreenRect.Height, (int)Math.Round(radiusPx), isCircle);
    }

    private void ApplyRegionForCurrentState(PixelRect rect, double progress)
    {
        if (_hwnd == IntPtr.Zero || rect.Width <= 0 || rect.Height <= 0) return;

        if (WindowState == WindowState.Maximized && progress >= 0.99)
        {
            NativeBackdropHelper.ClearWindowRegion(_hwnd);
            return;
        }

        double dpiScale = NativeBackdropHelper.GetDpiScaleForPoint(new PixelPoint(rect.CenterX, rect.CenterY));
        double orbRadiusPx = _orbRect.Width / 2.0;
        double expandedRadiusPx = OrbGeometry.DipToPhysical(OrbGeometry.DefaultExpandedRadiusDip, dpiScale);
        double radiusPx = OrbGeometry.InterpolateCornerRadius(orbRadiusPx, expandedRadiusPx, progress);

        bool isCircle = progress <= 0.02 && Math.Abs(rect.Width - rect.Height) <= 2;
        NativeBackdropHelper.ApplyRoundedRegion(_hwnd, rect.Width, rect.Height, (int)Math.Round(radiusPx), isCircle);
        _lastOrbRegionSize = isCircle ? rect.Width : 0;
    }

    private void SendMorphFrame(OrbWindowState state, double progress, PixelRect currentRect)
    {
        if (!_webViewReady) return;

        double scale = _orbMonitor.DpiScale > 0.25 ? _orbMonitor.DpiScale : 1.0;
        double shellX = _morphHostActive ? OrbGeometry.PhysicalToDip(currentRect.X - _morphHostRect.X, scale) : 0.0;
        double shellY = _morphHostActive ? OrbGeometry.PhysicalToDip(currentRect.Y - _morphHostRect.Y, scale) : 0.0;
        double orbRelX = _morphHostActive ? OrbGeometry.PhysicalToDip(_orbRect.X - _morphHostRect.X, scale) : 0.0;
        double orbRelY = _morphHostActive ? OrbGeometry.PhysicalToDip(_orbRect.Y - _morphHostRect.Y, scale) : 0.0;
        double curW = OrbGeometry.PhysicalToDip(currentRect.Width, scale);
        double curH = OrbGeometry.PhysicalToDip(currentRect.Height, scale);
        double expW = OrbGeometry.PhysicalToDip(_expandedRect.Width, scale);
        double expH = OrbGeometry.PhysicalToDip(_expandedRect.Height, scale);
        double radiusDip = OrbGeometry.InterpolateCornerRadius(OrbGeometry.DefaultOrbDipSize / 2.0, OrbGeometry.DefaultExpandedRadiusDip, progress);

        SendIpcResponse(new
        {
            type = "morph_frame",
            state = state.ToString().ToLowerInvariant(),
            progress,
            radiusPx = Math.Round(radiusDip, 2),
            shellX = Math.Round(shellX, 1),
            shellY = Math.Round(shellY, 1),
            orbRelX = Math.Round(orbRelX, 1),
            orbRelY = Math.Round(orbRelY, 1),
            width = Math.Round(curW, 1),
            height = Math.Round(curH, 1),
            expandedWidth = Math.Round(expW, 1),
            expandedHeight = Math.Round(expH, 1),
            reducedMotion = _appearance.ReducedMotion,
            edge = _orbEdge.ToString().ToLowerInvariant()
        });
    }

    private void HandleOrbDragStart()
    {
        if (_stateMachine.State != OrbWindowState.Collapsed || _stateMachine.IsAnimating)
            return;

        _isSnappingOrb = false;
        _isDraggingOrb = true;

        var cursor = NativeBackdropHelper.GetCursorPositionPhysical();
        var winRect = NativeBackdropHelper.GetWindowBoundsPhysical(_hwnd);
        if (winRect.Width > 0 && winRect.Height > 0)
        {
            _orbRect = winRect;
            _dragGrabFracX = Math.Clamp((double)(cursor.X - winRect.X) / winRect.Width, 0.1, 0.9);
            _dragGrabFracY = Math.Clamp((double)(cursor.Y - winRect.Y) / winRect.Height, 0.1, 0.9);
        }
        else
        {
            _dragGrabFracX = 0.5;
            _dragGrabFracY = 0.5;
        }
    }

    private void HandleOrbDragMove()
    {
        if (!_isDraggingOrb || _stateMachine.State != OrbWindowState.Collapsed)
            return;

        var cursor = NativeBackdropHelper.GetCursorPositionPhysical();
        var monitors = NativeBackdropHelper.GetAllMonitors();
        var mon = OrbGeometry.FindMonitorForPoint(cursor, monitors);
        int orbPx = OrbGeometry.ComputeOrbPhysicalSize(OrbGeometry.DefaultOrbDipSize, mon.DpiScale);

        int newX = cursor.X - (int)Math.Round(_dragGrabFracX * orbPx);
        int newY = cursor.Y - (int)Math.Round(_dragGrabFracY * orbPx);
        _orbRect = new PixelRect(newX, newY, orbPx, orbPx);
        _orbMonitor = mon;

        NativeBackdropHelper.SetWindowBoundsPhysical(_hwnd, _orbRect, topmost: true, activate: false);

        if (_lastOrbRegionSize != orbPx)
        {
            ApplyRegionForCurrentState(_orbRect, 0.0);
        }
    }

    private void HandleOrbDragEnd()
    {
        if (!_isDraggingOrb)
            return;

        _isDraggingOrb = false;

        var monitors = NativeBackdropHelper.GetAllMonitors();
        var mon = OrbGeometry.FindMonitorForPoint(new PixelPoint(_orbRect.CenterX, _orbRect.CenterY), monitors);
        int orbPx = OrbGeometry.ComputeOrbPhysicalSize(OrbGeometry.DefaultOrbDipSize, mon.DpiScale);
        int marginPx = OrbGeometry.DipToPhysical(OrbGeometry.DefaultEdgeMarginDip, mon.DpiScale);

        var currentSized = new PixelRect(_orbRect.X, _orbRect.Y, orbPx, orbPx);
        var (snapped, edge) = OrbGeometry.SnapOrbToNearestEdge(currentSized, mon.WorkArea, marginPx);

        _orbMonitor = mon;
        _orbEdge = edge;
        _snapStartRect = currentSized;
        _snapTargetRect = snapped;
        _snapStartMs = _clock.Elapsed.TotalMilliseconds;
        _isSnappingOrb = true;

        AttachAnimationLoop();
    }

    private void PersistCurrentState()
    {
        _settings.HasSavedPosition = true;
        _settings.OrbCenterX = _orbRect.CenterX;
        _settings.OrbCenterY = _orbRect.CenterY;
        _settings.OrbEdge = _orbEdge;
        _settings.MonitorDeviceName = _orbMonitor.DeviceName;
        var (fx, fy) = OrbGeometry.ComputeAnchorFractions(_orbRect, _orbMonitor.WorkArea);
        _settings.AnchorFractionX = fx;
        _settings.AnchorFractionY = fy;
        _settings.LastMode = _stateMachine.State is OrbWindowState.Expanded or OrbWindowState.Expanding
            ? "Expanded"
            : "Collapsed";

        OrbSettingsStore.Save(_settings);
    }

    // =========================================================
    // Embedded Resource & IPC Handlers
    // =========================================================

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

        if (string.Equals(path, "app_icon.ico", StringComparison.OrdinalIgnoreCase))
        {
            string rootIcon = Path.Combine(Directory.GetCurrentDirectory(), "app_icon.ico");
            if (File.Exists(rootIcon)) return File.OpenRead(rootIcon);
            string baseIcon = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "app_icon.ico");
            if (File.Exists(baseIcon)) return File.OpenRead(baseIcon);
        }

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
                // --- Floating Orb & Window Controls ---
                case "orb_expand":
                    StartExpand();
                    break;

                case "orb_collapse":
                    StartCollapse();
                    break;

                case "orb_drag_start":
                    HandleOrbDragStart();
                    break;

                case "orb_drag_move":
                    HandleOrbDragMove();
                    break;

                case "orb_drag_end":
                    HandleOrbDragEnd();
                    break;

                case "window_drag_start":
                    if (_stateMachine.State == OrbWindowState.Expanded && !_stateMachine.IsAnimating)
                    {
                        NativeBackdropHelper.BeginNativeWindowDrag(_hwnd);
                    }
                    break;

                case "window_toggle_maximize":
                    if (_stateMachine.State == OrbWindowState.Expanded && !_stateMachine.IsAnimating)
                    {
                        WindowState = WindowState == WindowState.Maximized ? WindowState.Normal : WindowState.Maximized;
                    }
                    break;

                case "window_close":
                    Close();
                    break;

                case "orb_file_drop":
                    HandleFileDrop(e, root);
                    break;

                case "get_orb_settings":
                    SendIpcResponse(new
                    {
                        id,
                        action = "orb_settings_result",
                        success = true,
                        settings = new
                        {
                            launchMode = _settings.LaunchMode.ToString(),
                            theme = _settings.Theme,
                            orbEdge = _orbEdge.ToString(),
                            state = _stateMachine.State.ToString().ToLowerInvariant()
                        },
                        appearance = new
                        {
                            reducedMotion = _appearance.ReducedMotion,
                            highContrast = _appearance.HighContrast,
                            transparencyEnabled = _appearance.TransparencyEnabled
                        }
                    });
                    break;

                case "update_orb_settings":
                    HandleUpdateOrbSettings(id, root);
                    break;

                case "task_progress_update":
                    HandleTaskProgressUpdate(root);
                    break;

                // --- Original NUP Extractor IPC Actions (unchanged contract) ---
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

                case "inspect_pack_source":
                    HandleInspectPackSource(id, root);
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

    private void HandleFileDrop(CoreWebView2WebMessageReceivedEventArgs e, JsonElement root)
    {
        string target = GetString(root, "target") ?? "orb";
        string? droppedNupPath = null;
        string? droppedFolderPath = null;

        try
        {
            var objects = e.AdditionalObjects;
            if (objects != null)
            {
                foreach (var obj in objects)
                {
                    if (obj is CoreWebView2File file && !string.IsNullOrWhiteSpace(file.Path))
                    {
                        if (File.Exists(file.Path) && file.Path.EndsWith(".nup", StringComparison.OrdinalIgnoreCase))
                        {
                            droppedNupPath = file.Path;
                            break;
                        }
                        if (Directory.Exists(file.Path) && droppedFolderPath == null)
                        {
                            droppedFolderPath = file.Path;
                        }
                    }
                }
            }
        }
        catch { }

        if (!string.IsNullOrWhiteSpace(droppedNupPath))
        {
            if (_stateMachine.State is OrbWindowState.Collapsed or OrbWindowState.Collapsing)
            {
                StartExpand();
            }
            SendIpcResponse(new { type = "load_dropped_file", filePath = droppedNupPath });
        }
        else if (!string.IsNullOrWhiteSpace(droppedFolderPath) && target == "pack")
        {
            SendIpcResponse(new { type = "load_dropped_pack_folder", folderPath = droppedFolderPath });
        }
    }

    private void HandleUpdateOrbSettings(string id, JsonElement root)
    {
        string? launchModeStr = GetString(root, "launchMode");
        if (!string.IsNullOrWhiteSpace(launchModeStr) &&
            Enum.TryParse<OrbLaunchMode>(launchModeStr, ignoreCase: true, out var parsedMode))
        {
            _settings.LaunchMode = parsedMode;
        }

        string? theme = GetString(root, "theme");
        if (!string.IsNullOrWhiteSpace(theme))
        {
            _settings.Theme = string.Equals(theme, "light", StringComparison.OrdinalIgnoreCase) ? "light" : "dark";
            ApplyNativeBackdrop();
            PushSystemAppearance();
        }

        OrbSettingsStore.Save(_settings);

        SendIpcResponse(new
        {
            id,
            action = "orb_settings_result",
            success = true,
            settings = new
            {
                launchMode = _settings.LaunchMode.ToString(),
                theme = _settings.Theme,
                orbEdge = _orbEdge.ToString(),
                state = _stateMachine.State.ToString().ToLowerInvariant()
            }
        });
    }

    private void HandleTaskProgressUpdate(JsonElement root)
    {
        string task = GetString(root, "task") ?? "";
        string state = GetString(root, "state") ?? "idle";
        int pct = GetInt(root, "percent") ?? 0;

        if (_trayIcon == null) return;

        if (state == "running")
        {
            string label = task switch
            {
                "verify" => "Verifying",
                "pack" => "Packing",
                _ => "Extracting"
            };
            _trayIcon.UpdateTooltip($"NUP Extractor — {label} ({pct}%)");
        }
        else if (state == "done")
        {
            _trayIcon.UpdateTooltip("NUP Extractor — Task Complete");
        }
        else
        {
            _trayIcon.UpdateTooltip("NUP Extractor — Floating Orb");
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
            _trayIcon?.UpdateTooltip($"NUP Extractor — Extracting {(int)Math.Round(info.Percent)}%");
            SendIpcResponse(new
            {
                type = "extract_progress",
                info
            });
        });

        try
        {
            var sw = Stopwatch.StartNew();
            await Task.Run(() => NupEngine.Extract(filePath, destDir, mode, pkgIdx, subtreePath, progress, token), token);
            sw.Stop();

            if (openWhenDone)
            {
                NupEngine.OpenFolder(destDir);
            }

            _trayIcon?.UpdateTooltip("NUP Extractor — Extraction Complete");
            SendIpcResponse(new
            {
                type = "extract_done",
                success = true,
                destDir,
                elapsedSeconds = sw.Elapsed.TotalSeconds
            });
        }
        catch (OperationCanceledException)
        {
            _trayIcon?.UpdateTooltip("NUP Extractor — Floating Orb");
            SendIpcResponse(new
            {
                type = "extract_error",
                error = "Extraction cancelled by user."
            });
        }
        catch (Exception ex)
        {
            _trayIcon?.UpdateTooltip("NUP Extractor — Floating Orb");
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
                _trayIcon?.UpdateTooltip("NUP Extractor — Verification Complete");
                SendIpcResponse(new
                {
                    type = "verify_done",
                    info
                });
            }
            else
            {
                _trayIcon?.UpdateTooltip($"NUP Extractor — Verifying {(int)Math.Round(info.Percent)}%");
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
            _trayIcon?.UpdateTooltip("NUP Extractor — Floating Orb");
            SendIpcResponse(new
            {
                type = "verify_error",
                error = "Verification cancelled."
            });
        }
        catch (Exception ex)
        {
            _trayIcon?.UpdateTooltip("NUP Extractor — Floating Orb");
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

    private void HandleInspectPackSource(string id, JsonElement root)
    {
        string? sourceDir = GetString(root, "sourceDir", "source_dir");
        if (!string.IsNullOrWhiteSpace(sourceDir) && Directory.Exists(sourceDir))
        {
            var pkgs = NupWriter.DetectPackageSummary(sourceDir);
            SendIpcResponse(new
            {
                id,
                action = "inspect_pack_source_result",
                success = true,
                packages = pkgs
            });
        }
        else
        {
            SendIpcResponse(new
            {
                id,
                action = "inspect_pack_source_result",
                success = false,
                packages = Array.Empty<object>()
            });
        }
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
            _trayIcon?.UpdateTooltip($"NUP Extractor — Packing {(int)Math.Round(info.Percent)}%");
            SendIpcResponse(new { type = "pack_progress", info });
        });

        Task.Run(() =>
        {
            try
            {
                var sw = Stopwatch.StartNew();
                NupEngine.Pack(options, progress, ct);
                sw.Stop();
                _trayIcon?.UpdateTooltip("NUP Extractor — Packaging Complete");
                SendIpcResponse(new
                {
                    type = "pack_done",
                    outputPath = options.OutputFilePath,
                    sourceDir = options.SourceDirectory,
                    elapsedSeconds = sw.Elapsed.TotalSeconds
                });
            }
            catch (OperationCanceledException)
            {
                _trayIcon?.UpdateTooltip("NUP Extractor — Floating Orb");
                SendIpcResponse(new { type = "pack_error", error = "Packaging cancelled by user." });
            }
            catch (Exception ex)
            {
                _trayIcon?.UpdateTooltip("NUP Extractor — Floating Orb");
                SendIpcResponse(new { type = "pack_error", error = ex.Message });
            }
        }, ct);
    }

    private void SendIpcResponse(object payload)
    {
        if (Dispatcher.CheckAccess())
        {
            if (WebView.CoreWebView2 != null)
            {
                string json = JsonSerializer.Serialize(payload, JsonOptions);
                WebView.CoreWebView2.PostWebMessageAsJson(json);
            }
            return;
        }

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
