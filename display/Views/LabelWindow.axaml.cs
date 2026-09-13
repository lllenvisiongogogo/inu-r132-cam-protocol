using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Media.Imaging;
using Avalonia.Threading;
using InuR132.Display.Services;

namespace InuR132.Display.Views;

/// <summary>
/// Hand-labelling for `train/raw`: the directories on the left, the photo on the
/// right, paging along the bottom and the training button top right.
///
/// Every edit is written to the `.txt` beside the photo immediately - there is no
/// save button, and no undo history beyond the file itself. That file is what
/// `train/scripts/assemble_dataset.py` reads, so the trainer never needs to know
/// this window exists.
/// </summary>
public partial class LabelWindow : Window
{
    private readonly TrainingRunner _trainer = new();
    private readonly StringBuilder _log = new();

    private List<(string Name, string Path, int Photos, int Unlabelled)> _directories = new();
    private List<string> _photos = new();
    private List<string> _classes = new();
    private int _index = -1;
    private Bitmap? _bitmap;
    private int _logLines;

    public LabelWindow()
    {
        InitializeComponent();

        Canvas.ClassName = NameOfClass;
        DirList.SelectionChanged += (_, _) => OpenDirectory();
        PrevButton.Click += (_, _) => Step(-1);
        NextButton.Click += (_, _) => Step(1);
        DeleteButton.Click += (_, _) => Canvas.RemoveSelected();
        NegativeButton.Click += (_, _) => MarkNegative();
        ClassBox.SelectionChanged += (_, _) =>
            Canvas.ArmedClassId = ClassBox.SelectedIndex < 0 ? 0 : ClassBox.SelectedIndex;
        Canvas.Changed += (_, _) => Save();
        Canvas.SelectionChanged += (_, _) => UpdateStatus();
        TrainButton.Click += (_, _) => StartTraining();

        _trainer.Output += line => Dispatcher.UIThread.Post(() => AppendLog(line));
        _trainer.Finished += code => Dispatcher.UIThread.Post(() => TrainingFinished(code));

        // Tunnel so the shortcut works wherever the focus happens to be, e.g. the
        // directory list, which would otherwise swallow the arrow keys.
        AddHandler(KeyDownEvent, OnKeyDown, RoutingStrategies.Tunnel);
        Closed += (_, _) => Cleanup();

        AppendLog($"仓库根目录 {LabelStore.RepoRoot}");
        LoadClasses();
        LoadDirectories();
        UpdateStatus();
    }

    private string NameOfClass(int classId) =>
        classId >= 0 && classId < _classes.Count ? _classes[classId] : $"class {classId}";

    private void LoadClasses()
    {
        _classes = LabelStore.Classes();
        ClassBox.Items.Clear();
        foreach (var name in _classes)
        {
            ClassBox.Items.Add(name);
        }

        ClassBox.SelectedIndex = _classes.Count > 0 ? 0 : -1;
        Canvas.ArmedClassId = 0;
        if (_classes.Count == 0)
        {
            AppendLog($"还没有类别：往 {LabelStore.ClassesFile} 里写，一行一个");
        }
    }

    private void LoadDirectories()
    {
        _directories = LabelStore.Directories();
        DirList.Items.Clear();
        foreach (var (name, _, photos, unlabelled) in _directories)
        {
            DirList.Items.Add(unlabelled > 0
                ? $"{name}    ({photos} 张 · {unlabelled} 未标)"
                : $"{name}    ({photos} 张 ✓)");
        }

        DirSummary.Text = $"{_directories.Count} 个目录\n{LabelStore.RawDir}";
        if (DirList.ItemCount > 0)
        {
            DirList.SelectedIndex = 0;
        }
        else
        {
            AppendLog($"没有目录：{LabelStore.RawDir} 下先建一个类别目录，把照片放进去");
        }
    }

    private void OpenDirectory()
    {
        var selected = DirList.SelectedIndex;
        if (selected < 0 || selected >= _directories.Count)
        {
            return;
        }

        _photos = LabelStore.Photos(_directories[selected].Path);
        _index = _photos.Count > 0 ? 0 : -1;
        if (_photos.Count == 0)
        {
            Canvas.SetImage(null, null);
            HeaderText.Text = _directories[selected].Path;
            CounterText.Text = "0 / 0";
        }

        ShowCurrent();
    }

    private void Step(int delta)
    {
        if (_photos.Count == 0)
        {
            return;
        }

        var next = Math.Clamp(_index + delta, 0, _photos.Count - 1);
        if (next == _index)
        {
            return;
        }

        _index = next;
        ShowCurrent();
    }

    private void ShowCurrent()
    {
        if (_index < 0 || _index >= _photos.Count)
        {
            UpdateStatus();
            return;
        }

        var photo = _photos[_index];
        var (boxes, problem) = LabelStore.Load(photo);

        Bitmap? loaded = null;
        try
        {
            loaded = new Bitmap(photo);
        }
        catch (Exception e)
        {
            AppendLog($"打不开 {Path.GetFileName(photo)}: {e.Message}");
        }

        var previous = _bitmap;
        _bitmap = loaded;
        Canvas.SetImage(loaded, boxes);
        previous?.Dispose();

        HeaderText.Text = photo;
        CounterText.Text = $"{_index + 1} / {_photos.Count}";
        if (problem is not null)
        {
            AppendLog($"{Path.GetFileName(photo)}: {problem}");
        }

        UpdateStatus();
    }

