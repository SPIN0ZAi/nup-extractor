using System;
using System.Collections.Generic;
using System.IO;
using NupExtractorApp.Orb;
using Xunit;

namespace NupExtractorApp.Tests;

public class OrbTests
{
    [Fact]
    public void StateMachine_ExpandAndCollapse_SettlesWithinTargetWindow()
    {
        var sm = new OrbStateMachine(OrbWindowState.Collapsed, reducedMotion: false);
        Assert.Equal(OrbWindowState.Collapsed, sm.State);
        Assert.Equal(0.0, sm.Progress);

        Assert.True(sm.RequestExpand(0));
        Assert.Equal(OrbWindowState.Expanding, sm.State);

        double prev = 0.0;
        double settledMs = -1;
        for (double t = 16; t <= 800; t += 16)
        {
            var snap = sm.Tick(t);
            Assert.True(snap.Progress >= prev - 0.0001, "Critically damped expand should not regress");
            prev = snap.Progress;
            if (!snap.IsAnimating)
            {
                settledMs = t;
                break;
            }
        }

        Assert.Equal(OrbWindowState.Expanded, sm.State);
        Assert.Equal(1.0, sm.Progress);
        Assert.InRange(settledMs, 350, 450);

        // Now collapse back
        Assert.True(sm.RequestCollapse(1000));
        Assert.Equal(OrbWindowState.Collapsing, sm.State);

        double collapseSettledMs = -1;
        for (double t = 1016; t <= 1800; t += 16)
        {
            var snap = sm.Tick(t);
            if (!snap.IsAnimating)
            {
                collapseSettledMs = t - 1000;
                break;
            }
        }

        Assert.Equal(OrbWindowState.Collapsed, sm.State);
        Assert.Equal(0.0, sm.Progress);
        Assert.InRange(collapseSettledMs, 350, 450);
    }

    [Fact]
    public void StateMachine_InterruptMidExpand_ReversesSmoothlyWithoutJumping()
    {
        var sm = new OrbStateMachine(OrbWindowState.Collapsed, reducedMotion: false);
        sm.RequestExpand(0);

        MorphSnapshot mid = default;
        for (double t = 16; t <= 160; t += 16)
        {
            mid = sm.Tick(t);
        }

        Assert.Equal(OrbWindowState.Expanding, mid.State);
        Assert.InRange(mid.Progress, 0.25, 0.90);

        // Reverse mid-flight
        Assert.True(sm.RequestCollapse(160));
        var nextFrame = sm.Tick(176);

        // Must remain continuous with no sudden jump back to 1.0 or 0.0
        Assert.True(Math.Abs(nextFrame.Progress - mid.Progress) < 0.12,
            $"Unexpected jump on reversal: {mid.Progress} -> {nextFrame.Progress}");

        // Continue until settled at Collapsed
        for (double t = 192; t <= 1200; t += 16)
        {
            var snap = sm.Tick(t);
            if (!snap.IsAnimating) break;
        }

        Assert.Equal(OrbWindowState.Collapsed, sm.State);
        Assert.Equal(0.0, sm.Progress);
    }

    [Fact]
    public void StateMachine_RapidSpamming_AlwaysSettlesCleanly()
    {
        var sm = new OrbStateMachine(OrbWindowState.Collapsed, reducedMotion: false);
        double now = 0;

        for (int i = 0; i < 12; i++)
        {
            sm.Toggle(now);
            for (int f = 0; f < 4; f++)
            {
                now += 16;
                var snap = sm.Tick(now);
                Assert.False(double.IsNaN(snap.Progress));
                Assert.InRange(snap.Progress, 0.0, 1.0);
            }
        }

        // End on explicit Expand and let settle
        sm.RequestExpand(now);
        for (int f = 0; f < 80; f++)
        {
            now += 16;
            if (!sm.Tick(now).IsAnimating) break;
        }

        Assert.Equal(OrbWindowState.Expanded, sm.State);
        Assert.Equal(1.0, sm.Progress);
    }

