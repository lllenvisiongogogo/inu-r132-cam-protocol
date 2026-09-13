using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using InuR132.Display.Services;

namespace InuR132.Display.Controls;

/// <summary>
/// The labelling surface: the photo, its boxes, and the mouse editing.
///
/// Drag on empty space to draw a box, drag a box to move it, drag one of the
/// eight handles of the selected box to resize it. Edits happen in image pixels
/// and are converted to the normalised form on the way out; <see cref="Changed"/>
/// fires once per gesture so the window can save immediately.
///
/// Everything is drawn in <see cref="Render"/> rather than as child controls, so
/// a drag does not churn the visual tree.
/// </summary>
public sealed class BoxCanvas : Control
{
    private enum Mode { Idle, Draw, Move, Resize }

    /// <summary>Box colours by class id, the same palette the camera viewer uses.</summary>
    private static readonly Color[] Palette =
    {
        Color.FromRgb(0, 255, 0),
        Color.FromRgb(255, 170, 0),
        Color.FromRgb(0, 200, 255),
        Color.FromRgb(255, 0, 255),
        Color.FromRgb(255, 60, 60),
        Color.FromRgb(255, 255, 0),
    };

    private const double HandleRadius = 4.0;   // control pixels
    private const double MinSize = 4.0;        // image pixels
    private const double LabelFontSize = 13.0;

    private readonly List<LabelBox> _boxes = new();
    private Bitmap? _image;
    private Mode _mode = Mode.Idle;
    private int _active = -1;
    private int _handle = -1;
    private Point _grab;
    private Rect _startRect;

    public BoxCanvas()
    {
        ClipToBounds = true;
        Focusable = true;
    }

    /// <summary>Class a newly drawn box gets.</summary>
    public int ArmedClassId { get; set; }

    /// <summary>Name of <see cref="ArmedClassId"/>, for the label on new boxes.</summary>
    public Func<int, string> ClassName { get; set; } = id => id.ToString(CultureInfo.InvariantCulture);

    /// <summary>Raised once a gesture has changed the boxes, for the auto-save.</summary>
    public event EventHandler? Changed;

    /// <summary>Raised when the selection moves, for the status line.</summary>
    public event EventHandler? SelectionChanged;

    public IReadOnlyList<LabelBox> Boxes => _boxes;

    public int SelectedIndex => _active;

    public int ImageWidth => _image?.PixelSize.Width ?? 0;
    public int ImageHeight => _image?.PixelSize.Height ?? 0;

