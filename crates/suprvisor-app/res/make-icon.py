# Draws suprvisor.ico: a flat rounded tile with one pulse line. Then compile app.rc to app.res
# with the Windows SDK's rc.exe; build.rs links app.res, so normal builds need no SDK.
#   python make-icon.py && rc /nologo /fo app.res app.rc
from PIL import Image, ImageDraw

TILE = (36, 99, 235, 255)
LINE = (255, 255, 255, 255)

def icon(size):
    s = 1024
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((64, 64, s - 64, s - 64), radius=200, fill=TILE)
    # Small sizes get a thicker line, so it stays readable at 16 px.
    w = 56 if size >= 48 else 88
    pts = [(220, 540), (400, 540), (470, 380), (560, 680), (630, 540), (804, 540)]
    d.line(pts, fill=LINE, width=w, joint="curve")
    for x, y in (pts[0], pts[-1]):
        d.ellipse((x - w // 2, y - w // 2, x + w // 2, y + w // 2), fill=LINE)
    return img.resize((size, size), Image.LANCZOS)

sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
images = [icon(n) for n in sizes]
images[-1].save("suprvisor.ico", sizes=[(n, n) for n in sizes], append_images=images[:-1])
