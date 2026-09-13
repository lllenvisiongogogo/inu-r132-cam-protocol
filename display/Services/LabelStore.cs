using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using Avalonia;

namespace InuR132.Display.Services;

/// <summary>
/// One box of a label file, in the normalised YOLO form the trainer expects:
/// centre and size as fractions of the image, class id from <c>classes.txt</c>.
/// </summary>
public sealed class LabelBox
{
    public int ClassId { get; set; }
    public double Cx { get; set; }
    public double Cy { get; set; }
    public double W { get; set; }
    public double H { get; set; }

    /// <summary>The box in image pixels, which is what editing works in.</summary>
    public Rect ToPixels(int imageWidth, int imageHeight) => new(
        (Cx - W / 2) * imageWidth,
        (Cy - H / 2) * imageHeight,
        W * imageWidth,
        H * imageHeight);

    public static LabelBox FromPixels(int classId, Rect rect, int imageWidth, int imageHeight) => new()
    {
        ClassId = classId,
        Cx = (rect.X + rect.Width / 2) / imageWidth,
        Cy = (rect.Y + rect.Height / 2) / imageHeight,
        W = rect.Width / imageWidth,
        H = rect.Height / imageHeight,
    };

    public LabelBox Copy() => new() { ClassId = ClassId, Cx = Cx, Cy = Cy, W = W, H = H };
}

/// <summary>One photo of a directory, with the path of its label file.</summary>
public sealed record LabelImage(string Photo, int BoxCount);

/// <summary>
/// Reads and writes the labelling data: `train/raw/&lt;class&gt;/&lt;stem&gt;.jpg` with a
/// `<stem>.txt` beside it, and `train/classes.txt` for the class names.
///
/// Nothing here generates a box. The trainer's own script
/// (`train/scripts/assemble_dataset.py`) reads exactly the same files.
/// </summary>
public static class LabelStore
{
    public static readonly string[] PhotoExtensions = { ".jpg", ".jpeg", ".png" };

    /// <summary>
    /// The repository root: the directory holding `Makefile` and `train/`. Found
    /// by walking up from the binary, since the app lives in
    /// `display/bin/&lt;config&gt;/net8.0/`.
    /// </summary>
    public static string RepoRoot { get; } = FindRepoRoot();

    public static string TrainDir => Path.Combine(RepoRoot, "train");
    public static string RawDir => Path.Combine(TrainDir, "raw");
    public static string ClassesFile => Path.Combine(TrainDir, "classes.txt");
    public static string PipelineScript => Path.Combine(TrainDir, "scripts", "pipeline.sh");

    private static string FindRepoRoot()
    {
        var fromEnv = Environment.GetEnvironmentVariable("INU_R132_REPO");
        if (!string.IsNullOrWhiteSpace(fromEnv) && Directory.Exists(fromEnv))
        {
            return Path.GetFullPath(fromEnv);
        }

        var candidates = new List<string> { AppContext.BaseDirectory, Directory.GetCurrentDirectory() };
        var info = new DirectoryInfo(AppContext.BaseDirectory);
        for (var level = 0; level < 8 && info?.Parent is not null; level++)
        {
            info = info.Parent;
            candidates.Add(info.FullName);
        }

        foreach (var candidate in candidates)
        {
            if (File.Exists(Path.Combine(candidate, "Makefile"))
                && Directory.Exists(Path.Combine(candidate, "train")))
            {
                return candidate;
            }
        }

        return Directory.GetCurrentDirectory();
    }

    /// <summary>The class names, one per line in `train/classes.txt`.</summary>
    public static List<string> Classes()
    {
        if (!File.Exists(ClassesFile))
        {
            return new List<string>();
        }

        return File.ReadAllLines(ClassesFile)
            .Select(line => line.Trim())
            .Where(line => line.Length > 0 && !line.StartsWith('#'))
            .ToList();
    }