    [Fact]
    public void StateMachine_ReducedMotion_CompletesQuickly()
    {
        var sm = new OrbStateMachine(OrbWindowState.Collapsed, reducedMotion: true);
        sm.RequestExpand(0);

        var half = sm.Tick(70);
        Assert.True(half.ReducedMotion);
        Assert.InRange(half.Progress, 0.45, 0.55);

        var done = sm.Tick(150);
        Assert.Equal(OrbWindowState.Expanded, done.State);
        Assert.Equal(1.0, done.Progress);
        Assert.False(done.IsAnimating);
    }

    [Fact]
    public void DragThreshold_DistinguishesClickFromDragAt4Px()
    {
        var tracker = new OrbDragThreshold(4.0);

        // Small jitter (< 4px) => click
        tracker.Begin(100, 100);
        Assert.False(tracker.Update(102, 102)); // hypot = 2.83 < 4
        Assert.False(tracker.IsDragging);
        Assert.True(tracker.End()); // click!

        // Movement >= 4px => drag, never click
        tracker.Begin(100, 100);
        Assert.False(tracker.Update(102, 101)); // hypot = 2.24
        Assert.True(tracker.Update(103, 103));  // hypot = 4.24 >= 4
        Assert.True(tracker.IsDragging);
        Assert.False(tracker.End()); // not a click!
    }

    [Theory]
    [InlineData(1.00, 64)]
    [InlineData(1.25, 80)]
    [InlineData(1.50, 96)]
    [InlineData(2.00, 128)]
    public void MixedDpi_OrbPhysicalSizeScalesAccurately(double dpiScale, int expectedPx)
    {
        Assert.Equal(expectedPx, OrbGeometry.ComputeOrbPhysicalSize(64.0, dpiScale));
        Assert.Equal(64.0, OrbGeometry.PhysicalToDip(expectedPx, dpiScale), precision: 3);
    }

    [Theory]
    [InlineData(200, 500, ScreenEdge.Left, 14)]
    [InlineData(1700, 500, ScreenEdge.Right, 1920 - 14 - 64)]
    [InlineData(960, 80, ScreenEdge.Top, 14)]
    [InlineData(960, 960, ScreenEdge.Bottom, 1032 - 14 - 64)]
    public void SnapOrbToNearestEdge_SnapsToClosestWorkAreaEdge(int x, int y, ScreenEdge expectedEdge, int expectedCoord)
    {
        var workArea = new PixelRect(0, 0, 1920, 1032);
        var orb = new PixelRect(x, y, 64, 64);

        var (snapped, edge) = OrbGeometry.SnapOrbToNearestEdge(orb, workArea, edgeMarginPx: 14);

        Assert.Equal(expectedEdge, edge);
        if (expectedEdge is ScreenEdge.Left or ScreenEdge.Right)
            Assert.Equal(expectedCoord, snapped.X);
        else
            Assert.Equal(expectedCoord, snapped.Y);

        Assert.True(snapped.Left >= workArea.Left);
        Assert.True(snapped.Right <= workArea.Right);
        Assert.True(snapped.Top >= workArea.Top);
        Assert.True(snapped.Bottom <= workArea.Bottom);
    }

