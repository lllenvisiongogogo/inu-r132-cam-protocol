namespace InuR132.Display.Services;

/// <summary>
/// The depth display window shared between the UI controls and the conversion
/// thread. The UI writes it when a control changes; the conversion thread reads
/// it and, in auto mode, eases it towards each frame's percentiles.
/// </summary>
public sealed class DepthSettings
{
    private readonly object _lock = new();
    private double _near = 300;
    private double _far = 5000;
    private double _gamma = 1;
    private bool _auto;

    /// <summary>A manual edit turns auto range off.</summary>
    public void SetManual(double near, double far, double gamma)
    {
        lock (_lock)
        {
            _near = near;
            _far = far;
            _gamma = gamma;
            _auto = false;
        }
    }

    public void SetGamma(double gamma)
    {
        lock (_lock)
        {
            _gamma = gamma;
        }
    }

    public void SetAuto(bool auto)
    {
        lock (_lock)
        {
            _auto = auto;
        }
    }

    public (double Near, double Far, double Gamma, bool Auto) Snapshot()
    {
        lock (_lock)
        {
            return (_near, _far, _gamma, _auto);
        }
    }

    /// <summary>Ease the window towards the frame percentiles, no flicker.</summary>
    public (double Near, double Far) ApplyAuto(double p2, double p98)
    {
        lock (_lock)
        {
            if (!_auto)
            {
                return (_near, _far);
            }

            _near = (_near * 3 + p2) / 4;
            _far = (_far * 3 + System.Math.Max(p98, p2 + 1)) / 4;
            if (_far <= _near)
            {
                _far = _near + 1;
            }

            return (_near, _far);
        }
    }
}
