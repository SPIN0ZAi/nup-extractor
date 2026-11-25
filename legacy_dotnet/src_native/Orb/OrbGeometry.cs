using System;
using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace NupExtractorApp.Orb;

[JsonConverter(typeof(JsonStringEnumConverter))]
public enum ScreenEdge
{
    Left,
    Right,
    Top,
    Bottom
}

[JsonConverter(typeof(JsonStringEnumConverter))]
public enum OrbLaunchMode
{
    Collapsed,
    Expanded,
    RememberLast
}

public readonly record struct PixelPoint(int X, int Y);

public readonly record struct PixelRect(int X, int Y, int Width, int Height)
{
    public int Left => X;
    public int Top => Y;
    public int Right => X + Width;
    public int Bottom => Y + Height;
    public int CenterX => X + (Width / 2);
    public int CenterY => Y + (Height / 2);

    public bool Contains(PixelPoint p) =>
        p.X >= Left && p.X < Right && p.Y >= Top && p.Y < Bottom;
}

public readonly record struct DipSize(double Width, double Height);

public readonly record struct MonitorWorkArea(
    string DeviceName,
    PixelRect Bounds,
    PixelRect WorkArea,
    double DpiScale,
    bool IsPrimary);

public static class OrbGeometry
{
    public const double DefaultOrbDipSize = 64.0;
    public const double DefaultExpandedRadiusDip = 28.0;
    public const double DefaultEdgeMarginDip = 14.0;
    public const double DefaultExpandedMarginDip = 18.0;
    public const double DefaultExpandedWidthDip = 1240.0;
    public const double DefaultExpandedHeightDip = 820.0;
    public const double MinExpandedWidthDip = 760.0;
    public const double MinExpandedHeightDip = 520.0;

    public static int DipToPhysical(double dips, double dpiScale)
    {
        double safeScale = dpiScale > 0.25 ? dpiScale : 1.0;
        return Math.Max(1, (int)Math.Round(dips * safeScale, MidpointRounding.AwayFromZero));
    }

    public static double PhysicalToDip(double pixels, double dpiScale)
    {
        double safeScale = dpiScale > 0.25 ? dpiScale : 1.0;
        return pixels / safeScale;
    }

    public static int ComputeOrbPhysicalSize(double orbDipSize, double dpiScale) =>
        DipToPhysical(orbDipSize > 0 ? orbDipSize : DefaultOrbDipSize, dpiScale);

    public static MonitorWorkArea FindMonitorForPoint(PixelPoint point, IReadOnlyList<MonitorWorkArea> monitors)
    {
        if (monitors == null || monitors.Count == 0)
        {
            return new MonitorWorkArea("DEFAULT", new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 0, 1920, 1040), 1.0, true);
        }

        // 1. Exact containment inside monitor bounds
        foreach (var m in monitors)
        {
            if (m.Bounds.Contains(point))
                return m;
        }

