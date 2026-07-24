/**
 * Edge-ribbon tessellation benchmark.
 *
 * Reproduces the hot per-vertex loop from SchemaCanvas.tsx
 * `buildEdgeBatchMesh` (:1287-1313) on synthetic polyline data at the
 * "millions of vertices" scale the renderer targets, and compares:
 *
 *   js-baseline   current code, Math.hypot per vertex
 *   js-sqrt       Math.hypot -> sqrt(dx*dx+dy*dy) (the free win)
 *   js-tight      + hoisted inner side branch, fewer array writes
 *   wasm-scalar   kernel.c compiled without SIMD  (if built)
 *   wasm-simd     kernel.c compiled with -msimd128 (if built)
 *
 * All variants run over identical input and their outputs are checked
 * against the baseline so we compare correct implementations only.
 *
 * Run: bun run bench/tess/bench.ts
 */

// ─── Synthetic input: a batch of polylines mirroring flattened edges ──
// Real edges are bezier paths sampled at EDGE_SAMPLE_STEPS=16 per
// segment; a typical edge has 2-4 segments -> ~33-65 points. We build a
// batch whose total point count lands in the millions-of-vertices range.

const HALF_W = 2.2;

function buildBatch(edgeCount: number): { pts: Float64Array; counts: Int32Array; totalPts: number } {
  // Deterministic LCG so runs are comparable without Math.random noise.
  let seed = 0x9e3779b9 >>> 0;
  const rnd = () => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 0xffffffff;
  };

  const counts = new Int32Array(edgeCount);
  let total = 0;
  for (let e = 0; e < edgeCount; e++) {
    const segs = 2 + Math.floor(rnd() * 3); // 2..4 segments
    const n = segs * 16 + 1;
    counts[e] = n;
    total += n;
  }

  const pts = new Float64Array(total * 2);
  let o = 0;
  for (let e = 0; e < edgeCount; e++) {
    const n = counts[e]!;
    let x = rnd() * 4000;
    let y = rnd() * 4000;
    let vx = (rnd() - 0.5) * 40;
    let vy = (rnd() - 0.5) * 40;
    for (let i = 0; i < n; i++) {
      pts[o++] = x;
      pts[o++] = y;
      // wander like a sampled bezier: gently curving path
      vx += (rnd() - 0.5) * 4;
      vy += (rnd() - 0.5) * 4;
      x += vx;
      y += vy;
    }
  }
  return { pts, counts, totalPts: total };
}

// ─── JS variants ─────────────────────────────────────────────────────

// Faithful port of the current code path (Math.hypot per vertex).
function jsBaseline(
  pts: Float64Array, counts: Int32Array, hw: number,
  positions: Float32Array, dists: Float32Array,
): number {
  let ptBase = 0;
  let vi = 0;
  for (let p = 0; p < counts.length; p++) {
    const n = counts[p]!;
    if (n >= 2) {
      const off = ptBase * 2;
      for (let i = 0; i < n; i++) {
        const x = pts[off + 2 * i]!;
        const y = pts[off + 2 * i + 1]!;
        const iPrev = Math.max(0, i - 1);
        const iNext = Math.min(n - 1, i + 1);
        let dx = pts[off + 2 * iNext]! - pts[off + 2 * iPrev]!;
        let dy = pts[off + 2 * iNext + 1]! - pts[off + 2 * iPrev + 1]!;
        const len = Math.hypot(dx, dy) || 1;
        dx /= len;
        dy /= len;
        const nx = -dy;
        const ny = dx;
        for (let side = 0; side < 2; side++) {
          const sgn = side === 0 ? 1 : -1;
          positions[vi * 2] = x + nx * hw * sgn;
          positions[vi * 2 + 1] = y + ny * hw * sgn;
          dists[vi] = hw * sgn;
          vi++;
        }
      }
    }
    ptBase += n;
  }
  return vi;
}