    [Fact]
    public void SnapAndClamp_RespectsTaskbarOnEveryScreenEdge()
    {
        // Taskbar on Left (width 72px)
        var leftTb = new PixelRect(72, 0, 1848, 1080);
        var (snapL, edgeL) = OrbGeometry.SnapOrbToNearestEdge(new PixelRect(10, 400, 64, 64), leftTb, 14);
        Assert.Equal(ScreenEdge.Left, edgeL);
        Assert.Equal(72 + 14, snapL.X);

        // Taskbar on Top (height 48px)
        var topTb = new PixelRect(0, 48, 1920, 1032);
        var (snapT, edgeT) = OrbGeometry.SnapOrbToNearestEdge(new PixelRect(900, 5, 64, 64), topTb, 14);
        Assert.Equal(ScreenEdge.Top, edgeT);
        Assert.Equal(48 + 14, snapT.Y);

        // Taskbar on Right (width 72px)
        var rightTb = new PixelRect(0, 0, 1848, 1080);
        var (snapR, edgeR) = OrbGeometry.SnapOrbToNearestEdge(new PixelRect(1890, 400, 64, 64), rightTb, 14);
        Assert.Equal(ScreenEdge.Right, edgeR);
        Assert.Equal(1848 - 14 - 64, snapR.X);

        // Taskbar on Bottom (height 48px)
        var bottomTb = new PixelRect(0, 0, 1920, 1032);
        var (snapB, edgeB) = OrbGeometry.SnapOrbToNearestEdge(new PixelRect(900, 1060, 64, 64), bottomTb, 14);
        Assert.Equal(ScreenEdge.Bottom, edgeB);
        Assert.Equal(1032 - 14 - 64, snapB.Y);
    }

    [Fact]
    public void MultiMonitor_MixedDpiAndNegativeOffsets_ResolvesAndExpandsInsideWorkArea()
    {
        var monitors = new List<MonitorWorkArea>
        {
            new(@"\\.\DISPLAY1", new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 0, 1920, 1032), 1.0, true),
            new(@"\\.\DISPLAY2", new PixelRect(1920, -116, 2560, 1440), new PixelRect(1920, -116, 2560, 1380), 1.5, false),
            new(@"\\.\DISPLAY3", new PixelRect(-1366, 0, 1366, 768), new PixelRect(-1366, 0, 1366, 728), 1.25, false)
        };

        // Dragged onto high-DPI monitor 2 with negative Y offset
        var mon2 = OrbGeometry.FindMonitorForPoint(new PixelPoint(4400, 200), monitors);
        Assert.Equal(@"\\.\DISPLAY2", mon2.DeviceName);
        Assert.Equal(1.5, mon2.DpiScale);

        int orbPx = OrbGeometry.ComputeOrbPhysicalSize(64, mon2.DpiScale);
        Assert.Equal(96, orbPx);

        var orb2 = new PixelRect(4400, 100, orbPx, orbPx);
        var exp2 = OrbGeometry.ComputeExpandedRectNearOrb(orb2, new DipSize(1240, 820), mon2);

        Assert.True(exp2.Left >= mon2.WorkArea.Left);
        Assert.True(exp2.Right <= mon2.WorkArea.Right);
        Assert.True(exp2.Top >= mon2.WorkArea.Top);
        Assert.True(exp2.Bottom <= mon2.WorkArea.Bottom);

        // Small monitor 3 with negative X offset: expanded size must clamp cleanly inside work area
        var mon3 = OrbGeometry.FindMonitorForPoint(new PixelPoint(-1300, 300), monitors);
        Assert.Equal(@"\\.\DISPLAY3", mon3.DeviceName);
        var orb3 = new PixelRect(-1350, 300, 80, 80);
        var exp3 = OrbGeometry.ComputeExpandedRectNearOrb(orb3, new DipSize(1240, 820), mon3);

