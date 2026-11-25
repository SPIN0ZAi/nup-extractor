using System;

namespace NupExtractorApp.Orb;

public enum OrbWindowState
{
    Collapsed,
    Expanding,
    Expanded,
    Collapsing
}

public readonly record struct MorphSnapshot(
    OrbWindowState State,
    double Progress,
    bool IsAnimating,
    bool ReducedMotion);

/// <summary>
/// Pure, deterministic, interruptible state machine driving the Orb <-> Expanded morph.
/// Uses a critically damped spring integrator so mid-flight expand/collapse reversals
/// preserve position and velocity with zero jumps.
/// </summary>
public sealed class OrbStateMachine
{
    // Critically damped spring: omega = sqrt(480) ≈ 21.91 rad/s -> taut, crisp ~355 ms settle with no sluggish tail
    private const double SpringStiffness = 480.0;
    private const double SpringDamping = 43.81780460041329; // 2 * sqrt(480)
    private const double SettlePositionEpsilon = 0.006;
    private const double SettleVelocityEpsilon = 0.08;
    private const double ReducedMotionDurationMs = 140.0;
    private const double MaxStepSeconds = 0.05;

    private double _position; // 0.0 = Collapsed, 1.0 = Expanded
    private double _velocity;
    private double _target;
    private double _lastTickMs;
    private bool _hasLastTick;

    public OrbWindowState State { get; private set; }
    public double Progress => Math.Clamp(_position, 0.0, 1.0);
    public double Velocity => _velocity;
    public bool IsAnimating => State is OrbWindowState.Expanding or OrbWindowState.Collapsing;
    public bool ReducedMotion { get; set; }

    public OrbStateMachine(OrbWindowState initialState = OrbWindowState.Collapsed, bool reducedMotion = false)
    {
        ReducedMotion = reducedMotion;
        SetImmediateState(initialState);
    }

    public void SetImmediateState(OrbWindowState state)
    {
        if (state == OrbWindowState.Expanded || state == OrbWindowState.Expanding)
        {
            State = OrbWindowState.Expanded;
            _position = 1.0;
            _target = 1.0;
        }
        else
        {
            State = OrbWindowState.Collapsed;
            _position = 0.0;
            _target = 0.0;
        }
        _velocity = 0.0;
        _hasLastTick = false;
    }

    public bool RequestExpand(double nowMs)
    {
        if (State == OrbWindowState.Expanded || State == OrbWindowState.Expanding)
            return false;

        State = OrbWindowState.Expanding;
        _target = 1.0;
        _lastTickMs = nowMs;
        _hasLastTick = true;
        return true;
    }

    public bool RequestCollapse(double nowMs)
    {
        if (State == OrbWindowState.Collapsed || State == OrbWindowState.Collapsing)
            return false;

        State = OrbWindowState.Collapsing;
        _target = 0.0;
        _lastTickMs = nowMs;
        _hasLastTick = true;
        return true;
    }

    public bool Toggle(double nowMs)
    {
        return State is OrbWindowState.Collapsed or OrbWindowState.Collapsing
            ? RequestExpand(nowMs)
            : RequestCollapse(nowMs);
    }

    public MorphSnapshot Tick(double nowMs)
    {
        if (!IsAnimating)
        {
            return new MorphSnapshot(State, Progress, false, ReducedMotion);
        }

        if (!_hasLastTick)
        {
            _lastTickMs = nowMs;
            _hasLastTick = true;
            return new MorphSnapshot(State, Progress, true, ReducedMotion);
        }

        double dtMs = Math.Max(0.0, nowMs - _lastTickMs);
        _lastTickMs = nowMs;

        if (dtMs <= 0.0)
        {
            return new MorphSnapshot(State, Progress, true, ReducedMotion);
        }

        if (ReducedMotion)
        {
            double delta = dtMs / ReducedMotionDurationMs;
            if (_target > 0.5)
            {
                _position = Math.Min(1.0, _position + delta);
                if (_position >= 1.0)
                {
                    _position = 1.0;
                    _velocity = 0.0;
                    State = OrbWindowState.Expanded;
                }
            }
            else
            {
                _position = Math.Max(0.0, _position - delta);
                if (_position <= 0.0)
                {
                    _position = 0.0;
                    _velocity = 0.0;
                    State = OrbWindowState.Collapsed;
                }
            }
            return new MorphSnapshot(State, Progress, IsAnimating, true);
        }

        // Sub-stepped semi-implicit Euler for unconditional stability even on slow frames
        double remainingSec = Math.Min(dtMs / 1000.0, 0.25);
        while (remainingSec > 0.0)
        {
            double h = Math.Min(remainingSec, 0.008); // <= 8ms sub-steps
            double displacement = _position - _target;
            double accel = (-SpringStiffness * displacement) - (SpringDamping * _velocity);
            _velocity += accel * h;
            _position += _velocity * h;
            remainingSec -= h;
        }

        if (Math.Abs(_target - _position) <= SettlePositionEpsilon && Math.Abs(_velocity) <= SettleVelocityEpsilon)
        {
            _position = _target;
            _velocity = 0.0;
            State = _target > 0.5 ? OrbWindowState.Expanded : OrbWindowState.Collapsed;
        }

        return new MorphSnapshot(State, Progress, IsAnimating, false);
    }
}

/// <summary>
/// Pure click-vs-drag threshold detector for the floating orb.
/// </summary>
public sealed class OrbDragThreshold
{
    public const double DefaultThresholdPx = 4.0;

    private double _startX;
    private double _startY;
    private bool _isPointerDown;
    private bool _isDragging;

    public double ThresholdPx { get; }
    public bool IsPointerDown => _isPointerDown;
    public bool IsDragging => _isDragging;

    public OrbDragThreshold(double thresholdPx = DefaultThresholdPx)
    {
        ThresholdPx = thresholdPx > 0 ? thresholdPx : DefaultThresholdPx;
    }

    public void Begin(double x, double y)
    {
        _startX = x;
        _startY = y;
        _isPointerDown = true;
        _isDragging = false;
    }

    /// <summary>
    /// Updates pointer coordinates. Returns true on the exact frame dragging begins.
    /// </summary>
    public bool Update(double x, double y)
    {
        if (!_isPointerDown || _isDragging)
            return false;

        double dx = x - _startX;
        double dy = y - _startY;
        if ((dx * dx) + (dy * dy) >= ThresholdPx * ThresholdPx)
        {
            _isDragging = true;
            return true;
        }
        return false;
    }

    /// <summary>
    /// Ends pointer interaction. Returns true if the gesture was a click (never crossed drag threshold).
    /// </summary>
    public bool End()
    {
        bool wasClick = _isPointerDown && !_isDragging;
        _isPointerDown = false;
        _isDragging = false;
        return wasClick;
    }
}