        // 2. Nearest monitor by squared distance to monitor bounds
        MonitorWorkArea nearest = monitors[0];
        double bestDistSq = double.MaxValue;
        foreach (var m in monitors)
        {
            int cx = Math.Clamp(point.X, m.Bounds.Left, m.Bounds.Right - 1);
            int cy = Math.Clamp(point.Y, m.Bounds.Top, m.Bounds.Bottom - 1);
            double dx = point.X - cx;
            double dy = point.Y - cy;
            double d2 = dx * dx + dy * dy;
            if (d2 < bestDistSq)
            {
                bestDistSq = d2;
                nearest = m;
            }
        }
        return nearest;
    }

    public static PixelRect ClampOrbToWorkArea(PixelRect orbRect, PixelRect workArea, int edgeMarginPx)
    {
        int w = Math.Min(orbRect.Width, Math.Max(1, workArea.Width));
        int h = Math.Min(orbRect.Height, Math.Max(1, workArea.Height));

        int effMarginX = Math.Min(Math.Max(0, edgeMarginPx), Math.Max(0, (workArea.Width - w) / 2));
        int effMarginY = Math.Min(Math.Max(0, edgeMarginPx), Math.Max(0, (workArea.Height - h) / 2));

        int minX = workArea.Left + effMarginX;
        int maxX = workArea.Right - effMarginX - w;
        int minY = workArea.Top + effMarginY;
        int maxY = workArea.Bottom - effMarginY - h;

        int x = Math.Clamp(orbRect.X, minX, Math.Max(minX, maxX));
        int y = Math.Clamp(orbRect.Y, minY, Math.Max(minY, maxY));

        return new PixelRect(x, y, w, h);
    }

    public static (PixelRect SnappedRect, ScreenEdge Edge) SnapOrbToNearestEdge(
        PixelRect orbRect,
        PixelRect workArea,
        int edgeMarginPx)
    {
        var clamped = ClampOrbToWorkArea(orbRect, workArea, edgeMarginPx);
        int effMarginX = Math.Min(Math.Max(0, edgeMarginPx), Math.Max(0, (workArea.Width - clamped.Width) / 2));
        int effMarginY = Math.Min(Math.Max(0, edgeMarginPx), Math.Max(0, (workArea.Height - clamped.Height) / 2));

        int minX = workArea.Left + effMarginX;
        int maxX = Math.Max(minX, workArea.Right - effMarginX - clamped.Width);
        int minY = workArea.Top + effMarginY;
        int maxY = Math.Max(minY, workArea.Bottom - effMarginY - clamped.Height);

        int distLeft = Math.Abs(clamped.X - minX);
        int distRight = Math.Abs(maxX - clamped.X);
        int distTop = Math.Abs(clamped.Y - minY);
        int distBottom = Math.Abs(maxY - clamped.Y);

        // Prefer Right on exact tie, then Left, then Top, then Bottom (AssistiveTouch feel)
        ScreenEdge bestEdge = ScreenEdge.Right;
        int bestDist = distRight;

        if (distLeft < bestDist)
        {
            bestDist = distLeft;
            bestEdge = ScreenEdge.Left;
        }
        if (distTop < bestDist)
        {
            bestDist = distTop;
            bestEdge = ScreenEdge.Top;
        }
        if (distBottom < bestDist)
        {
            bestEdge = ScreenEdge.Bottom;
        }

        PixelRect snapped = bestEdge switch
        {
            ScreenEdge.Left => new PixelRect(minX, clamped.Y, clamped.Width, clamped.Height),
            ScreenEdge.Right => new PixelRect(maxX, clamped.Y, clamped.Width, clamped.Height),
            ScreenEdge.Top => new PixelRect(clamped.X, minY, clamped.Width, clamped.Height),
            ScreenEdge.Bottom => new PixelRect(clamped.X, maxY, clamped.Width, clamped.Height),
            _ => new PixelRect(maxX, clamped.Y, clamped.Width, clamped.Height)
        };

        return (snapped, bestEdge);
    }

    public static PixelRect ComputeExpandedRectNearOrb(
        PixelRect orbRect,
        DipSize desiredExpandedDip,
        MonitorWorkArea monitor,
        double marginDip = DefaultExpandedMarginDip)
    {
        var work = monitor.WorkArea;
        int marginPx = DipToPhysical(marginDip, monitor.DpiScale);
        int desiredW = DipToPhysical(desiredExpandedDip.Width, monitor.DpiScale);
        int desiredH = DipToPhysical(desiredExpandedDip.Height, monitor.DpiScale);

        int maxAvailW = Math.Max(200, work.Width - (marginPx * 2));
        int maxAvailH = Math.Max(200, work.Height - (marginPx * 2));

        int w = Math.Clamp(desiredW, Math.Min(320, maxAvailW), maxAvailW);
        int h = Math.Clamp(desiredH, Math.Min(240, maxAvailH), maxAvailH);

        int minX = work.Left + marginPx;
        int maxX = Math.Max(minX, work.Right - marginPx - w);
        int minY = work.Top + marginPx;
        int maxY = Math.Max(minY, work.Bottom - marginPx - h);

        double relX = work.Width > 0 ? (double)(orbRect.CenterX - work.Left) / work.Width : 0.5;
        double relY = work.Height > 0 ? (double)(orbRect.CenterY - work.Top) / work.Height : 0.5;

        int targetX;
        if (relX < 0.35)
        {
            // Orb is on the left side: expand rightward from the orb's left edge
            targetX = orbRect.Left;
        }
        else if (relX > 0.65)
        {
            // Orb is on the right side: expand leftward from the orb's right edge
            targetX = orbRect.Right - w;
        }
        else
        {
            targetX = orbRect.CenterX - (w / 2);
        }

        int targetY;
        if (relY < 0.30)
        {
            targetY = orbRect.Top;
        }
        else if (relY > 0.70)
        {
            targetY = orbRect.Bottom - h;
        }
        else
        {
            targetY = orbRect.CenterY - (h / 2);
        }

        int clampedX = Math.Clamp(targetX, minX, maxX);
        int clampedY = Math.Clamp(targetY, minY, maxY);

        return new PixelRect(clampedX, clampedY, w, h);
    }

    public static (PixelRect OrbRect, MonitorWorkArea Monitor, ScreenEdge Edge) ResolveInitialOrbPosition(
        OrbSettings settings,
        IReadOnlyList<MonitorWorkArea> monitors,
        double orbDipSize = DefaultOrbDipSize,
        double edgeMarginDip = DefaultEdgeMarginDip)
    {
        MonitorWorkArea primary = monitors.Count > 0 ? monitors[0] : new MonitorWorkArea(
            "DEFAULT", new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 0, 1920, 1040), 1.0, true);
        foreach (var m in monitors)
        {
            if (m.IsPrimary)
            {
                primary = m;
                break;
            }
        }

        MonitorWorkArea chosen = primary;
        bool matchedSavedMonitor = false;

        if (!string.IsNullOrWhiteSpace(settings.MonitorDeviceName))
        {
            foreach (var m in monitors)
            {
                if (string.Equals(m.DeviceName, settings.MonitorDeviceName, StringComparison.OrdinalIgnoreCase))
                {
                    chosen = m;
                    matchedSavedMonitor = true;
                    break;
                }
            }
        }

        if (!matchedSavedMonitor && settings.HasSavedPosition)
        {
            var pt = new PixelPoint(settings.OrbCenterX, settings.OrbCenterY);
            foreach (var m in monitors)
            {
                if (m.Bounds.Contains(pt))
                {
                    chosen = m;
                    matchedSavedMonitor = true;
                    break;
                }
            }
        }

        int orbPx = ComputeOrbPhysicalSize(orbDipSize, chosen.DpiScale);
        int marginPx = DipToPhysical(edgeMarginDip, chosen.DpiScale);
        var work = chosen.WorkArea;

        PixelRect candidate;
        if (settings.HasSavedPosition && matchedSavedMonitor)
        {
            candidate = new PixelRect(
                settings.OrbCenterX - (orbPx / 2),
                settings.OrbCenterY - (orbPx / 2),
                orbPx,
                orbPx);
        }
        else if (settings.HasSavedPosition)
        {
            // Original monitor is gone; map proportionally onto primary monitor respecting saved edge
            double fracX = Math.Clamp(settings.AnchorFractionX, 0.0, 1.0);
            double fracY = Math.Clamp(settings.AnchorFractionY, 0.0, 1.0);
            int cx = work.Left + (int)Math.Round(fracX * work.Width);
            int cy = work.Top + (int)Math.Round(fracY * work.Height);
            candidate = settings.OrbEdge switch
            {
                ScreenEdge.Left => new PixelRect(work.Left + marginPx, cy - (orbPx / 2), orbPx, orbPx),
                ScreenEdge.Right => new PixelRect(work.Right - marginPx - orbPx, cy - (orbPx / 2), orbPx, orbPx),
                ScreenEdge.Top => new PixelRect(cx - (orbPx / 2), work.Top + marginPx, orbPx, orbPx),
                ScreenEdge.Bottom => new PixelRect(cx - (orbPx / 2), work.Bottom - marginPx - orbPx, orbPx, orbPx),
                _ => new PixelRect(work.Right - marginPx - orbPx, cy - (orbPx / 2), orbPx, orbPx)
            };
        }
        else
        {
            // First launch default: right edge, ~38% down from top (AssistiveTouch feel)
            int defaultY = work.Top + (int)Math.Round(work.Height * 0.38) - (orbPx / 2);
            candidate = new PixelRect(work.Right - marginPx - orbPx, defaultY, orbPx, orbPx);
        }

        var (snapped, edge) = SnapOrbToNearestEdge(candidate, work, marginPx);
        return (snapped, chosen, edge);
    }

    public static (double FractionX, double FractionY) ComputeAnchorFractions(PixelRect orbRect, PixelRect workArea)
    {
        double fx = workArea.Width > 0 ? (double)(orbRect.CenterX - workArea.Left) / workArea.Width : 0.95;
        double fy = workArea.Height > 0 ? (double)(orbRect.CenterY - workArea.Top) / workArea.Height : 0.38;
        return (Math.Clamp(fx, 0.0, 1.0), Math.Clamp(fy, 0.0, 1.0));
    }

    public static PixelRect Union(PixelRect a, PixelRect b)
    {
        int left = Math.Min(a.Left, b.Left);
        int top = Math.Min(a.Top, b.Top);
        int right = Math.Max(a.Right, b.Right);
        int bottom = Math.Max(a.Bottom, b.Bottom);
        return new PixelRect(left, top, Math.Max(1, right - left), Math.Max(1, bottom - top));
    }

    public static PixelRect InterpolateRect(PixelRect from, PixelRect to, double t)
    {
        double clamped = Math.Clamp(t, 0.0, 1.0);
        int x = (int)Math.Round(from.X + ((to.X - from.X) * clamped));
        int y = (int)Math.Round(from.Y + ((to.Y - from.Y) * clamped));
        int w = Math.Max(1, (int)Math.Round(from.Width + ((to.Width - from.Width) * clamped)));
        int h = Math.Max(1, (int)Math.Round(from.Height + ((to.Height - from.Height) * clamped)));
        return new PixelRect(x, y, w, h);
    }

    public static double InterpolateCornerRadius(double orbRadiusPx, double expandedRadiusPx, double t)
    {
        double clamped = Math.Clamp(t, 0.0, 1.0);
        return orbRadiusPx + ((expandedRadiusPx - orbRadiusPx) * clamped);
    }
}
