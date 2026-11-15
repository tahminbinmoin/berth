"""Generate Berth's app icon (assets/berth.ico and assets/berth-logo.png).

The icon is drawn from simple shapes so it can be regenerated and tweaked
without any third-party artwork. Requires Pillow:  pip install pillow

    python tools/make_icon.py
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
S = 1024  # master canvas size


def lerp(a, b, t):
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(len(a)))


def vertical_gradient(size, top, bottom):
    w, h = size
    img = Image.new("RGBA", size)
    px = img.load()
    for y in range(h):
        c = lerp(top, bottom, y / max(1, h - 1))
        for x in range(w):
            px[x, y] = c
    return img


def rounded_mask(size, box, radius):
    m = Image.new("L", size, 0)
    ImageDraw.Draw(m).rounded_rectangle(box, radius=radius, fill=255)
    return m


def master(simple=False):
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # Background tile: deep teal to indigo.
    inset = 64
    tile_box = (inset, inset, S - inset, S - inset)
    tile = vertical_gradient((S, S), (20, 150, 160, 255), (37, 56, 140, 255))
    img.paste(tile, (0, 0), rounded_mask((S, S), tile_box, 210))

    # Soft top sheen.
    sheen = Image.new("RGBA", (S, S), (255, 255, 255, 0))
    ImageDraw.Draw(sheen).ellipse((-300, -700, S + 300, 380), fill=(255, 255, 255, 22))
    sheen = sheen.filter(ImageFilter.GaussianBlur(60))
    sheen.putalpha(Image.composite(sheen.getchannel("A"), Image.new("L", (S, S), 0),
                                   rounded_mask((S, S), tile_box, 210)))
    img = Image.alpha_composite(img, sheen)

    d = ImageDraw.Draw(img)

    # The "dock" shelf.
    shelf = (190, 610, S - 190, 790)
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((shelf[0], shelf[1] + 24, shelf[2], shelf[3] + 24),
                                             radius=80, fill=(0, 0, 0, 90))
    img = Image.alpha_composite(img, shadow.filter(ImageFilter.GaussianBlur(26)))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle(shelf, radius=80, fill=(255, 255, 255, 235))

    # Three "app" tiles resting on the shelf.
    colors = [(255, 184, 77, 255), (94, 214, 190, 255), (130, 160, 255, 255)]
    tw = 150 if not simple else 190
    gap = 40 if not simple else 30
    total = tw * 3 + gap * 2
    x0 = (S - total) // 2
    for i, c in enumerate(colors):
        x = x0 + i * (tw + gap)
        lift = 70 if (i == 1 and not simple) else 0
        box = (x, shelf[1] - tw + 60 - lift, x + tw, shelf[1] + 60 - lift)
        d.rounded_rectangle(box, radius=int(tw * 0.28), fill=c)

    # Running indicator dot under the middle tile.
    if not simple:
        cx = S // 2
        d.ellipse((cx - 18, shelf[3] - 62, cx + 18, shelf[3] - 26), fill=(37, 56, 140, 255))
    return img


def main():
    ASSETS.mkdir(exist_ok=True)
    big = master()
    small = master(simple=True)
    sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
    frames = []
    for s in sizes:
        src = small if s <= 32 else big
        frames.append(src.resize((s, s), Image.LANCZOS))
    frames[-1].save(ASSETS / "berth.ico", format="ICO", sizes=[(s, s) for s in sizes],
                    append_images=frames[:-1])
    big.resize((512, 512), Image.LANCZOS).save(ASSETS / "berth-logo.png")
    print("wrote", ASSETS / "berth.ico", "and", ASSETS / "berth-logo.png")


if __name__ == "__main__":
    main()