    /// <summary>
    /// The directories under `train/raw`, sorted, with how many photos each has
    /// and how many of them have no label file yet.
    /// </summary>
    public static List<(string Name, string Path, int Photos, int Unlabelled)> Directories()
    {
        if (!Directory.Exists(RawDir))
        {
            return new List<(string, string, int, int)>();
        }

        return Directory.EnumerateDirectories(RawDir)
            .OrderBy(path => path, StringComparer.Ordinal)
            .Select(path =>
            {
                var photos = Photos(path);
                return (
                    Path.GetFileName(path),
                    path,
                    photos.Count,
                    photos.Count(photo => !HasLabel(photo)));
            })
            .ToList();
    }

    /// <summary>
    /// Does a label file exist? An empty one counts - that is a negative, and
    /// `assemble_dataset.py` treats "no file at all" as unlabelled and stops.
    /// </summary>
    public static bool HasLabel(string photo) => File.Exists(LabelPath(photo));

    public static bool IsPhoto(string path) =>
        PhotoExtensions.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase);

    /// <summary>Every photo of a directory, sorted by name (which is a timestamp).</summary>
    public static List<string> Photos(string directory)
    {
        if (!Directory.Exists(directory))
        {
            return new List<string>();
        }

        return Directory.EnumerateFiles(directory)
            .Where(IsPhoto)
            .OrderBy(path => path, StringComparer.Ordinal)
            .ToList();
    }

    /// <summary>`&lt;stem&gt;.txt` beside the photo.</summary>
    public static string LabelPath(string photo) => Path.ChangeExtension(photo, ".txt");

    /// <summary>How many boxes each photo of a directory has, for the list.</summary>
    public static List<LabelImage> Summarise(IEnumerable<string> photos) =>
        photos.Select(photo => new LabelImage(photo, Load(photo).Boxes.Count)).ToList();

    /// <summary>
    /// The labels beside a photo. A missing file is not an error - it just has no
    /// boxes yet - but a malformed line is reported so it is never silently lost.
    /// </summary>
    public static (List<LabelBox> Boxes, string? Problem) Load(string photo)
    {
        var path = LabelPath(photo);
        if (!File.Exists(path))
        {
            return (new List<LabelBox>(), null);
        }

        var boxes = new List<LabelBox>();
        var problems = new List<string>();
        var lineNumber = 0;
        foreach (var line in File.ReadAllLines(path))
        {
            lineNumber++;
            var text = line.Trim();
            if (text.Length == 0)
            {
                continue;
            }

            var parts = text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
            if (parts.Length != 5
                || !int.TryParse(parts[0], NumberStyles.Integer, CultureInfo.InvariantCulture, out var classId)
                || !double.TryParse(parts[1], NumberStyles.Float, CultureInfo.InvariantCulture, out var cx)
                || !double.TryParse(parts[2], NumberStyles.Float, CultureInfo.InvariantCulture, out var cy)
                || !double.TryParse(parts[3], NumberStyles.Float, CultureInfo.InvariantCulture, out var w)
                || !double.TryParse(parts[4], NumberStyles.Float, CultureInfo.InvariantCulture, out var h)
                || w <= 0 || h <= 0)
            {
                problems.Add($"第 {lineNumber} 行");
                continue;
            }

            boxes.Add(new LabelBox { ClassId = classId, Cx = cx, Cy = cy, W = w, H = h });
        }

        return (boxes, problems.Count == 0 ? null : string.Join("、", problems) + " 无法解析，已跳过");
    }

    /// <summary>
    /// Write the labels beside the photo. No boxes writes an empty file, which is
    /// how a negative ("nothing here") is recorded.
    /// </summary>
    public static void Save(string photo, IEnumerable<LabelBox> boxes)
    {
        var lines = boxes
            .Where(box => box.W > 0 && box.H > 0)
            .Select(box => string.Format(
                CultureInfo.InvariantCulture,
                "{0} {1:F6} {2:F6} {3:F6} {4:F6}",
                box.ClassId,
                Math.Clamp(box.Cx, 0.0, 1.0),
                Math.Clamp(box.Cy, 0.0, 1.0),
                Math.Clamp(box.W, 0.0, 1.0),
                Math.Clamp(box.H, 0.0, 1.0)));

        File.WriteAllText(LabelPath(photo), string.Join("\n", lines) + (lines.Any() ? "\n" : ""));
    }
}
