/* Рисование чужой графической библиотекой: cairo поверх pixman, libpng и
 * freetype (фаза 61a).
 *
 * # Что она доказывает
 *
 * Что стопка, на которой стоит `System.Drawing` Mono (libgdiplus рисует через
 * cairo), собрана нашим набором и **рисует правильно** — а не только
 * компонуется. Каждая проверка смотрит в конкретные точки картинки и на
 * сломанной системе пройти не может:
 *
 * - залитый прямоугольник — ровно своего цвета внутри и не задевает соседей;
 * - круг со сглаживанием: центр залит целиком, точка на краю — **частично**
 *   (прозрачность строго между нулём и 255: без сглаживания pixman её бы не
 *   было, со сломанным — не было бы заливки);
 * - линейный градиент: слева красный, справа синий, посередине — смесь;
 * - PNG: картинка записана через libpng в файл и прочитана обратно — байт в
 *   байт та же;
 * - текст: глиф, отрисованный freetype, закрашивает точки в своей рамке и
 *   только в ней (шрифт — второй аргумент). По умолчанию — DejaVu Sans, который кладёт в
 *   систему установщик.
 *
 * Итог — одна строка с числом проваленных; ноль и только ноль — всё прошло.
 */

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <cairo.h>
#include <cairo-ft.h>
#include <ft2build.h>
#include FT_FREETYPE_H

#define W 64
#define H 48

static int failures;

static void check(int passed, const char *what) {
    if (passed) {
        printf("cdraw: ok %s\n", what);
    } else {
        printf("cdraw: FAILED %s\n", what);
        failures++;
    }
}

/* Точка картинки как ARGB32 (так её держит cairo: предумноженная альфа). */
static uint32_t pixel(cairo_surface_t *surface, int x, int y) {
    unsigned char *data = cairo_image_surface_get_data(surface);
    int stride = cairo_image_surface_get_stride(surface);
    return *(uint32_t *)(data + y * stride + x * 4);
}

#define ALPHA(p) (((p) >> 24) & 0xff)
#define RED(p) (((p) >> 16) & 0xff)
#define GREEN(p) (((p) >> 8) & 0xff)
#define BLUE(p) ((p) & 0xff)

static void check_shapes(cairo_surface_t *surface) {
    cairo_t *cr = cairo_create(surface);

    /* Прямоугольник: зелёный, по целым точкам. */
    cairo_set_source_rgb(cr, 0, 1, 0);
    cairo_rectangle(cr, 4, 4, 10, 8);
    cairo_fill(cr);
    uint32_t inside = pixel(surface, 8, 8);
    uint32_t outside = pixel(surface, 15, 8);
    printf("cdraw: rectangle %08x, beside it %08x, cairo says %s\n", inside, outside,
           cairo_status_to_string(cairo_status(cr)));
    check(inside == 0xff00ff00u && outside == 0, "a filled rectangle keeps to its edges");

    /* Круг со сглаживанием. */
    cairo_set_source_rgb(cr, 1, 0, 0);
    cairo_arc(cr, 40, 12, 8, 0, 2 * 3.14159265358979);
    cairo_fill(cr);
    uint32_t centre = pixel(surface, 40, 12);
    /* Точка, через которую проходит край круга: x = 40 + 8·cos(45°) ≈ 45.66. */
    uint32_t edge = pixel(surface, 45, 17);
    printf("cdraw: circle centre %08x, edge %08x\n", centre, edge);
    check(centre == 0xffff0000u && ALPHA(edge) > 0 && ALPHA(edge) < 255 && GREEN(edge) == 0,
          "an antialiased circle has a solid centre and a soft edge");

    /* Линейный градиент слева направо: красный -> синий. */
    cairo_pattern_t *ramp = cairo_pattern_create_linear(0, 0, W, 0);
    cairo_pattern_add_color_stop_rgb(ramp, 0, 1, 0, 0);
    cairo_pattern_add_color_stop_rgb(ramp, 1, 0, 0, 1);
    cairo_set_source(cr, ramp);
    cairo_rectangle(cr, 0, 30, W, 4);
    cairo_fill(cr);
    cairo_pattern_destroy(ramp);
    uint32_t left = pixel(surface, 0, 31);
    uint32_t middle = pixel(surface, W / 2, 31);
    uint32_t right = pixel(surface, W - 1, 31);
    printf("cdraw: gradient %08x %08x %08x, cairo says %s\n", left, middle, right,
           cairo_status_to_string(cairo_status(cr)));
    check(RED(left) > 240 && BLUE(left) < 15 && BLUE(right) > 240 && RED(right) < 15 && RED(middle) > 100 &&
              RED(middle) < 160 && BLUE(middle) > 100 && BLUE(middle) < 160,
          "a linear gradient runs from red to blue");

    cairo_destroy(cr);
}

