// Make CineForm test clips with GoPro's SDK: cfenc W H FRAMES fmt(yuv|rgb|rgba) gop(1|2) quality(1-6) interlaced(0|1) out.cfs
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <vector>
#include <cmath>
#include "CFHDTypes.h"
#include "CFHDError.h"
#include "CFHDEncoder.h"
int main(int argc, char **argv) {
  if (argc < 9) { fprintf(stderr, "usage\n"); return 2; }
  int w = atoi(argv[1]), h = atoi(argv[2]), n = atoi(argv[3]);
  const char *fmt = argv[4]; int gop = atoi(argv[5]); int q = atoi(argv[6]); int il = atoi(argv[7]);
  CFHD_EncoderRef enc; CFHD_Error e = CFHD_OpenEncoder(&enc, NULL);
  if (e) { fprintf(stderr, "open %d\n", e); return 1; }
  CFHD_PixelFormat pf; CFHD_EncodedFormat ef; int bpp;
  if (!strcmp(fmt, "yuv")) { pf = CFHD_PIXEL_FORMAT_YUY2; ef = CFHD_ENCODED_FORMAT_YUV_422; bpp = 2; }
  else if (!strcmp(fmt, "rgb")) { pf = CFHD_PIXEL_FORMAT_RG48; ef = CFHD_ENCODED_FORMAT_RGB_444; bpp = 6; }
  else { pf = CFHD_PIXEL_FORMAT_RG64; ef = CFHD_ENCODED_FORMAT_RGBA_4444; bpp = 8; }
  CFHD_EncodingFlags flags = CFHD_ENCODING_FLAGS_NONE;
  if (gop == 2) flags = (CFHD_EncodingFlags)(flags | CFHD_ENCODING_FLAGS_YUV_2FRAME_GOP);
  CFHD_EncodingQuality quality = (CFHD_EncodingQuality)q;
  if (il) flags = (CFHD_EncodingFlags)(flags | CFHD_ENCODING_FLAGS_YUV_INTERLACED);
  e = CFHD_PrepareToEncode(enc, w, h, pf, ef, flags, quality);
  if (e) { fprintf(stderr, "prepare %d\n", e); return 1; }
  int pitch = w * bpp;
  std::vector<uint8_t> buf(pitch * h);
  FILE *o = fopen(argv[8], "wb");
  for (int f = 0; f < n; f++) {
    for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
      double fx = (double)x / w, fy = (double)y / h;
      int inbox = (x > 40 + f * 7 && x < 140 + f * 7 && y > 30 + f * 3 && y < 110 + f * 3);
      double r = 0.5 + 0.45 * sin(fx * 6.0 + f * 0.3), g = fy, b = 0.5 + 0.4 * cos((fx + fy) * 9.0 - f * 0.2);
      if (inbox) { r = 0.95; g = 0.1; b = 0.2; }
      if (((x / 8) + (y / 8)) % 2 == 0 && x > w * 3 / 4 && y > h * 2 / 3) { r = g = b = 0.9; }
      double a = 0.5 + 0.5 * sin(fx * 4.0 + fy * 3.0 + f * 0.1);
      uint8_t *p = &buf[y * pitch + x * bpp];
      if (bpp == 2) {
        double Y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        double cb = (b - Y) / 1.8556, cr = (r - Y) / 1.5748;
        p[0] = (uint8_t)(16 + 219 * Y);
        p[1] = (uint8_t)(128 + 224 * ((x & 1) ? cr : cb));
      } else {
        uint16_t *s = (uint16_t *)p;
        if (bpp == 6) { s[0] = (uint16_t)(r * 65535); s[1] = (uint16_t)(g * 65535); s[2] = (uint16_t)(b * 65535); }
        else { s[0] = (uint16_t)(r * 65535); s[1] = (uint16_t)(g * 65535); s[2] = (uint16_t)(b * 65535); s[3] = (uint16_t)(a * 65535); }
      }
    }
    e = CFHD_EncodeSample(enc, buf.data(), pitch);
    if (e) { fprintf(stderr, "encode %d\n", e); return 1; }
    void *data; size_t size;
    e = CFHD_GetSampleData(enc, &data, &size);
    if (e) { fprintf(stderr, "get %d\n", e); return 1; }
    uint32_t s32 = (uint32_t)size; fwrite(&s32, 4, 1, o); fwrite(data, 1, size, o);
  }
  fclose(o);
  CFHD_CloseEncoder(enc);
  return 0;
}