// Free win: drop Math.hypot for an inline sqrt (no overflow-safety cost).
function jsSqrt(
  pts: Float64Array, counts: Int32Array, hw: number,
  positions: Float32Array, dists: Float32Array,
): number {
  let ptBase = 0;
  let vi = 0;
  for (let p = 0; p < counts.length; p++) {
    const n = counts[p]!;
    if (n >= 2) {
      const off = ptBase * 2;
      for (let i = 0; i < n; i++) {
        const x = pts[off + 2 * i]!;
        const y = pts[off + 2 * i + 1]!;
        const iPrev = i > 0 ? i - 1 : 0;
        const iNext = i < n - 1 ? i + 1 : n - 1;
        let dx = pts[off + 2 * iNext]! - pts[off + 2 * iPrev]!;
        let dy = pts[off + 2 * iNext + 1]! - pts[off + 2 * iPrev + 1]!;
        let len = Math.sqrt(dx * dx + dy * dy);
        if (len === 0) len = 1;
        const inv = 1 / len;
        dx *= inv;
        dy *= inv;
        const nx = -dy;
        const ny = dx;
        for (let side = 0; side < 2; side++) {
          const sgn = side === 0 ? 1 : -1;
          positions[vi * 2] = x + nx * hw * sgn;
          positions[vi * 2 + 1] = y + ny * hw * sgn;
          dists[vi] = hw * sgn;
          vi++;
        }
      }
    }
    ptBase += n;
  }
  return vi;
}

// Tightened: unrolled two sides, hoisted offsets, minimal index math.
function jsTight(
  pts: Float64Array, counts: Int32Array, hw: number,
  positions: Float32Array, dists: Float32Array,
): number {
  let ptBase = 0;
  let vi = 0;
  for (let p = 0; p < counts.length; p++) {
    const n = counts[p]!;
    if (n >= 2) {
      const off = ptBase * 2;
      for (let i = 0; i < n; i++) {
        const b = off + 2 * i;
        const x = pts[b]!;
        const y = pts[b + 1]!;
        const pv = i > 0 ? b - 2 : b;
        const nx2 = i < n - 1 ? b + 2 : b;
        let dx = pts[nx2]! - pts[pv]!;
        let dy = pts[nx2 + 1]! - pts[pv + 1]!;
        let len = Math.sqrt(dx * dx + dy * dy);
        if (len === 0) len = 1;
        const s = hw / len;
        const ox = -dy * s;
        const oy = dx * s;
        const j = vi * 2;
        positions[j] = x + ox;
        positions[j + 1] = y + oy;
        positions[j + 2] = x - ox;
        positions[j + 3] = y - oy;
        dists[vi] = hw;
        dists[vi + 1] = -hw;
        vi += 2;
      }
    }
    ptBase += n;
  }
  return vi;
}

// ─── WASM loader ─────────────────────────────────────────────────────

interface WasmKernel {
  memory: WebAssembly.Memory;
  ptsPtr: number;
  positionsPtr: number;
  distsPtr: number;
  countsPtr: number;
  tessellateBatch(count: number, hw: number): void;
}

async function loadKernel(path: string): Promise<WasmKernel | null> {
  const file = Bun.file(path);
  if (!(await file.exists())) return null;
  const bytes = await file.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  const ex = instance.exports as Record<string, any>;
  return {
    memory: ex.memory as WebAssembly.Memory,
    ptsPtr: ex.pts_ptr(),
    positionsPtr: ex.positions_ptr(),
    distsPtr: ex.dists_ptr(),
    countsPtr: ex.counts_ptr(),
    tessellateBatch: ex.tessellate_batch,
  };
}

// ─── Verification ────────────────────────────────────────────────────

function maxAbsDiff(a: Float32Array, b: Float32Array, len: number): number {
  let m = 0;
  for (let i = 0; i < len; i++) {
    const d = Math.abs(a[i]! - b[i]!);
    if (d > m) m = d;
  }
  return m;
}

// ─── Timing ──────────────────────────────────────────────────────────

function time(label: string, iters: number, fn: () => void): number {
  // warmup
  for (let i = 0; i < 3; i++) fn();
  const t0 = performance.now();
  for (let i = 0; i < iters; i++) fn();
  const t1 = performance.now();
  const ms = (t1 - t0) / iters;
  return ms;
}

