/* The OpenFX functions that take a variable number of arguments, which Rust
 * can't define: they unpack the arguments and call back into the Rust host
 * through the function pointers it hands over with rf_ofx_shim_init. */
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

typedef int OfxStatus;

typedef struct {
    /* What a parameter holds: 1 = doubles, 2 = ints, 3 = a string (0 = not a
     * parameter); `dims` gets how many values. */
    int (*kind)(void *param, int *dims);
    /* Its values at `time` (NaN = now). */
    OfxStatus (*get)(void *param, double time, double *d, int *i, const char **s);
    OfxStatus (*set)(void *param, const double *d, const int *i, const char *s);
    void (*message)(const char *type, const char *text, int persistent);
} RfCallbacks;

static RfCallbacks cb;

void rf_ofx_shim_init(const RfCallbacks *c) { cb = *c; }

#define NOW (__builtin_nan(""))

static OfxStatus get_into(void *h, double t, va_list ap) {
    int dims = 0;
    int k = cb.kind ? cb.kind(h, &dims) : 0;
    if (!k) return 9; /* kOfxStatErrBadHandle */
    double d[4] = {0, 0, 0, 0};
    int iv[4] = {0, 0, 0, 0};
    const char *s = "";
    OfxStatus st = cb.get(h, t, d, iv, &s);
    if (st) return st;
    for (int j = 0; j < dims && j < 4; j++) {
        if (k == 1) { double *p = va_arg(ap, double *); if (p) *p = d[j]; }
        else if (k == 2) { int *p = va_arg(ap, int *); if (p) *p = iv[j]; }
        else { const char **p = va_arg(ap, const char **); if (p) *p = s; }
    }
    return 0;
}

OfxStatus rf_ofx_param_get_value(void *h, ...) {
    va_list ap; va_start(ap, h);
    OfxStatus r = get_into(h, NOW, ap);
    va_end(ap); return r;
}

OfxStatus rf_ofx_param_get_value_at_time(void *h, double time, ...) {
    va_list ap; va_start(ap, time);
    OfxStatus r = get_into(h, time, ap);
    va_end(ap); return r;
}

/* Parameters don't change within one render: no slope. */
OfxStatus rf_ofx_param_get_derivative(void *h, double time, ...) {
    int dims = 0;
    int k = cb.kind ? cb.kind(h, &dims) : 0;
    if (k != 1) return k ? 5 : 9;
    va_list ap; va_start(ap, time);
    for (int j = 0; j < dims; j++) { double *p = va_arg(ap, double *); if (p) *p = 0.0; }
    va_end(ap); return 0;
}

/* The value times the span (it's constant over it). */
OfxStatus rf_ofx_param_get_integral(void *h, double t1, double t2, ...) {
    int dims = 0;
    int k = cb.kind ? cb.kind(h, &dims) : 0;
    if (k != 1) return k ? 5 : 9;
    double d[4] = {0, 0, 0, 0}; int iv[4]; const char *s;
    OfxStatus st = cb.get(h, t1, d, iv, &s);
    if (st) return st;
    va_list ap; va_start(ap, t2);
    for (int j = 0; j < dims; j++) { double *p = va_arg(ap, double *); if (p) *p = d[j] * (t2 - t1); }
    va_end(ap); return 0;
}

static OfxStatus set_from(void *h, va_list ap) {
    int dims = 0;
    int k = cb.kind ? cb.kind(h, &dims) : 0;
    if (!k) return 9;
    double d[4] = {0, 0, 0, 0}; int iv[4] = {0, 0, 0, 0}; const char *s = 0;
    for (int j = 0; j < dims && j < 4; j++) {
        if (k == 1) d[j] = va_arg(ap, double);
        else if (k == 2) iv[j] = va_arg(ap, int);
        else s = va_arg(ap, const char *);
    }
    return cb.set(h, d, iv, s);
}

OfxStatus rf_ofx_param_set_value(void *h, ...) {
    va_list ap; va_start(ap, h);
    OfxStatus r = set_from(h, ap);
    va_end(ap); return r;
}

OfxStatus rf_ofx_param_set_value_at_time(void *h, double time, ...) {
    (void)time;
    va_list ap; va_start(ap, time);
    OfxStatus r = set_from(h, ap);
    va_end(ap); return r;
}

/* ---- Messages ---- */

static void say(const char *type, int persistent, const char *format, va_list ap) {
    char text[2048];
    vsnprintf(text, sizeof text, format ? format : "", ap);
    if (cb.message) cb.message(type ? type : "", text, persistent);
}

OfxStatus rf_ofx_message(void *handle, const char *type, const char *id, const char *format, ...) {
    (void)handle; (void)id;
    va_list ap; va_start(ap, format);
    say(type, 0, format, ap);
    va_end(ap);
    /* A question ("OfxMessageQuestion") gets the default answer. */
    return (type && strcmp(type, "OfxMessageQuestion") == 0) ? 14 /* kOfxStatReplyDefault */ : 0;
}

OfxStatus rf_ofx_set_persistent_message(void *handle, const char *type, const char *id, const char *format, ...) {
    (void)handle; (void)id;
    va_list ap; va_start(ap, format);
    say(type, 1, format, ap);
    va_end(ap); return 0;
}

static OfxStatus clear_persistent(void *handle) { (void)handle; return 0; }

typedef struct { void *message; } MessageSuiteV1;
typedef struct { void *message; void *set_persistent; void *clear_persistent; } MessageSuiteV2;
static MessageSuiteV1 message_v1 = { (void *)rf_ofx_message };
static MessageSuiteV2 message_v2 = { (void *)rf_ofx_message, (void *)rf_ofx_set_persistent_message, (void *)clear_persistent };

const void *rf_ofx_message_suite(int version) {
    return version == 1 ? (const void *)&message_v1 : version == 2 ? (const void *)&message_v2 : 0;
}
