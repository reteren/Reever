"""Картинки установщика: боковая панель приветствия/финиша и шапка страниц.

Тот же язык, что у лёгкого интерфейса (frontend/lite.css): воронёная сталь, сфера с
люминофорной жидкостью в стальном ободе, шкала-насечки, логотип Handjet.
Пишет 24-битные BMP (NSIS другого не понимает) вдвое крупнее места на странице —
MUI ужимает их по размеру элемента, и на 200% масштабе картинка остаётся чёткой.

    python tools/installer_art.py
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src-tauri" / "windows" / "art"
FONT = ROOT / "frontend" / "fonts" / "Handjet.ttf"

# Цвета — как в models.nsh (REEVER_C_*): фон страниц совпадает с фоном картинок.
BG = (12, 19, 18)
ACC = (39, 230, 170)
HOT = (200, 255, 236)


def font(size, weight=500):
    f = ImageFont.truetype(str(FONT), size)
    try:
        f.set_variation_by_axes([weight, 2, 1])
    except Exception:
        pass
    return f


def brushed(w, h, top, bottom):
    """Шлифованная сталь: вертикальный градиент и тонкие горизонтальные штрихи."""
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(1, h - 1)
        base = [round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)]
        streak = 3 if y % 3 == 0 else (-2 if y % 3 == 2 else 0)
        for x in range(w):
            px[x, y] = tuple(max(0, min(255, c + streak)) for c in base)
    return img


def glow_text(img, xy, text, f, anchor="mm", blur=6):
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    d.text(xy, text, font=f, fill=ACC + (210,), anchor=anchor)
    halo = layer.filter(ImageFilter.GaussianBlur(blur))
    img.alpha_composite(halo)
    img.alpha_composite(halo)
    d2 = ImageDraw.Draw(img)
    d2.text(xy, text, font=f, fill=HOT + (255,), anchor=anchor)


def orb(size, lit=True, ticks=True):
    """Сфера лёгкого интерфейса: стальной обод, жидкость на две трети, шкала вокруг."""
    s = size
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    c = s / 2
    r_dial = s * 0.48
    r_ring = s * 0.38
    r_glass = s * 0.33
    d = ImageDraw.Draw(img)
    if lit:
        halo = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        ImageDraw.Draw(halo).ellipse([c - r_ring - s * .04, c - r_ring - s * .04, c + r_ring + s * .04, c + r_ring + s * .04], fill=ACC + (120,))
        img.alpha_composite(halo.filter(ImageFilter.GaussianBlur(s * 0.06)))
    if ticks:
        for i in range(36):
            a = math.radians(i * 10 - 90)
            r0, r1 = r_dial - s * 0.035, r_dial
            d.line([(c + r0 * math.cos(a), c + r0 * math.sin(a)), (c + r1 * math.cos(a), c + r1 * math.sin(a))], fill=ACC + (110,), width=max(1, round(s * 0.008)))
    # стальной обод: коническая заливка по углу — блики сверху-слева и снизу-справа
    ring = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    rp = ring.load()
    stops = [(0, (154, 168, 164)), (.18, (46, 56, 54)), (.34, (201, 214, 210)), (.52, (58, 69, 66)), (.70, (127, 141, 137)), (.86, (38, 47, 45)), (1, (154, 168, 164))]
    for y in range(s):
        for x in range(s):
            dx, dy = x + .5 - c, y + .5 - c
            rr = math.hypot(dx, dy)
            if r_glass <= rr <= r_ring:
                t = ((math.atan2(dy, dx) + math.pi) / (2 * math.pi) + 210 / 360) % 1
                for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
                    if t0 <= t <= t1:
                        k = (t - t0) / (t1 - t0)
                        col = tuple(round(c0[i] + (c1[i] - c0[i]) * k) for i in range(3))
                        break
                edge = min(rr - r_glass, r_ring - rr)
                rp[x, y] = col + (round(255 * min(1, edge + .5)),)
    img.alpha_composite(ring)
    # стекло и жидкость
    glass = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    gp = glass.load()
    level = c - r_glass + 2 * r_glass * 0.34
    for y in range(s):
        for x in range(s):
            rr = math.hypot(x + .5 - c, y + .5 - c)
            if rr <= r_glass:
                a = round(255 * min(1, r_glass - rr + .5))
                if lit and y >= level:
                    t = (y - level) / (c + r_glass - level)
                    if y - level < s * 0.012:
                        col = HOT
                    else:
                        col = tuple(round(ACC[i] * (1 - t) * .9 + (4, 70, 50)[i] * t) for i in range(3))
                else:
                    t = rr / r_glass
                    col = tuple(round((6, 25, 15)[i] * (1 - t) + (2, 10, 8)[i] * t) for i in range(3))
                gp[x, y] = col + (a,)
    img.alpha_composite(glass)
    # блик стекла
    hl = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(hl).ellipse([c - r_glass * .62, c - r_glass * .78, c - r_glass * .05, c - r_glass * .38], fill=(220, 255, 240, 40))
    img.alpha_composite(hl.filter(ImageFilter.GaussianBlur(s * 0.02)))
    return img


def sidebar():
    w, h = 328, 628
    img = brushed(w, h, (44, 54, 51), (14, 19, 18)).convert("RGBA")
    # правая кромка — стык металла с тёмной страницей: блик и тень
    d = ImageDraw.Draw(img)
    d.line([(w - 2, 0), (w - 2, h)], fill=(70, 84, 80, 255), width=1)
    d.line([(w - 1, 0), (w - 1, h)], fill=(5, 8, 8, 255), width=1)
    o = orb(236)
    img.alpha_composite(o, ((w - 236) // 2, 150))
    glow_text(img, (w // 2, 440), "REEVER", font(74, 600), blur=7)
    d = ImageDraw.Draw(img)
    for i in range(23):
        x = w // 2 - 110 + i * 10
        d.rectangle([x, 492, x + 4, 495], fill=ACC + (70 if i % 4 else 150,))
    return img.convert("RGB")


def header():
    w, h = 300, 114
    img = Image.new("RGBA", (w, h), BG + (255,))
    img.alpha_composite(orb(92, ticks=False), (w - 104, 11))
    glow_text(img, (w - 116, h // 2 + 2), "REEVER", font(50, 600), anchor="rm", blur=5)
    return img.convert("RGB")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    sidebar().save(OUT / "sidebar.bmp")
    header().save(OUT / "header.bmp")
    print("ok:", OUT)
