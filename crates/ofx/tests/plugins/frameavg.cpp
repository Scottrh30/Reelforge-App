// A test plugin for Reelforge's OpenFX host (not shipped): averages each
// frame with its neighbours, `radius` frames either side, so it reads other
// frames of its source. It says which with GetFramesNeeded, and averages only
// the frames the host could give (so a missing one shows in the result).
// Float RGBA only. Written for these tests against the OpenFX headers.

#include <cstring>
#include <vector>
#include "ofxImageEffect.h"

static OfxHost *host = nullptr;
static OfxPropertySuiteV1 *props = nullptr;
static OfxImageEffectSuiteV1 *fx = nullptr;
static OfxParameterSuiteV1 *params = nullptr;

static OfxStatus load() {
  props = (OfxPropertySuiteV1 *)host->fetchSuite(host->host, kOfxPropertySuite, 1);
  fx = (OfxImageEffectSuiteV1 *)host->fetchSuite(host->host, kOfxImageEffectSuite, 1);
  params = (OfxParameterSuiteV1 *)host->fetchSuite(host->host, kOfxParameterSuite, 1);
  return props && fx && params ? kOfxStatOK : kOfxStatErrMissingHostFeature;
}

static OfxStatus describe(OfxImageEffectHandle e) {
  OfxPropertySetHandle p;
  fx->getPropertySet(e, &p);
  props->propSetString(p, kOfxPropLabel, 0, "Test Frame Average");
  props->propSetString(p, kOfxImageEffectPluginPropGrouping, 0, "Reelforge Tests");
  props->propSetString(p, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
  props->propSetString(p, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthFloat);
  props->propSetInt(p, kOfxImageEffectPropTemporalClipAccess, 0, 1);
  return kOfxStatOK;
}

static OfxStatus describeInContext(OfxImageEffectHandle e) {
  OfxPropertySetHandle p;
  fx->clipDefine(e, kOfxImageEffectOutputClipName, &p);
  props->propSetString(p, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
  fx->clipDefine(e, kOfxImageEffectSimpleSourceClipName, &p);
  props->propSetString(p, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
  props->propSetInt(p, kOfxImageEffectPropTemporalClipAccess, 0, 1);
  OfxParamSetHandle ps;
  fx->getParamSet(e, &ps);
  params->paramDefine(ps, kOfxParamTypeInteger, "radius", &p);
  props->propSetString(p, kOfxPropLabel, 0, "Radius");
  props->propSetInt(p, kOfxParamPropDefault, 0, 1);
  props->propSetInt(p, kOfxParamPropMin, 0, 0);
  props->propSetInt(p, kOfxParamPropMax, 0, 8);
  return kOfxStatOK;
}

static int radius(OfxImageEffectHandle e) {
  OfxParamSetHandle ps;
  fx->getParamSet(e, &ps);
  OfxParamHandle r;
  params->paramGetHandle(ps, "radius", &r, nullptr);
  int v = 1;
  params->paramGetValue(r, &v);
  return v;
}

static OfxStatus framesNeeded(OfxImageEffectHandle e, OfxPropertySetHandle in, OfxPropertySetHandle out) {
  double t = 0;
  props->propGetDouble(in, kOfxPropTime, 0, &t);
  int r = radius(e);
  double range[2] = {t - r, t + r};
  props->propSetDoubleN(out, "OfxImageClipPropFrameRange_Source", 2, range);
  return kOfxStatOK;
}

static OfxStatus render(OfxImageEffectHandle e, OfxPropertySetHandle in) {
  double t = 0;
  props->propGetDouble(in, kOfxPropTime, 0, &t);
  OfxImageClipHandle src, dst;
  fx->clipGetHandle(e, kOfxImageEffectSimpleSourceClipName, &src, nullptr);
  fx->clipGetHandle(e, kOfxImageEffectOutputClipName, &dst, nullptr);
  OfxPropertySetHandle out;
  if (fx->clipGetImage(dst, t, nullptr, &out) != kOfxStatOK) return kOfxStatFailed;
  void *outData;
  int outRow, b[4];
  props->propGetPointer(out, kOfxImagePropData, 0, &outData);
  props->propGetInt(out, kOfxImagePropRowBytes, 0, &outRow);
  props->propGetIntN(out, kOfxImagePropBounds, 4, b);
  int w = b[2] - b[0], h = b[3] - b[1];
  std::vector<float> sum((size_t)w * h * 4, 0.0f);
  int n = 0, r = radius(e);
  for (int k = -r; k <= r; ++k) {
    OfxPropertySetHandle img;
    if (fx->clipGetImage(src, t + k, nullptr, &img) != kOfxStatOK) continue;
    void *data;
    int row;
    props->propGetPointer(img, kOfxImagePropData, 0, &data);
    props->propGetInt(img, kOfxImagePropRowBytes, 0, &row);
    for (int y = 0; y < h; ++y) {
      const float *s = (const float *)((const char *)data + (size_t)y * row);
      for (int x = 0; x < w * 4; ++x) sum[(size_t)y * w * 4 + x] += s[x];
    }
    fx->clipReleaseImage(img);
    ++n;
  }
  for (int y = 0; y < h; ++y) {
    float *d = (float *)((char *)outData + (size_t)y * outRow);
    for (int x = 0; x < w * 4; ++x) d[x] = n ? sum[(size_t)y * w * 4 + x] / n : 0.0f;
  }
  fx->clipReleaseImage(out);
  return kOfxStatOK;
}

static OfxStatus mainEntry(const char *action, const void *handle, OfxPropertySetHandle in, OfxPropertySetHandle out) {
  OfxImageEffectHandle e = (OfxImageEffectHandle)handle;
  if (!strcmp(action, kOfxActionLoad)) return load();
  if (!strcmp(action, kOfxActionDescribe)) return describe(e);
  if (!strcmp(action, kOfxImageEffectActionDescribeInContext)) return describeInContext(e);
  if (!strcmp(action, kOfxImageEffectActionGetFramesNeeded)) return framesNeeded(e, in, out);
  if (!strcmp(action, kOfxImageEffectActionRender)) return render(e, in);
  return kOfxStatReplyDefault;
}

static void setHost(OfxHost *h) { host = h; }

static OfxPlugin plugin = {kOfxImageEffectPluginApi, 1, "com.reelforge.test.FrameAverage", 1, 0, setHost, mainEntry};

extern "C" {
OfxExport int OfxGetNumberOfPlugins(void) { return 1; }
OfxExport OfxPlugin *OfxGetPlugin(int) { return &plugin; }
}