    /// <summary>Point the bitmap at a photo and its labels. The caller owns the bitmap.</summary>
    public void SetImage(Bitmap? image, IEnumerable<LabelBox>? boxes)
    {
        _image = image;
        _boxes.Clear();
        if (boxes is not null)
        {
            _boxes.AddRange(boxes);
        }

        _mode = Mode.Idle;
        _active = -1;
        _handle = -1;
        InvalidateVisual();
        SelectionChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>Drop the selected box. True when one was removed.</summary>
    public bool RemoveSelected()
    {
        if (_active < 0 || _active >= _boxes.Count)
        {
            return false;
        }

        _boxes.RemoveAt(_active);
        _active = -1;
        InvalidateVisual();
        SelectionChanged?.Invoke(this, EventArgs.Empty);
        Changed?.Invoke(this, EventArgs.Empty);
        return true;
    }

    // ------------------------------------------------------------ geometry ----

    /// <summary>Where the photo is drawn inside the control, keeping its aspect.</summary>
    private Rect ImageRect()
    {
        var area = Bounds.Size;
        if (_image is null || area.Width <= 0 || area.Height <= 0)
        {
            return default;
        }

        var imageWidth = (double)_image.PixelSize.Width;
        var imageHeight = (double)_image.PixelSize.Height;
        var scale = Math.Min(area.Width / imageWidth, area.Height / imageHeight);
        var width = imageWidth * scale;
        var height = imageHeight * scale;
        return new Rect((area.Width - width) / 2, (area.Height - height) / 2, width, height);
    }

    private Point ToImage(Point control) => _image is null
        ? default
        : new Point(
            (control.X - ImageRect().X) / ImageRect().Width * _image.PixelSize.Width,
            (control.Y - ImageRect().Y) / ImageRect().Height * _image.PixelSize.Height);

    private Point ToControl(Point image) => _image is null
        ? default
        : new Point(
            ImageRect().X + image.X / _image.PixelSize.Width * ImageRect().Width,
            ImageRect().Y + image.Y / _image.PixelSize.Height * ImageRect().Height);

    private Rect ToControl(Rect image) => new(ToControl(image.TopLeft), ToControl(image.BottomRight));

    private static Rect FromCorners(Point a, Point b) =>
        new(Math.Min(a.X, b.X), Math.Min(a.Y, b.Y), Math.Abs(b.X - a.X), Math.Abs(b.Y - a.Y));

    private Rect Clamp(Rect rect)
    {
        var width = Math.Min(rect.Width, ImageWidth);
        var height = Math.Min(rect.Height, ImageHeight);
        var x = Math.Clamp(rect.X, 0, Math.Max(0, ImageWidth - width));
        var y = Math.Clamp(rect.Y, 0, Math.Max(0, ImageHeight - height));
        return new Rect(x, y, width, height);
    }

    /// <summary>The eight grab points of a box, in control coordinates.</summary>
    private static Point[] Handles(Rect rect) => new[]
    {
        rect.TopLeft,
        new Point(rect.Center.X, rect.Top),
        rect.TopRight,
        new Point(rect.Right, rect.Center.Y),
        rect.BottomRight,
        new Point(rect.Center.X, rect.Bottom),
        rect.BottomLeft,
        new Point(rect.Left, rect.Center.Y),
    };

    private int HitHandle(Rect controlRect, Point point)
    {
        var handles = Handles(controlRect);
        for (var index = 0; index < handles.Length; index++)
        {
            var delta = handles[index] - point;
            if (Math.Abs(delta.X) <= HandleRadius + 2 && Math.Abs(delta.Y) <= HandleRadius + 2)
            {
                return index;
            }
        }

        return -1;
    }

    private static Color ColourFor(int classId)
    {
        var index = ((classId % Palette.Length) + Palette.Length) % Palette.Length;
        return Palette[index];
    }

    // ------------------------------------------------------------ drawing ----

    public override void Render(DrawingContext context)
    {
        base.Render(context);
        if (_image is null)
        {
            return;
        }

        var rect = ImageRect();
        if (rect.Width <= 0 || rect.Height <= 0)
        {
            return;
        }

        context.DrawImage(_image,
            new Rect(0, 0, _image.PixelSize.Width, _image.PixelSize.Height), rect);

        for (var index = 0; index < _boxes.Count; index++)
        {
            var box = _boxes[index];
            var controlRect = ToControl(box.ToPixels(ImageWidth, ImageHeight));
            var colour = ColourFor(box.ClassId);
            var selected = index == _active;
            context.DrawRectangle(null, new Pen(new SolidColorBrush(colour), selected ? 3 : 2), controlRect);

            // A dark plate behind the text so it stays readable over the photo.
            var text = new FormattedText(
                ClassName(box.ClassId),
                CultureInfo.CurrentCulture,
                FlowDirection.LeftToRight,
                Typeface.Default,
                LabelFontSize,
                new SolidColorBrush(colour));
            var origin = new Point(controlRect.X, Math.Max(0, controlRect.Y - text.Height - 2));
            context.FillRectangle(
                new SolidColorBrush(Color.FromArgb(0xCC, 0, 0, 0)),
                new Rect(origin.X, origin.Y, text.Width + 6, text.Height + 2));
            context.DrawText(text, new Point(origin.X + 3, origin.Y + 1));
        }

        if (_active >= 0 && _active < _boxes.Count)
        {
            var brush = new SolidColorBrush(ColourFor(_boxes[_active].ClassId));
            foreach (var handle in Handles(ToControl(_boxes[_active].ToPixels(ImageWidth, ImageHeight))))
            {
                context.FillRectangle(brush, new Rect(
                    handle.X - HandleRadius, handle.Y - HandleRadius,
                    HandleRadius * 2, HandleRadius * 2));
            }
        }

        // Help when there is nothing to see yet.
        if (_boxes.Count == 0)
        {
            var hint = new FormattedText(
                "拖拽画出框 · 拖框移动 · 拖角缩放 · Del 删除 · 一个框都不画 = 负样本",
                CultureInfo.CurrentCulture,
                FlowDirection.LeftToRight,
                Typeface.Default,
                13,
                new SolidColorBrush(Color.FromArgb(0xAA, 0xFF, 0xFF, 0xFF)));
            context.DrawText(hint, new Point(12, Bounds.Height - hint.Height - 10));
        }
    }

    // ------------------------------------------------------------- editing ----

    protected override void OnPointerPressed(PointerPressedEventArgs e)
    {
        base.OnPointerPressed(e);
        if (_image is null || !e.GetCurrentPoint(this).Properties.IsLeftButtonPressed)
        {
            return;
        }

        Focus();
        var position = e.GetCurrentPoint(this).Position;
        var image = ToImage(position);

        // 1. a handle of the selected box, checked first so a corner is grabbable
        //    even when another box overlaps it.
        if (_active >= 0 && _active < _boxes.Count)
        {
            var selected = ToControl(_boxes[_active].ToPixels(ImageWidth, ImageHeight));
            var handle = HitHandle(selected, position);
            if (handle >= 0)
            {
                _mode = Mode.Resize;
                _handle = handle;
                _grab = image;
                _startRect = _boxes[_active].ToPixels(ImageWidth, ImageHeight);
                e.Pointer.Capture(this);
                e.Handled = true;
                return;
            }
        }

        // 2. an existing box, topmost first
        for (var index = _boxes.Count - 1; index >= 0; index--)
        {
            if (_boxes[index].ToPixels(ImageWidth, ImageHeight).Contains(image))
            {
                _active = index;
                _mode = Mode.Move;
                _grab = image;
                _startRect = _boxes[index].ToPixels(ImageWidth, ImageHeight);
                e.Pointer.Capture(this);
                InvalidateVisual();
                SelectionChanged?.Invoke(this, EventArgs.Empty);
                e.Handled = true;
                return;
            }
        }

        // 3. nothing there: start a new box. It is thrown away on release if it
        //    never grew, so a stray click does not leave a speck behind.
        _boxes.Add(new LabelBox { ClassId = ArmedClassId, Cx = 0, Cy = 0, W = 0, H = 0 });
        _active = _boxes.Count - 1;
        _mode = Mode.Draw;
        _grab = image;
        _startRect = new Rect(image, image);
        e.Pointer.Capture(this);
        InvalidateVisual();
        SelectionChanged?.Invoke(this, EventArgs.Empty);
        e.Handled = true;
    }

    protected override void OnPointerMoved(PointerEventArgs e)
    {
        base.OnPointerMoved(e);
        if (_mode == Mode.Idle || _active < 0 || _active >= _boxes.Count || _image is null)
        {
            return;
        }

        var position = ToImage(e.GetCurrentPoint(this).Position);
        var rect = _mode switch
        {
            Mode.Draw => FromCorners(_grab, position),
            Mode.Move => MoveTo(_startRect, position - _grab),
            Mode.Resize => ResizeTo(_startRect, _handle, position),
            _ => _startRect,
        };

        var classId = _boxes[_active].ClassId;
        _boxes[_active] = LabelBox.FromPixels(classId, Clamp(rect), ImageWidth, ImageHeight);
        InvalidateVisual();
        e.Handled = true;
    }

    protected override void OnPointerReleased(PointerReleasedEventArgs e)
    {
        base.OnPointerReleased(e);
        if (_mode == Mode.Idle)
        {
            return;
        }

        e.Pointer.Capture(null);
        if (_active >= 0 && _active < _boxes.Count)
        {
            var rect = _boxes[_active].ToPixels(ImageWidth, ImageHeight);
            if (rect.Width < MinSize || rect.Height < MinSize)
            {
                _boxes.RemoveAt(_active);
                _active = -1;
                SelectionChanged?.Invoke(this, EventArgs.Empty);
            }
        }

        _mode = Mode.Idle;
        _handle = -1;
        InvalidateVisual();
        Changed?.Invoke(this, EventArgs.Empty);
        e.Handled = true;
    }

    private Rect MoveTo(Rect start, Vector delta)
    {
        var x = Math.Clamp(start.X + delta.X, 0, Math.Max(0, ImageWidth - start.Width));
        var y = Math.Clamp(start.Y + delta.Y, 0, Math.Max(0, ImageHeight - start.Height));
        return new Rect(x, y, start.Width, start.Height);
    }

    private Rect ResizeTo(Rect start, int handle, Point position)
    {
        double left = start.Left, top = start.Top, right = start.Right, bottom = start.Bottom;
        switch (handle)
        {
            case 0: left = position.X; top = position.Y; break;
            case 1: top = position.Y; break;
            case 2: right = position.X; top = position.Y; break;
            case 3: right = position.X; break;
            case 4: right = position.X; bottom = position.Y; break;
            case 5: bottom = position.Y; break;
            case 6: left = position.X; bottom = position.Y; break;
            case 7: left = position.X; break;
        }

        return FromCorners(new Point(left, top), new Point(right, bottom));
    }
}