// ─── Main ────────────────────────────────────────────────────────────

const EDGE_COUNT = Number(process.env.EDGES ?? 40_000);
const ITERS = Number(process.env.ITERS ?? 20);

console.log(`Building batch: ${EDGE_COUNT.toLocaleString()} edges ...`);
const { pts, counts, totalPts } = buildBatch(EDGE_COUNT);
const outVerts = totalPts * 2;
console.log(
  `  ${totalPts.toLocaleString()} polyline points -> ${outVerts.toLocaleString()} ribbon vertices`,
);
console.log(`  iters/measure: ${ITERS}\n`);

const positions = new Float32Array(outVerts * 2);
const dists = new Float32Array(outVerts);

// Reference output from baseline.
const refPos = new Float32Array(outVerts * 2);
const refDist = new Float32Array(outVerts);
jsBaseline(pts, counts, HALF_W, refPos, refDist);

const rows: Array<{ name: string; ms: number; diff: string }> = [];

function record(name: string, ms: number, diff: number | null) {
  rows.push({ name, ms, diff: diff == null ? "-" : diff.toExponential(1) });
}

// JS variants
let ms = time("js-baseline", ITERS, () => jsBaseline(pts, counts, HALF_W, positions, dists));
record("js-baseline", ms, 0);

ms = time("js-sqrt", ITERS, () => jsSqrt(pts, counts, HALF_W, positions, dists));
record("js-sqrt", ms, maxAbsDiff(positions, refPos, outVerts * 2));

ms = time("js-tight", ITERS, () => jsTight(pts, counts, HALF_W, positions, dists));
record("js-tight", ms, maxAbsDiff(positions, refPos, outVerts * 2));

// WASM variants
for (const [name, path] of [
  ["wasm-scalar", `${import.meta.dir}/kernel_scalar.wasm`],
  ["wasm-simd", `${import.meta.dir}/kernel_simd.wasm`],
] as const) {
  const k = await loadKernel(path);
  if (!k) {
    console.log(`(skip ${name}: ${path.split("/").pop()} not built)`);
    continue;
  }
  // kernel.c static buffers are sized for MAX_PTS = 2Mi points.
  const MAX_PTS = 2 * 1024 * 1024;
  if (totalPts > MAX_PTS) {
    console.log(`(skip ${name}: ${totalPts.toLocaleString()} pts > wasm buffer ${MAX_PTS.toLocaleString()}; lower EDGES)`);
    continue;
  }
  // Load input into wasm memory once (this is a one-time upload, matching
  // the real code where geometry is built then handed to the GPU).
  const f32 = new Float32Array(k.memory.buffer, k.ptsPtr, totalPts * 2);
  for (let i = 0; i < totalPts * 2; i++) f32[i] = pts[i]!;
  const i32 = new Int32Array(k.memory.buffer, k.countsPtr, counts.length);
  i32.set(counts);

  const outPos = new Float32Array(k.memory.buffer, k.positionsPtr, outVerts * 2);
  ms = time(name, ITERS, () => k.tessellateBatch(counts.length, HALF_W));
  // wasm uses f32 inputs vs js f64, so tolerance is f32 epsilon-ish
  record(name, ms, maxAbsDiff(outPos, refPos, outVerts * 2));
}

// ─── Report ──────────────────────────────────────────────────────────

const base = rows[0]!.ms;
console.log("\n" + "name".padEnd(14) + "ms/iter".padStart(10) + "speedup".padStart(10) + "  maxdiff");
console.log("─".repeat(48));
for (const r of rows) {
  const speedup = (base / r.ms).toFixed(2) + "x";
  console.log(r.name.padEnd(14) + r.ms.toFixed(3).padStart(10) + speedup.padStart(10) + "  " + r.diff);
}
const mvps = (outVerts / 1e6 / (base / 1000)).toFixed(1);
console.log(`\nbaseline throughput: ${mvps}M vertices/s`);
