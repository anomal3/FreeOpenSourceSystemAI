#!/usr/bin/env python3
"""Генератор растровых начертаний для mini-ui.

Зачем он есть
=============

Ядру нужен текст, который не выглядит консолью 1993 года, а разбирать TrueType
внутри ядра — это парсер шрифта в no_std, то есть отдельная подсистема со своими
переполнениями. Компромисс: настоящий шрифт растеризуется **здесь**, на машине
разработчика, а в ядро попадает таблица уже готовых полутоновых картинок.

Растеризатор — FreeType через Pillow, тот же, что рисует текст в Linux. Поэтому
хинтинг, наплывы и толщина штриха на 11 точках получаются такими же, как в любой
другой программе, а не «как получилось у самодельного растеризатора».

Что на выходе
=============

`crates/mini-ui/src/typeface/data.rs` — один файл с двумя массивами байт (покрытие
глифов, по 4 бита на точку) и таблицами глифов на каждое начертание. Файл
сгенерирован, руками его не правят: правят этот скрипт и запускают

    python tools/font/genfont.py

Почему 4 бита на точку
======================

Восемь бит на точку — это 16 различимых уровней сверх того, что видно глазу на
однопиксельном штрихе, и вдвое больший образ ядра. Шестнадцать уровней покрытия
неотличимы от 256 на кегле 11–20 точек: проверялось сравнением снимков.

Почему два размерных ряда
=========================

Экран 1280×720 и экран 2560×1440 требуют разного кегля, а масштабировать
растровый глиф целым числом — значит вернуть ступеньки, ради избавления от
которых всё и затевалось. Поэтому рядов два, и каждый растеризован отдельно.
"""

import os
import sys
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
TTF = os.path.join(HERE, "ttf")
OUT = os.path.join(
    HERE, "..", "..", "crates", "mini-ui", "src", "typeface", "data.rs"
)

SANS = os.path.join(TTF, "Inter.ttf")
MONO = os.path.join(TTF, "JetBrainsMono.ttf")

# Набор знаков.
#
# Латиница и кириллица — потому что система говорит на двух языках. Стрелки,
# галочка и крестик — потому что иначе кнопка «закрыть» рисуется отдельным
# кодом вместо строки. Псевдографика — потому что её печатает `mc` и потому
# что рамка из символов должна сходиться, а не рассыпаться.
def charset():
    out = []
    out += [chr(c) for c in range(0x20, 0x7F)]          # ASCII
    out += ["Ё", "ё"]                          # Ё ё
    out += [chr(c) for c in range(0x0410, 0x0450)]       # А-я
    out += list(" «»°·×№")
    out += list("–—…‘’“”")
    out += list("←↑→↓▸◂▾▴")
    out += list("✓✗✕●○•")
    out += list("─│┌┐└┘├┤┬┴┼")
    out += list("░▒▓█▌▀▄")
    seen, uniq = set(), []
    for ch in out:
        if ch not in seen:
            seen.add(ch)
            uniq.append(ch)
    return uniq


CHARS = charset()

# Начертания: имя в Rust, файл, кегль в обычном и крупном ряду, насыщенность.
#
# Ряды подобраны по экрану: обычный — под 1280×720 и 1920×1080, крупный — под
# 2560 и выше. Кегли крупного ряда не «×1.5 и округлить», а подобраны так, чтобы
# каждый попал на целое число точек по высоте строчной буквы.
FACES = [
    # (роль,          файл, кегль обычный, кегль крупный, вес)
    ("Caption",       SANS, 11, 15, 400),
    ("Body",          SANS, 13, 18, 400),
    ("Label",         SANS, 12, 16, 500),
    ("Title",         SANS, 13, 18, 600),
    ("Strong",        SANS, 14, 19, 600),
    ("Heading",       SANS, 20, 27, 600),
    ("Mono",          MONO, 12, 16, 400),
    ("MonoSmall",     MONO, 11, 14, 400),
    ("MonoCaps",      MONO, 10, 13, 500),
]

TIERS = [("Normal", 2), ("Large", 3)]  # индекс кегля в кортеже FACES


def load(path, size, weight):
    font = ImageFont.truetype(path, size)
    axes = font.get_variation_axes()
    if axes:
        values = []
        for axis in axes:
            name = (axis["name"] or b"").decode("ascii", "replace").lower()
            if "weight" in name:
                values.append(float(weight))
            elif "optical" in name:
                # Оптический размер зажимается в собственный диапазон оси:
                # у Inter он начинается с 14, и просить 11 бессмысленно.
                values.append(float(max(axis["minimum"], min(size, axis["maximum"]))))
            else:
                values.append(float(axis["default"]))
        font.set_variation_by_axes(values)
    return font


