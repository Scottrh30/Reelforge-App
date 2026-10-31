// A test plugin for Reelforge's OpenFX host (not shipped): multiplies the
// colour by `gain` with an OpenCL kernel when the host renders it with
// OpenCL, on the processor otherwise. Built three ways for the tests:
//   RF_IMAGES=0: OpenCL buffers;  RF_IMAGES=1: OpenCL images;
//   RF_CPU=0: OpenCL only (says "needed").
// Float RGBA only. Written for these tests against the OpenFX headers.

#include <cstring>
#include <map>
#include <CL/cl.h>
#include "ofxImageEffect.h"
#include "ofxGPURender.h"

#ifndef RF_IMAGES
#define RF_IMAGES 0
#endif
#ifndef RF_CPU
#define RF_CPU 1
#endif

static OfxHost *host = nullptr;
static OfxPropertySuiteV1 *props = nullptr;
static OfxImageEffectSuiteV1 *fx = nullptr;
static OfxParameterSuiteV1 *params = nullptr;
static OfxOpenCLProgramSuiteV1 *programs = nullptr;
// Kernels per OpenCL context, as the standard asks.
static std::map<cl_context, cl_kernel> kernels;

static const char *SOURCE =
    "__kernel void gain_buf(__global const float4 *src, __global float4 *dst, float g) {\n"
    "  size_t i = get_global_id(0); float4 p = src[i]; dst[i] = (float4)(p.xyz * g, p.w);\n"
    "}\n"
    "__kernel void gain_img(__read_only image2d_t src, __write_only image2d_t dst, float g) {\n"
    "  int2 at = (int2)(get_global_id(0), get_global_id(1));\n"
    "  float4 p = read_imagef(src, CLK_NORMALIZED_COORDS_FALSE | CLK_ADDRESS_CLAMP_TO_EDGE | CLK_FILTER_NEAREST, at);\n"
    "  write_imagef(dst, at, (float4)(p.xyz * g, p.w));\n"
    "}\n";

static OfxStatus load() {
  props = (OfxPropertySuiteV1 *)host->fetchSuite(host->host, kOfxPropertySuite, 1);
  fx = (OfxImageEffectSuiteV1 *)host->fetchSuite(host->host, kOfxImageEffectSuite, 1);
  params = (OfxParameterSuiteV1 *)host->fetchSuite(host->host, kOfxParameterSuite, 1);
  programs = (OfxOpenCLProgramSuiteV1 *)host->fetchSuite(host->host, kOfxOpenCLProgramSuite, 1);
  return props && fx && params ? kOfxStatOK : kOfxStatErrMissingHostFeature;
}

