// A test plugin for Reelforge's OpenFX host (not shipped): multiplies the
// colour by 2. When the host renders it with CUDA it checks what it's given
// against the stand-in CUDA driver the tests load (REELFORGE_CUDA_DRIVER):
// device memory, the context current, the host's stream. A real CUDA plugin
// would launch kernels here; the stand-in's "device memory" is ordinary
// memory, so this one just works on it. Float RGBA only.

#include <cstdlib>
#include <cstring>
#include <dlfcn.h>
#include "ofxImageEffect.h"
#include "ofxGPURender.h"

static OfxHost *host = nullptr;
static OfxPropertySuiteV1 *props = nullptr;
static OfxImageEffectSuiteV1 *fx = nullptr;

static OfxStatus describe(OfxImageEffectHandle e) {
  props = (OfxPropertySuiteV1 *)host->fetchSuite(host->host, kOfxPropertySuite, 1);
  fx = (OfxImageEffectSuiteV1 *)host->fetchSuite(host->host, kOfxImageEffectSuite, 1);
  OfxPropertySetHandle p;
  fx->getPropertySet(e, &p);
  props->propSetString(p, kOfxPropLabel, 0, "Test CUDA Gain");
  props->propSetString(p, kOfxImageEffectPluginPropGrouping, 0, "Reelforge Tests");
  props->propSetString(p, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
  props->propSetString(p, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthFloat);
  props->propSetString(p, kOfxImageEffectPropCudaRenderSupported, 0, "true");
  props->propSetString(p, kOfxImageEffectPropCudaStreamSupported, 0, "true");
  return kOfxStatOK;
}

static OfxStatus describeInContext(OfxImageEffectHandle e) {
  OfxPropertySetHandle p;
  fx->clipDefine(e, kOfxImageEffectOutputClipName, &p);
  props->propSetString(p, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
  fx->clipDefine(e, kOfxImageEffectSimpleSourceClipName, &p);
  props->propSetString(p, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
  return kOfxStatOK;
}

template <class F> static F fake(const char *name) {
  const char *lib = getenv("REELFORGE_CUDA_DRIVER");
  void *h = lib ? dlopen(lib, RTLD_NOW | RTLD_NOLOAD) : nullptr;
  return h ? (F)dlsym(h, name) : nullptr;
}

static OfxStatus render(OfxImageEffectHandle e, OfxPropertySetHandle in) {
  double t = 0;
  props->propGetDouble(in, kOfxPropTime, 0, &t);
  int cuda = 0;
  props->propGetInt(in, kOfxImageEffectPropCudaEnabled, 0, &cuda);
  OfxImageClipHandle src, dst;
  fx->clipGetHandle(e, kOfxImageEffectSimpleSourceClipName, &src, nullptr);
  fx->clipGetHandle(e, kOfxImageEffectOutputClipName, &dst, nullptr);
  OfxPropertySetHandle si, di;
  if (fx->clipGetImage(src, t, nullptr, &si) != kOfxStatOK) return kOfxStatFailed;
  if (fx->clipGetImage(dst, t, nullptr, &di) != kOfxStatOK) return kOfxStatFailed;
  void *s, *d;
  int sr, dr, b[4];
  props->propGetPointer(si, kOfxImagePropData, 0, &s);
  props->propGetPointer(di, kOfxImagePropData, 0, &d);
  props->propGetInt(si, kOfxImagePropRowBytes, 0, &sr);
  props->propGetInt(di, kOfxImagePropRowBytes, 0, &dr);
  props->propGetIntN(di, kOfxImagePropBounds, 4, b);
  OfxStatus st = kOfxStatOK;
  if (cuda) {
    auto is_device = fake<int (*)(const void *)>("rf_fake_cuda_is_device");
    auto current = fake<int (*)()>("rf_fake_cuda_context_current");
    auto stream = fake<void *(*)()>("rf_fake_cuda_stream");
    void *given = nullptr;
    props->propGetPointer(in, kOfxImageEffectPropCudaStream, 0, &given);
    if (!is_device || !is_device(s) || !is_device(d)) st = kOfxStatErrValue;       // not device memory
    else if (!current || !current()) st = kOfxStatErrBadHandle;                     // no context current
    else if (!stream || given != stream()) st = kOfxStatErrBadIndex;                // not the host's stream
  }
  if (st == kOfxStatOK) {
    for (int y = 0; y < b[3] - b[1]; ++y) {
      const float *a = (const float *)((const char *)s + (size_t)y * sr);
      float *o = (float *)((char *)d + (size_t)y * dr);
      for (int x = 0; x < (b[2] - b[0]) * 4; ++x) o[x] = x % 4 == 3 ? a[x] : a[x] * 2.0f;
    }
  }
  fx->clipReleaseImage(si);
  fx->clipReleaseImage(di);
  return st;
}

static OfxStatus mainEntry(const char *action, const void *handle, OfxPropertySetHandle in, OfxPropertySetHandle) {
  OfxImageEffectHandle e = (OfxImageEffectHandle)handle;
  if (!strcmp(action, kOfxActionDescribe)) return describe(e);
  if (!strcmp(action, kOfxImageEffectActionDescribeInContext)) return describeInContext(e);
  if (!strcmp(action, kOfxImageEffectActionRender)) return render(e, in);
  return kOfxStatReplyDefault;
}

static void setHost(OfxHost *h) { host = h; }
static OfxPlugin plugin = {kOfxImageEffectPluginApi, 1, "com.reelforge.test.CudaGain", 1, 0, setHost, mainEntry};

extern "C" {
OfxExport int OfxGetNumberOfPlugins(void) { return 1; }
OfxExport OfxPlugin *OfxGetPlugin(int) { return &plugin; }
}