static void check_png(cairo_surface_t *surface, const char *path) {
    cairo_status_t wrote = cairo_surface_write_to_png(surface, path);
    cairo_surface_t *back = cairo_image_surface_create_from_png(path);
    int same = wrote == CAIRO_STATUS_SUCCESS && cairo_surface_status(back) == CAIRO_STATUS_SUCCESS &&
               cairo_image_surface_get_width(back) == W && cairo_image_surface_get_height(back) == H;
    if (same) {
        cairo_surface_flush(surface);
        for (int y = 0; y < H && same; y++) {
            for (int x = 0; x < W; x++) {
                if (pixel(surface, x, y) != pixel(back, x, y)) {
                    printf("cdraw: png differs at %d,%d: %08x vs %08x\n", x, y, pixel(surface, x, y),
                           pixel(back, x, y));
                    same = 0;
                    break;
                }
            }
        }
    } else {
        printf("cdraw: png write %d, read %d\n", wrote, cairo_surface_status(back));
    }
    cairo_surface_destroy(back);
    check(same, "a PNG written by libpng reads back pixel for pixel");
}

static void check_text(const char *font) {
    FT_Library library;
    FT_Face face;
    if (FT_Init_FreeType(&library) != 0 || FT_New_Face(library, font, 0, &face) != 0) {
        printf("cdraw: no font at %s, text skipped\n", font);
        failures++;
        return;
    }
    cairo_surface_t *surface = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, W, H);
    cairo_t *cr = cairo_create(surface);
    cairo_font_face_t *cface = cairo_ft_font_face_create_for_ft_face(face, 0);
    cairo_set_font_face(cr, cface);
    cairo_set_font_size(cr, 32);
    cairo_set_source_rgb(cr, 0, 0, 0);
    cairo_move_to(cr, 8, 40);
    cairo_show_text(cr, "H");
    cairo_surface_flush(surface);

    /* Глиф «H» кегля 32 — две вертикальные штанги и перекладина: где-то внутри
     * его рамки точки закрашены, а в углу картинки — нет. */
    int inked = 0;
    for (int y = 10; y < 40; y++) {
        for (int x = 8; x < 40; x++) {
            if (ALPHA(pixel(surface, x, y)) > 200) {
                inked++;
            }
        }
    }
    uint32_t corner = pixel(surface, W - 1, 0);
    printf("cdraw: glyph inked %d points, cairo says %s\n", inked, cairo_status_to_string(cairo_status(cr)));
    check(inked > 50 && corner == 0, "freetype draws a glyph inside its box");

    cairo_destroy(cr);
    cairo_font_face_destroy(cface);
    cairo_surface_destroy(surface);
    FT_Done_Face(face);
    FT_Done_FreeType(library);
}

#include <pixman.h>

/* pixman напрямую: заливка и композиция без маски. Её реализация создаётся
 * конструктором (`__attribute__((constructor))`), и до фазы 61 конструкторы
 * программ на C не вызывались вовсе — заливка отвечала «не умею», а
 * композиция молча ничего не писала. Круг со сглаживанием при этом рисовался:
 * он идёт через маску и другой путь. */
static void check_pixman(void) {
    static uint32_t filled[16 * 16];
    static uint32_t composed[16 * 16];
    pixman_bool_t answered = pixman_fill(filled, 16, 32, 2, 2, 4, 4, 0xff00ff00u);
    pixman_image_t *dst = pixman_image_create_bits(PIXMAN_a8r8g8b8, 16, 16, composed, 64);
    pixman_color_t green = {0, 0xffff, 0, 0xffff};
    pixman_image_t *src = pixman_image_create_solid_fill(&green);
    pixman_image_composite32(PIXMAN_OP_SRC, src, NULL, dst, 0, 0, 0, 0, 2, 2, 4, 4);
    pixman_image_unref(src);
    pixman_image_unref(dst);
    check(answered && filled[3 * 16 + 3] == 0xff00ff00u && composed[3 * 16 + 3] == 0xff00ff00u,
          "pixman fills and composites on its own");
}

int main(int argc, char **argv) {
    const char *png = argc > 1 ? argv[1] : "/home/roman/cdraw.png";
    const char *font = argc > 2 ? argv[2] : "/usr/share/fonts/dejavu/DejaVuSans.ttf";
    printf("cdraw: cairo %s, pixman %s\n", cairo_version_string(), "0.42");

    check_pixman();
    cairo_surface_t *surface = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, W, H);
    check_shapes(surface);
    check_png(surface, png);
    cairo_surface_destroy(surface);
    if (font != NULL) {
        check_text(font);
    } else {
        printf("cdraw: no font given, text skipped\n");
    }

    printf("cdraw: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
