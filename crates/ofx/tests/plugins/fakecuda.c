/* A stand-in for NVIDIA's CUDA driver library, for testing Reelforge's
   OpenFX CUDA path on machines without an NVIDIA card: the driver calls the
   host makes, with "device memory" that is ordinary memory, and a record of
   what was done so the tests (and the test plugin) can check it. Not shipped. */
#include <stdlib.h>
#include <string.h>

typedef unsigned long long CUdeviceptr;
#define MAX 256
static void *live[MAX];
static int allocs, frees, syncs, depth;
static int stream_obj;
static int ctx_obj;

static int find(void *p) {
  for (int i = 0; i < MAX; ++i) if (live[i] == p) return i;
  return -1;
}

int cuInit(unsigned flags) { (void)flags; return 0; }
int cuDeviceGetCount(int *n) { *n = 1; return 0; }
int cuDeviceGet(int *d, int i) { *d = i; return 0; }
int cuDeviceGetName(char *name, int len, int d) { (void)d; strncpy(name, "Reelforge stand-in CUDA device", len); return 0; }
int cuDevicePrimaryCtxRetain(void **ctx, int d) { (void)d; *ctx = &ctx_obj; return 0; }
int cuCtxPushCurrent_v2(void *ctx) { if (ctx != &ctx_obj) return 201; ++depth; return 0; }
int cuCtxPopCurrent_v2(void **ctx) { if (depth <= 0) return 201; --depth; if (ctx) *ctx = &ctx_obj; return 0; }
int cuCtxSynchronize(void) { ++syncs; return depth > 0 ? 0 : 201; }
int cuMemAlloc_v2(CUdeviceptr *p, size_t n) {
  if (depth <= 0) return 201; /* no context current */
  int i = find(NULL);
  if (i < 0) return 2;
  live[i] = malloc(n);
  *p = (CUdeviceptr)live[i];
  ++allocs;
  return 0;
}
int cuMemFree_v2(CUdeviceptr p) {
  int i = find((void *)p);
  if (i < 0) return 1;
  free(live[i]);
  live[i] = NULL;
  ++frees;
  return 0;
}
int cuMemcpyHtoD_v2(CUdeviceptr d, const void *s, size_t n) { if (find((void *)d) < 0) return 1; memcpy((void *)d, s, n); return 0; }
int cuMemcpyDtoH_v2(void *d, CUdeviceptr s, size_t n) { if (find((void *)s) < 0) return 1; memcpy(d, (void *)s, n); return 0; }
int cuStreamCreate(void **s, unsigned flags) { (void)flags; *s = &stream_obj; return 0; }
int cuStreamSynchronize(void *s) { ++syncs; return s == &stream_obj ? 0 : 400; }

/* For the tests. */
int rf_fake_cuda_is_device(const void *p) { return find((void *)p) >= 0; }
int rf_fake_cuda_context_current(void) { return depth > 0; }
void *rf_fake_cuda_stream(void) { return &stream_obj; }
void rf_fake_cuda_stats(int *a, int *f, int *s) { *a = allocs; *f = frees; *s = syncs; }