        Assert.True(exp3.Left >= mon3.WorkArea.Left);
        Assert.True(exp3.Right <= mon3.WorkArea.Right);
        Assert.True(exp3.Top >= mon3.WorkArea.Top);
        Assert.True(exp3.Bottom <= mon3.WorkArea.Bottom);
        Assert.True(exp3.Width <= mon3.WorkArea.Width);
        Assert.True(exp3.Height <= mon3.WorkArea.Height);
    }

    [Fact]
    public void ResolveInitialOrbPosition_ClampsToPrimaryWhenSavedMonitorDisconnected()
    {
        var saved = new OrbSettings
        {
            HasSavedPosition = true,
            OrbCenterX = 3700,
            OrbCenterY = 400,
            AnchorFractionX = 0.95,
            AnchorFractionY = 0.50,
            OrbEdge = ScreenEdge.Right,
            MonitorDeviceName = @"\\.\DISPLAY_DISCONNECTED"
        };

        var currentMonitors = new List<MonitorWorkArea>
        {
            new(@"\\.\DISPLAY1", new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 0, 1920, 1032), 1.25, true)
        };

        var (orbRect, monitor, edge) = OrbGeometry.ResolveInitialOrbPosition(saved, currentMonitors);

        Assert.Equal(@"\\.\DISPLAY1", monitor.DeviceName);
        Assert.Equal(80, orbRect.Width);
        Assert.Equal(ScreenEdge.Right, edge);
        Assert.True(orbRect.Left >= monitor.WorkArea.Left);
        Assert.True(orbRect.Right <= monitor.WorkArea.Right);
        Assert.True(orbRect.Top >= monitor.WorkArea.Top);
        Assert.True(orbRect.Bottom <= monitor.WorkArea.Bottom);
    }

    [Fact]
    public void SettingsStore_RoundTripAndCorruptFallback()
    {
        string tempDir = Path.Combine(Path.GetTempPath(), "nup_orb_settings_test_" + Guid.NewGuid().ToString("N"));
        string filePath = Path.Combine(tempDir, "orb-state.json");

        try
        {
            var original = new OrbSettings
            {
                HasSavedPosition = true,
                OrbCenterX = 1850,
                OrbCenterY = 420,
                AnchorFractionX = 0.96,
                AnchorFractionY = 0.41,
                OrbEdge = ScreenEdge.Right,
                MonitorDeviceName = @"\\.\DISPLAY1",
                LastMode = "Expanded",
                LaunchMode = OrbLaunchMode.RememberLast,
                ExpandedWidthDip = 1320,
                ExpandedHeightDip = 860,
                Theme = "light"
            };

            Assert.True(OrbSettingsStore.Save(original, filePath));
            Assert.True(File.Exists(filePath));

            var loaded = OrbSettingsStore.Load(filePath);
            Assert.True(loaded.HasSavedPosition);
            Assert.Equal(1850, loaded.OrbCenterX);
            Assert.Equal(420, loaded.OrbCenterY);
            Assert.Equal(ScreenEdge.Right, loaded.OrbEdge);
            Assert.Equal(@"\\.\DISPLAY1", loaded.MonitorDeviceName);
            Assert.Equal("Expanded", loaded.LastMode);
            Assert.Equal(OrbLaunchMode.RememberLast, loaded.LaunchMode);
            Assert.Equal(1320, loaded.ExpandedWidthDip);
            Assert.Equal(860, loaded.ExpandedHeightDip);
            Assert.Equal("light", loaded.Theme);

            // Verify startup mode resolution rules
            Assert.Equal(OrbWindowState.Expanded, loaded.ResolveStartupState(hasInitialFileArg: false));
            loaded.LaunchMode = OrbLaunchMode.Collapsed;
            Assert.Equal(OrbWindowState.Collapsed, loaded.ResolveStartupState(hasInitialFileArg: false));
            // Opening a .nup file directly always launches Expanded
            Assert.Equal(OrbWindowState.Expanded, loaded.ResolveStartupState(hasInitialFileArg: true));

            // Corrupt JSON falls back safely to defaults
            File.WriteAllText(filePath, "{ not valid json !!!");
            var fallback = OrbSettingsStore.Load(filePath);
            Assert.Equal(OrbLaunchMode.Collapsed, fallback.LaunchMode);
            Assert.Equal("dark", fallback.Theme);
            Assert.False(fallback.HasSavedPosition);
        }
        finally
        {
            try { if (Directory.Exists(tempDir)) Directory.Delete(tempDir, true); } catch { }
        }
    }
}
