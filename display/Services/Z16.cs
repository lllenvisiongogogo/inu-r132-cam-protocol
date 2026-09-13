using System;

namespace InuR132.Display.Services;

/// <summary>Percentile / range summary of one Z16 depth frame.</summary>
public readonly record struct Z16Summary(
    ushort Min,
    ushort P2,
    ushort P50,
    ushort P98,
    ushort Max,
    double InvalidRatio);

/// <summary>Depth maths that mirror src/frame.rs.</summary>
public static class Z16
{
    /// <summary>
    /// Map a depth sample to a near-bright/far-dark grey level. 0 means "no
    /// measurement" and stays black. gamma &lt; 1 brightens the middle, which
    /// helps a scene with a near object and a far background.
    /// </summary>
    public static byte Level(ushort value, double near, double far, double gamma)
    {
        if (value == 0 || far <= near)
        {
            return 0;
        }

        var linear = Math.Clamp((value - near) / (far - near), 0.0, 1.0);
        var level = Math.Pow(1.0 - linear, gamma <= 0 ? 1.0 : gamma);
        return (byte)Math.Round(level * 255.0);
    }

    /// <summary>Percentiles and range from a 4096 bin histogram (16 mm bins).</summary>
    public static Z16Summary Summarize(byte[] payload)
    {
        const int bins = 4096;
        var histogram = new int[bins];
        long valid = 0;
        ushort min = ushort.MaxValue;
        ushort max = 0;

        for (var i = 0; i + 1 < payload.Length; i += 2)
        {
            var value = (ushort)(payload[i] | (payload[i + 1] << 8));
            if (value == 0)
            {
                continue;
            }

            histogram[value >> 4]++;
            valid++;
            min = Math.Min(min, value);
            max = Math.Max(max, value);
        }

        if (valid == 0)
        {
            return new Z16Summary(0, 0, 0, 0, 0, 1.0);
        }

        ushort Percentile(double fraction)
        {
            var target = (long)(valid * fraction);
            long accumulated = 0;
            for (var bin = 0; bin < bins; bin++)
            {
                accumulated += histogram[bin];
                if (accumulated >= target)
                {
                    return (ushort)Math.Min(((bin << 4) + 15), 65535);
                }
            }

            return max;
        }

        var total = Math.Max(payload.Length / 2.0, 1.0);
        return new Z16Summary(
            min,
            Percentile(0.02),
            Percentile(0.50),
            Percentile(0.98),
            max,
            1.0 - valid / total);
    }
}
