"""Картинки установщика: боковая панель приветствия/финиша и шапка страниц.

Тот же язык, что у лёгкого интерфейса (frontend/lite.css): воронёная сталь, иконка
программы (src-tauri/icons/icon.png) с люминофорным ореолом, логотип Handjet.
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
ICON = ROOT / "src-tauri" / "icons" / "icon.png"

# Цвета — как в models.nsh (REEVER_C_*): фон страниц совпадает с фоном картинок.
BG = (71, 82, 79)
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


def icon(size):
    """Иконка программы с мягким люминофорным ореолом."""
    pad = round(size * 0.08)
    s = size + 2 * pad
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    c = s / 2
    halo = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    r = size * 0.5
    ImageDraw.Draw(halo).ellipse([c - r, c - r, c + r, c + r], fill=ACC + (90,))
    img.alpha_composite(halo.filter(ImageFilter.GaussianBlur(size * 0.06)))
    ico = Image.open(ICON).convert("RGBA").resize((size, size), Image.LANCZOS)
    img.alpha_composite(ico, (pad, pad))
    return img


def sidebar():
    w, h = 328, 628
    img = brushed(w, h, (116, 129, 125), (60, 70, 67)).convert("RGBA")
    # правая кромка — стык металла с тёмной страницей: блик и тень
    d = ImageDraw.Draw(img)
    d.line([(w - 2, 0), (w - 2, h)], fill=(70, 84, 80, 255), width=1)
    d.line([(w - 1, 0), (w - 1, h)], fill=(5, 8, 8, 255), width=1)
    o = icon(196)
    img.alpha_composite(o, ((w - o.width) // 2, 268 - o.height // 2))
    glow_text(img, (w // 2, 440), "REEVER", font(74, 600), blur=7)
    d = ImageDraw.Draw(img)
    for i in range(23):
        x = w // 2 - 110 + i * 10
        d.rectangle([x, 492, x + 4, 495], fill=ACC + (70 if i % 4 else 150,))
    return img.convert("RGB")


def header():
    w, h = 300, 114
    img = Image.new("RGBA", (w, h), BG + (255,))
    o = icon(88)
    img.alpha_composite(o, (w - o.width - 2, (h - o.height) // 2))
    glow_text(img, (w - 116, h // 2 + 2), "REEVER", font(50, 600), anchor="rm", blur=5)
    return img.convert("RGB")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    sidebar().save(OUT / "sidebar.bmp")
    header().save(OUT / "header.bmp")
    print("ok:", OUT)
