using System.Drawing;
using System.Windows.Forms;

// Plain dark window behind the target's rectangle (plus margin for the shadow).
// Activated right before each launch, so the target opens on top of a static image.
public sealed class Curtain : IDisposable
{
    readonly Form _form;
    readonly Thread _thread;

    public Curtain(Rect r)
    {
        var ready = new ManualResetEventSlim();
        _form = new Form
        {
            FormBorderStyle = FormBorderStyle.None, StartPosition = FormStartPosition.Manual, ShowInTaskbar = false,
            BackColor = Color.FromArgb(24, 24, 24),
            Bounds = Rectangle.FromLTRB(r.Left - 60, r.Top - 60, r.Right + 60, r.Bottom + 60),
        };
        _form.Shown += (_, _) => ready.Set();
        _thread = new Thread(() => Application.Run(_form)) { IsBackground = true };
        _thread.SetApartmentState(ApartmentState.STA);
        _thread.Start();
        ready.Wait();
    }

    public void Activate() => _form.Invoke(() => { _form.TopMost = true; _form.TopMost = false; _form.Activate(); });

    public void Dispose() { _form.Invoke(_form.Close); _thread.Join(); }
}