static OfxStatus describe(OfxImageEffectHandle e) {
  OfxPropertySetHandle p;
  fx->getPropertySet(e, &p);
  props->propSetString(p, kOfxPropLabel, 0, RF_IMAGES ? "Test OpenCL Gain (images)" : RF_CPU ? "Test OpenCL Gain" : "Test OpenCL Gain (OpenCL only)");
  props->propSetString(p, kOfxImageEffectPluginPropGrouping, 0, "Reelforge Tests");
  props->propSetString(p, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
  props->propSetString(p, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthFloat);
  // Buffers are "RenderSupported", images "Supported".
  props->propSetString(p, RF_IMAGES ? kOfxImageEffectPropOpenCLSupported : kOfxImageEffectPropOpenCLRenderSupported, 0, RF_CPU ? "true" : "needed");
  props->propSetString(p, kOfxImageEffectPropCPURenderSupported, 0, RF_CPU ? "true" : "false");
  return kOfxStatOK;
}

static OfxStatus describeInContext(OfxImageEffectHandle e) {
  OfxPropertySetHandle p;
  fx->clipDefine(e, kOfxImageEffectOutputClipName, &p);
  props->propSetString(p, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
  fx->clipDefine(e, kOfxImageEffectSimpleSourceClipName, &p);
  props->propSetString(p, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
  OfxParamSetHandle ps;
  fx->getParamSet(e, &ps);
  params->paramDefine(ps, kOfxParamTypeDouble, "gain", &p);
  props->propSetDouble(p, kOfxParamPropDefault, 0, 2.0);
  return kOfxStatOK;
}

static double gain(OfxImageEffectHandle e) {
  OfxParamSetHandle ps;
  fx->getParamSet(e, &ps);
  OfxParamHandle h;
  params->paramGetHandle(ps, "gain", &h, nullptr);
  double g = 1;
  params->paramGetValue(h, &g);
  return g;
}

static cl_kernel kernel(cl_command_queue q) {
  cl_context ctx;
  clGetCommandQueueInfo(q, CL_QUEUE_CONTEXT, sizeof ctx, &ctx, nullptr);
  auto it = kernels.find(ctx);
  if (it != kernels.end()) return it->second;
  cl_program prog = nullptr;
  if (programs) {
    // The host's program suite builds it on its context.
    if (programs->compileProgram(SOURCE, 0, &prog) != kOfxStatOK) return nullptr;
  } else {
    cl_int err;
    prog = clCreateProgramWithSource(ctx, 1, &SOURCE, nullptr, &err);
    if (err || clBuildProgram(prog, 0, nullptr, nullptr, nullptr, nullptr)) return nullptr;
  }
  cl_int err;
  cl_kernel k = clCreateKernel(prog, RF_IMAGES ? "gain_img" : "gain_buf", &err);
  if (err) return nullptr;
  kernels[ctx] = k;
  return k;
}

static OfxStatus render(OfxImageEffectHandle e, OfxPropertySetHandle in) {
  double t = 0;
  props->propGetDouble(in, kOfxPropTime, 0, &t);
  int cl = 0;
  props->propGetInt(in, kOfxImageEffectPropOpenCLEnabled, 0, &cl);
  if (!cl && !RF_CPU) return kOfxStatErrUnsupported;
  OfxImageClipHandle src, dst;
  fx->clipGetHandle(e, kOfxImageEffectSimpleSourceClipName, &src, nullptr);
  fx->clipGetHandle(e, kOfxImageEffectOutputClipName, &dst, nullptr);
  OfxPropertySetHandle si, di;
  if (fx->clipGetImage(src, t, nullptr, &si) != kOfxStatOK) return kOfxStatFailed;
  if (fx->clipGetImage(dst, t, nullptr, &di) != kOfxStatOK) return kOfxStatFailed;
  int b[4];
  props->propGetIntN(di, kOfxImagePropBounds, 4, b);
  size_t w = b[2] - b[0], h = b[3] - b[1];
  float g = (float)gain(e);
  OfxStatus st = kOfxStatOK;
  if (cl) {
    void *q = nullptr, *s = nullptr, *d = nullptr;
    props->propGetPointer(in, kOfxImageEffectPropOpenCLCommandQueue, 0, &q);
    const char *which = RF_IMAGES ? kOfxImageEffectPropOpenCLImage : kOfxImagePropData;
    props->propGetPointer(si, which, 0, &s);
    props->propGetPointer(di, which, 0, &d);
    cl_kernel k = q ? kernel((cl_command_queue)q) : nullptr;
    if (!k || !s || !d) {
      st = kOfxStatFailed;
    } else {
      cl_mem sm = (cl_mem)s, dm = (cl_mem)d;
      clSetKernelArg(k, 0, sizeof sm, &sm);
      clSetKernelArg(k, 1, sizeof dm, &dm);
      clSetKernelArg(k, 2, sizeof g, &g);
      size_t global[2] = {RF_IMAGES ? w : w * h, h};
      // Enqueued on the host's queue; the host waits for it.
      if (clEnqueueNDRangeKernel((cl_command_queue)q, k, RF_IMAGES ? 2 : 1, nullptr, global, nullptr, 0, nullptr, nullptr)) st = kOfxStatFailed;
    }
  } else {
    void *s, *d;
    int sr, dr;
    props->propGetPointer(si, kOfxImagePropData, 0, &s);
    props->propGetPointer(di, kOfxImagePropData, 0, &d);
    props->propGetInt(si, kOfxImagePropRowBytes, 0, &sr);
    props->propGetInt(di, kOfxImagePropRowBytes, 0, &dr);
    for (size_t y = 0; y < h; ++y) {
      const float *a = (const float *)((const char *)s + y * sr);
      float *o = (float *)((char *)d + y * dr);
      for (size_t x = 0; x < w; ++x)
        for (int c = 0; c < 4; ++c) o[x * 4 + c] = c < 3 ? a[x * 4 + c] * g : a[x * 4 + c];
    }
  }
  fx->clipReleaseImage(si);
  fx->clipReleaseImage(di);
  return st;
}

static OfxStatus mainEntry(const char *action, const void *handle, OfxPropertySetHandle in, OfxPropertySetHandle) {
  OfxImageEffectHandle e = (OfxImageEffectHandle)handle;
  if (!strcmp(action, kOfxActionLoad)) return load();
  if (!strcmp(action, kOfxActionDescribe)) return describe(e);
  if (!strcmp(action, kOfxImageEffectActionDescribeInContext)) return describeInContext(e);
  if (!strcmp(action, kOfxImageEffectActionRender)) return render(e, in);
  return kOfxStatReplyDefault;
}

static void setHost(OfxHost *h) { host = h; }

#define STR2(x) #x
#define STR(x) STR2(x)
static OfxPlugin plugin = {kOfxImageEffectPluginApi, 1, "com.reelforge.test.OpenCLGain." STR(RF_IMAGES) STR(RF_CPU), 1, 0, setHost, mainEntry};

extern "C" {
OfxExport int OfxGetNumberOfPlugins(void) { return 1; }
OfxExport OfxPlugin *OfxGetPlugin(int) { return &plugin; }
}