    /// <summary>Update the photo/未标 counts of the directory being worked in.</summary>
    private void RefreshDirectoryItem()
    {
        var selected = DirList.SelectedIndex;
        if (selected < 0 || selected >= _directories.Count)
        {
            return;
        }

        var (name, path, _, _) = _directories[selected];
        var photos = LabelStore.Photos(path);
        var unlabelled = photos.Count(photo => !LabelStore.HasLabel(photo));
        _directories[selected] = (name, path, photos.Count, unlabelled);
        DirList.Items[selected] = unlabelled > 0
            ? $"{name}    ({photos.Count} 张 · {unlabelled} 未标)"
            : $"{name}    ({photos.Count} 张 ✓)";
    }

    /// <summary>Write the labels beside the photo. Called after every edit.</summary>
    private void Save()
    {
        if (_index < 0 || _index >= _photos.Count)
        {
            return;
        }

        try
        {
            LabelStore.Save(_photos[_index], Canvas.Boxes);
            SaveText.Text = $"已保存 {DateTime.Now:HH:mm:ss}";
            RefreshDirectoryItem();
        }
        catch (Exception e)
        {
            SaveText.Text = $"保存失败: {e.Message}";
            AppendLog($"保存 {Path.GetFileName(_photos[_index])} 失败: {e.Message}");
        }

        UpdateStatus();
    }

    /// <summary>
    /// Write an empty label: this photo has nothing to find. The file has to
    /// exist, because assemble_dataset.py refuses to guess whether a missing one
    /// means "not labelled yet" or "negative", and stops the run instead.
    /// </summary>
    private void MarkNegative()
    {
        if (_index < 0 || _index >= _photos.Count)
        {
            return;
        }

        Canvas.SetImage(_bitmap, Array.Empty<LabelBox>());
        Save();
        AppendLog($"{Path.GetFileName(_photos[_index])}: 标记为无目标（空标签）");
    }

    private void UpdateStatus()
    {
        var count = Canvas.Boxes.Count;
        var selected = Canvas.SelectedIndex;
        var labelled = _index >= 0 && _index < _photos.Count && LabelStore.HasLabel(_photos[_index]);
        LabelledText.Text = labelled ? "已标注 ✓" : "未标注";
        LabelledText.Foreground = labelled
            ? new Avalonia.Media.SolidColorBrush(Avalonia.Media.Colors.LightGreen)
            : new Avalonia.Media.SolidColorBrush(Avalonia.Media.Colors.Orange);
        if (selected >= 0 && selected < count)
        {
            StatusText.Text = $"{count} 个框 · 选中 {NameOfClass(Canvas.Boxes[selected].ClassId)}";
        }
        else
        {
            StatusText.Text = count == 0
                ? "0 个框 —— 空标签就是负样本"
                : $"{count} 个框";
        }
    }

    private void OnKeyDown(object? sender, KeyEventArgs e)
    {
        switch (e.Key)
        {
            case Key.Delete or Key.Back:
                e.Handled = Canvas.RemoveSelected();
                break;
            case Key.N:
                MarkNegative();
                e.Handled = true;
                break;
            case Key.Left or Key.PageUp:
                Step(-1);
                e.Handled = true;
                break;
            case Key.Right or Key.PageDown:
                Step(1);
                e.Handled = true;
                break;
            case Key.D1 or Key.D2 or Key.D3 or Key.D4 or Key.D5 or Key.D6 or Key.D7 or Key.D8 or Key.D9:
                var classIndex = e.Key - Key.D1;
                if (classIndex < _classes.Count)
                {
                    ClassBox.SelectedIndex = classIndex;
                    e.Handled = true;
                }

                break;
        }
    }

    private void StartTraining()
    {
        if (_trainer.IsRunning)
        {
            return;
        }

        LogPanel.IsVisible = true;
        AppendLog($"$ {LabelStore.PipelineScript}");
        TrainButton.IsEnabled = false;
        try
        {
            _trainer.Start();
        }
        catch (Exception e)
        {
            AppendLog($"启动训练失败: {e.Message}");
            TrainButton.IsEnabled = true;
        }
    }

    private void TrainingFinished(int exitCode)
    {
        TrainButton.IsEnabled = true;
        AppendLog(exitCode == 0
            ? "训练完成，最佳权重已发布到 models/"
            : $"训练失败，退出码 {exitCode}");
    }

    private void AppendLog(string line)
    {
        _log.Append(line).Append('\n');
        _logLines++;
        if (_logLines > 800)
        {
            var text = _log.ToString();
            var cut = text.IndexOf('\n', text.Length / 3);
            _log.Clear();
            _log.Append(cut >= 0 ? text[(cut + 1)..] : text);
            _logLines = _log.ToString().Count(c => c == '\n');
        }

        LogBox.Text = _log.ToString();
        LogBox.CaretIndex = LogBox.Text?.Length ?? 0;
    }

    private void Cleanup()
    {
        _trainer.Dispose();
        _bitmap?.Dispose();
    }
}
