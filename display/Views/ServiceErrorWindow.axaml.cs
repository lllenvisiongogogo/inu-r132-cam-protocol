using System;
using Avalonia.Controls;

namespace InuR132.Display.Views;

public partial class ServiceErrorWindow : Window
{
    public event Action? RetryRequested;

    public ServiceErrorWindow()
    {
        InitializeComponent();
        RetryButton.Click += (_, _) => RetryRequested?.Invoke();
        QuitButton.Click += (_, _) => Close();
    }

    public void SetMessage(string message) => MessageText.Text = message;
}
