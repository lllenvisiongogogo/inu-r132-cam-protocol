using System;

namespace InuR132.Display.Services;

/// <summary>Millimetres at a point, and how many of the 3x3 samples were valid.</summary>
public readonly record struct DepthSample(int? Millimetres, int Valid)
{
    public static readonly DepthSample None = new(null, 0);

    /// <summary>`412mm`, `412mm (6/9)` or `无测量`.</summary>
    public string Text => Millimetres switch
    {
        null => "无测量",
        var mm when Valid < 9 => $"{mm}mm ({Valid}/9)",
        var mm => $"{mm}mm",
    };
}

/// <summary>
/// Reads millimetres out of a raw little endian Z16 depth frame. Both the
/// free-floating probe and the centre of every detected box go through here.
/// </summary>
public static class DepthSampler
{
    /// <summary>
    /// Average the valid values of a 3x3 window at a normalised point of the
    /// depth image. A single pixel is noisy and depth has holes, so "no valid
    /// sample" is reported rather than a number made of zeros.
    /// </summary>
    public static DepthSample At(byte[] raw, int width, int height, double u, double v)
    {
        if (raw.Length == 0 || width <= 0 || height <= 0
            || !double.IsFinite(u) || !double.IsFinite(v))
        {
            return DepthSample.None;
        }

        var centreX = (int)Math.Clamp(u * width, 0, width - 1);
        var centreY = (int)Math.Clamp(v * height, 0, height - 1);
        long sum = 0;
        var count = 0;
        for (var dy = -1; dy <= 1; dy++)
        {
            for (var dx = -1; dx <= 1; dx++)
            {
                var px = centreX + dx;
                var py = centreY + dy;
                if (px < 0 || py < 0 || px >= width || py >= height)
                {
                    continue;
                }

                var index = (py * width + px) * 2;
                if (index + 1 >= raw.Length)
                {
                    continue;
                }

                var value = (ushort)(raw[index] | (raw[index + 1] << 8));
                if (value == 0)
                {
                    continue;
                }

                sum += value;
                count++;
            }
        }

        return count == 0 ? DepthSample.None : new DepthSample((int)(sum / count), count);
    }

    /// <summary>
    /// Distance at the centre of a box, with the box given in the coordinates of
    /// the frame the detector saw. Registered depth is aligned to the colour
    /// camera, so normalising both puts the box centre on the same point of the
    /// depth image even if the two frames differ in size.
    /// </summary>
    public static DepthSample BoxCentre(
        byte[] raw, int width, int height,
        double x1, double y1, double x2, double y2,
        int frameWidth, int frameHeight)
    {
        if (frameWidth <= 0 || frameHeight <= 0)
        {
            return DepthSample.None;
        }

        return At(raw, width, height, (x1 + x2) / 2 / frameWidth, (y1 + y2) / 2 / frameHeight);
    }
}