def render(font, ch):
    """Вернуть (adv, left, top, w, h, покрытие) для одного знака.

    `top` считается вниз от верхней линии строки, а не от базовой: так рисующий
    код складывает две координаты и не хранит базовую линию отдельно.
    """
    adv = int(round(font.getlength(ch)))
    box = font.getbbox(ch)
    if box is None:
        return adv, 0, 0, 0, 0, b""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    if w <= 0 or h <= 0:
        return adv, 0, 0, 0, 0, b""
    # Рисуем с запасом и режем по рамке: якорь `la` ставит начало координат в
    # левый край на верхней линии строки, ровно там же, где его считает getbbox.
    pad = 8
    image = Image.new("L", (w + 2 * pad, h + 2 * pad), 0)
    ImageDraw.Draw(image).text((pad - x0, pad - y0), ch, font=font, fill=255)
    data = image.crop((pad, pad, pad + w, pad + h)).tobytes()
    return adv, x0, y0, w, h, data


def pack4(data, w, h):
    """Упаковать покрытие по 4 бита на точку, младшая тетрада — левая точка."""
    out = bytearray()
    for y in range(h):
        row = data[y * w : (y + 1) * w]
        for x in range(0, w, 2):
            lo = row[x] >> 4
            hi = (row[x + 1] >> 4) if x + 1 < w else 0
            out.append(lo | (hi << 4))
    return bytes(out)


def main():
    blob = bytearray()
    # Одинаковые картинки встречаются постоянно: пробел, точки, повторы между
    # начертаниями одного кегля. Общий словарь режет образ примерно на шестую.
    seen = {}
    faces = []

    for tier_name, size_index in TIERS:
        for role, path, *sizes_weight in FACES:
            size = sizes_weight[size_index - 2]
            weight = sizes_weight[-1]
            font = load(path, size, weight)
            ascent, descent = font.getmetrics()
            glyphs = []
            widths = set()
            for ch in CHARS:
                adv, left, top, w, h, data = render(font, ch)
                widths.add(adv)
                if w == 0 or h == 0:
                    off = 0
                else:
                    packed = pack4(data, w, h)
                    key = (w, h, packed)
                    if key in seen:
                        off = seen[key]
                    else:
                        off = len(blob)
                        seen[key] = off
                        blob += packed
                glyphs.append((ord(ch), adv, left, top, w, h, off))
            # Моноширинность определяется, а не объявляется: у JetBrains Mono
            # все ширины совпадают, и терминал имеет право считать по ячейкам.
            mono = min(widths) if len(widths) == 1 else 0
            faces.append(
                {
                    "name": f"{role}{tier_name}",
                    "role": role,
                    "tier": tier_name,
                    "size": size,
                    "weight": weight,
                    "line": ascent + descent,
                    "ascent": ascent,
                    "mono": mono,
                    "glyphs": glyphs,
                }
            )

    lines = []
    push = lines.append
    push("//! Таблицы начертаний. Файл сгенерирован `tools/font/genfont.py`.")
    push("//!")
    push("//! Править руками бессмысленно: следующий запуск генератора всё")
    push("//! перепишет. Менять надо набор знаков и список начертаний в скрипте.")
    push("")
    push("#![allow(clippy::unreadable_literal)]")
    push("")
    push("use super::{Face, Glyph};")
    push("")
    push(f"/// Покрытие всех глифов подряд, по 4 бита на точку.")
    push(f"pub static COVERAGE: [u8; {len(blob)}] = [")
    for i in range(0, len(blob), 32):
        chunk = ",".join(str(b) for b in blob[i : i + 32])
        push(f"    {chunk},")
    push("];")
    push("")

    for face in faces:
        name = face["name"]
        push(
            f"/// {face['role']} · {face['tier']} · "
            f"{face['size']} pt · вес {face['weight']}."
        )
        push(f"static {name.upper()}_GLYPHS: [Glyph; {len(face['glyphs'])}] = [")
        for code, adv, left, top, w, h, off in face["glyphs"]:
            push(
                f"    Glyph {{ code: {code}, adv: {adv}, left: {left}, "
                f"top: {top}, w: {w}, h: {h}, off: {off} }},"
            )
        push("];")
        push(f"pub static {name.upper()}: Face = Face {{")
        push(f"    line: {face['line']},")
        push(f"    ascent: {face['ascent']},")
        push(f"    mono: {face['mono']},")
        push(f"    glyphs: &{name.upper()}_GLYPHS,")
        push("};")
        push("")

    with open(OUT, "w", encoding="utf-8", newline="\n") as handle:
        handle.write("\n".join(lines))

    total = len(blob)
    print(f"знаков: {len(CHARS)}, начертаний: {len(faces)}")
    print(f"покрытие: {total} байт ({total / 1024:.1f} КиБ)")
    print(f"написано: {os.path.normpath(OUT)}")


if __name__ == "__main__":
    sys.exit(main())
