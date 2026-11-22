"""GIF для README из кадров `REEVER_README_DEMO` (src-tauri/src/readme_demo.rs).

    python tools/readme_gifs.py <папка-кадров> [<папка-GIF>]   # по умолчанию docs/media

Каждый сценарий — папка с окнами (`<окно>/NNNN.png` и `frames.txt`: имя, мс, x, y на экране).
Окна сценария складываются по местам на экране на прозрачном холсте, холст обрезается по
содержимому, кадры берутся с ровным шагом по времени. Фон GIF прозрачный: полупрозрачные края и
свечение ложатся на `MATTE`, совсем прозрачное остаётся прозрачным. Палитра общая на все кадры —
иначе хром рамки мерцает от кадра к кадру.
"""

import os
import sys
from bisect import bisect_right

import numpy as np
from PIL import Image

# сценарий -> (ширина GIF в px или None — как есть, главное окно сценария)
SCENES = {
    "command": (760, "main"),
    "tour": (760, "main"),
    "mascot": (None, "mascot"),
    "listen": (None, "bubble"),
    "picker": (440, "picker"),
    "confirm": (440, "confirm"),
}
STEP_MS = 66  # ~15 кадров в секунду
# GIF знает только «прозрачно / непрозрачно»: пиксель с альфой не ниже порога рисуется
# поверх MATTE, ниже — прозрачный
ALPHA_CUT = 110
MATTE = (24, 28, 32)
PAD = 6


def load_window(path):
    frames = []
    with open(os.path.join(path, "frames.txt"), encoding="utf-8") as f:
        for line in f:
            name, ms, x, y = line.split()
            frames.append((int(ms), os.path.join(path, name), int(x), int(y)))
    return frames


def frame_at(frames, times, t):
    i = bisect_right(times, t) - 1
    return frames[i] if i >= 0 else None


def build(scene_dir, width, lead):
    windows = {w: load_window(os.path.join(scene_dir, w)) for w in sorted(os.listdir(scene_dir))}
    windows = {w: f for w, f in windows.items() if f}
    sizes = {}
    for w, frames in windows.items():
        with Image.open(frames[0][1]) as im:
            sizes[w] = im.size
    # холст в экранных координатах: объединение мест всех окон
    x0 = min(x for f in windows.values() for _, _, x, _ in f)
    y0 = min(y for f in windows.values() for _, _, _, y in f)
    x1 = max(x + sizes[w][0] for w, f in windows.items() for _, _, x, _ in f)
    y1 = max(y + sizes[w][1] for w, f in windows.items() for _, _, _, y in f)
    times = {w: [ms for ms, *_ in f] for w, f in windows.items()}
    start = windows[lead][0][0]
    end = windows[lead][-1][0]
    canvases = []
    cache = {}
    t = start
    while t <= end:
        canvas = Image.new("RGBA", (x1 - x0, y1 - y0), (0, 0, 0, 0))
        for w, frames in windows.items():
            got = frame_at(frames, times[w], t)
            if not got:
                continue
            _, path, x, y = got
            if path not in cache:
                cache.clear()
                cache[path] = Image.open(path).convert("RGBA")
            canvas.alpha_composite(cache[path], (x - x0, y - y0))
        canvases.append(canvas)
        t += STEP_MS
    # пустые кадры в начале и конце (окно ещё не появилось / уже ушло) — долой
    while canvases and not canvases[0].getbbox():
        canvases.pop(0)
    while canvases and not canvases[-1].getbbox():
        canvases.pop()
    # обрезка по содержимому всех кадров
    boxes = [c.getchannel("A").point(lambda a: 255 if a >= ALPHA_CUT else 0).getbbox() for c in canvases]
    boxes = [b for b in boxes if b]
    l = max(0, min(b[0] for b in boxes) - PAD)
    tp = max(0, min(b[1] for b in boxes) - PAD)
    r = min(canvases[0].width, max(b[2] for b in boxes) + PAD)
    bt = min(canvases[0].height, max(b[3] for b in boxes) + PAD)
    out = []
    for c in canvases:
        c = c.crop((l, tp, r, bt))
        if width and c.width > width:
            c = c.resize((width, round(c.height * width / c.width)), Image.LANCZOS)
        out.append(c)
    return out


def flatten(frame):
    """RGBA -> RGB поверх MATTE и маска прозрачности (True — прозрачно)."""
    bg = Image.new("RGBA", frame.size, MATTE + (255,))
    bg.alpha_composite(frame)
    clear = frame.getchannel("A").point(lambda a: 255 if a < ALPHA_CUT else 0)
    return bg.convert("RGB"), clear


def save_gif(frames, path):
    flat = [flatten(f) for f in frames]
    # общая палитра: 255 цветов по выборке кадров + индекс 255 под прозрачность
    sample = flat[:: max(1, len(flat) // 24)]
    w, h = sample[0][0].size
    mosaic = Image.new("RGB", (w, h * len(sample)))
    for i, (rgb, _) in enumerate(sample):
        mosaic.paste(rgb, (0, h * i))
    palette = mosaic.quantize(colors=255, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
    # индекс 255 — прозрачность; цвет у него такой, какого в интерфейсе нет: иначе ближайшим его
    # выбирают настоящие пиксели (с чёрным так пропадали края букв)
    pal = palette.getpalette()[: 255 * 3] + [255, 0, 255]
    palette.putpalette(pal)
    # без дизеринга: он перемешивает пиксели и в неподвижных местах, а GIF хранит только
    # изменившееся
    clears = np.stack([np.array(clear) > 0 for _, clear in flat])
    # кадр поверх предыдущего (disposal 1): неизменившиеся пиксели прозрачны. Так нельзя, если
    # пиксель из видимого становится прозрачным. Когда таких пикселей горстка (искры за рамкой,
    # край после уменьшения), они прозрачны всегда; когда много (окно появляется и уходит) —
    # каждый кадр целиком (disposal 2)
    toggling = clears.any(0) & ~clears.all(0)
    delta = toggling.mean() < 0.005
    full = []
    for rgb, clear in flat:
        p = np.array(rgb.quantize(palette=palette, dither=Image.Dither.NONE))
        p[np.array(clear) > 0] = 255
        if delta:
            p[toggling] = 255
        full.append(p)
    frames_out, durations = [], []
    prev = None
    for cur in full:
        if delta and prev is not None:
            same = cur == prev
            if same.all():
                durations[-1] += STEP_MS
                continue
            shown = cur.copy()
            shown[same] = 255
        else:
            shown = cur
        im = Image.fromarray(shown, "P")
        im.putpalette(pal)
        im.info["transparency"] = 255
        frames_out.append(im)
        durations.append(STEP_MS)
        prev = cur
    frames_out[0].save(
        path,
        save_all=True,
        append_images=frames_out[1:],
        duration=durations,
        loop=0,
        transparency=255,
        disposal=1 if delta else 2,
        optimize=False,
    )


def main():
    src = sys.argv[1]
    dst = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "..", "docs", "media")
    os.makedirs(dst, exist_ok=True)
    only = set(sys.argv[3].split(",")) if len(sys.argv) > 3 else None
    for scene, (width, lead) in SCENES.items():
        scene_dir = os.path.join(src, scene)
        if (only and scene not in only) or not os.path.isdir(scene_dir):
            continue
        frames = build(scene_dir, width, lead)
        path = os.path.join(dst, f"{scene}.gif")
        save_gif(frames, path)
        print(f"{path}: {len(frames)} frames, {frames[0].size[0]}x{frames[0].size[1]}, {os.path.getsize(path) / 1e6:.1f} MB")


if __name__ == "__main__":
    main()
