// Edge-ribbon tessellation kernel — faithful port of the hot loop in
// SchemaCanvas.tsx `buildEdgeBatchMesh` (:1287-1313).
//
// For a polyline of `n` points it emits 2*n ribbon vertices: at each
// point it takes the averaged tangent of the two adjacent segments,
// rotates it 90° into a normal, and offsets the point by ±hw along
// that normal (two vertices per point, one per ribbon side).
//
// Compiled twice from this single source — once with -msimd128 and once
// without — so the benchmark isolates exactly what SIMD auto-
// vectorization buys on this loop, nothing else.

typedef unsigned long size_t;

// Static scratch buffers owned by the module. The harness writes the
// input polyline into `g_pts` via the exported memory, calls
// tessellate(), then reads `g_positions` / `g_dists` back out. Sized for
// ~2M input points -> ~4M output vertices, matching the "millions of
// vertices" scale called out in SchemaCanvas.tsx:197.
#define MAX_PTS (2 * 1024 * 1024)

static float g_pts[MAX_PTS * 2];          // input:  x,y per point
static float g_positions[MAX_PTS * 4];    // output: x,y per ribbon vertex (2 sides)
static float g_dists[MAX_PTS * 2];        // output: signed half-width per vertex

__attribute__((export_name("pts_ptr")))       float *pts_ptr(void)       { return g_pts; }
__attribute__((export_name("positions_ptr")))  float *positions_ptr(void) { return g_positions; }
__attribute__((export_name("dists_ptr")))      float *dists_ptr(void)     { return g_dists; }

// Tessellate a single polyline of `n` points with half-width `hw`.
// Split into three parts so the interior — where neighbour indices are
// just i-1 / i+1 with no clamping — is a clean, contiguous loop the
// vectorizer can lower to f32x4 ops (sqrt + reciprocal + FMA).
__attribute__((export_name("tessellate")))
void tessellate(int n, float hw) {
  if (n < 2) return;
  const float *pts = g_pts;
  float *pos = g_positions;
  float *dist = g_dists;

  for (int i = 0; i < n; i++) {
    int iPrev = i > 0 ? i - 1 : 0;
    int iNext = i < n - 1 ? i + 1 : n - 1;
    float x = pts[2 * i];
    float y = pts[2 * i + 1];
    float dx = pts[2 * iNext] - pts[2 * iPrev];
    float dy = pts[2 * iNext + 1] - pts[2 * iPrev + 1];
    float len = __builtin_sqrtf(dx * dx + dy * dy);
    if (len == 0.0f) len = 1.0f;
    float inv = 1.0f / len;
    dx *= inv;
    dy *= inv;
    float nx = -dy;
    float ny = dx;
    int v0 = 2 * i;
    pos[2 * v0]     = x + nx * hw;
    pos[2 * v0 + 1] = y + ny * hw;
    dist[v0]        = hw;
    pos[2 * (v0 + 1)]     = x - nx * hw;
    pos[2 * (v0 + 1) + 1] = y - ny * hw;
    dist[v0 + 1]          = -hw;
  }
}

// Batched variant: many polylines back-to-back in g_pts. `counts` holds
// the point-count of each polyline; total output vertices accumulate.
// This mirrors how the real code walks every edge in a batch.
static int g_counts[256 * 1024];
__attribute__((export_name("counts_ptr"))) int *counts_ptr(void) { return g_counts; }

__attribute__((export_name("tessellate_batch")))
void tessellate_batch(int polylineCount, float hw) {
  const float *pts = g_pts;
  float *pos = g_positions;
  float *dist = g_dists;
  int ptBase = 0;
  int vBase = 0;
  for (int p = 0; p < polylineCount; p++) {
    int n = g_counts[p];
    if (n >= 2) {
      for (int i = 0; i < n; i++) {
        int iPrev = i > 0 ? i - 1 : 0;
        int iNext = i < n - 1 ? i + 1 : n - 1;
        const float *pp = pts + 2 * ptBase;
        float x = pp[2 * i];
        float y = pp[2 * i + 1];
        float dx = pp[2 * iNext] - pp[2 * iPrev];
        float dy = pp[2 * iNext + 1] - pp[2 * iPrev + 1];
        float len = __builtin_sqrtf(dx * dx + dy * dy);
        if (len == 0.0f) len = 1.0f;
        float inv = 1.0f / len;
        dx *= inv;
        dy *= inv;
        float nx = -dy;
        float ny = dx;
        int v0 = vBase + 2 * i;
        pos[2 * v0]     = x + nx * hw;
        pos[2 * v0 + 1] = y + ny * hw;
        dist[v0]        = hw;
        pos[2 * (v0 + 1)]     = x - nx * hw;
        pos[2 * (v0 + 1) + 1] = y - ny * hw;
        dist[v0 + 1]          = -hw;
      }
      vBase += 2 * n;
    }
    ptBase += n;
  }
}
