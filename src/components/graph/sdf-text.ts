import TinySDF from "@mapbox/tiny-sdf";
import {
  BufferImageSource,
  Container,
  Geometry,
  Mesh,
  Rectangle,
  Shader,
  Texture,
  UniformGroup,
} from "pixi.js";

/**
 * SDF text rendering for node cards (hybrid renderer experiment).
 *
 * Instead of baking text into per-node Canvas 2D textures — which
 * forces zoom-bucket texture rebuilds and ties GPU memory to node
 * count × resolution — glyphs are rasterized once into a shared
 * signed-distance-field atlas (TinySDF, single channel in alpha) and
 * each node's text becomes a static world-space quad mesh. The
 * fragment shader resolves the glyph edge with a screen-space feather
 * derived from the live zoom uniform (same convention as the edge
 * ribbon shader), so one geometry stays crisp at every zoom level with
 * zero rebuilds.
 */

// ─── Text runs ────────────────────────────────────────────────────────

/** One fillText-equivalent captured from the card painter. Coordinates
 *  are card-local CSS px with an alphabetic baseline at `y`. */
export interface TextRun {
  text: string;
  x: number;
  y: number;
  /** Font size in px (as it appeared in ctx.font). */
  px: number;
  /** CSS numeric weight, e.g. 400/600. */
  weight: number;
  italic: boolean;
  /** Resolved fill color, any CSS color string. */
  color: string;
  alpha: number;
}

// ─── Atlas ────────────────────────────────────────────────────────────

// SDF bake parameters (mapbox-gl's proven defaults, scaled up):
// glyphs are rasterized at BASE px, with RADIUS px of distance ramp
// and CUTOFF of the ramp inside the glyph. The rendered edge then sits
// at alpha = 1 - CUTOFF.
const BASE = 32;
const BUFFER = 4;
const RADIUS = 8;
const CUTOFF = 0.25;
export const SDF_EDGE = 1 - CUTOFF; // 0.75

const ATLAS_SIZE = 2048;

const MONO = "ui-monospace, SFMono-Regular, Menlo, monospace";

interface AtlasGlyph {
  /** UV rect in the atlas (normalized). */
  u0: number;
  v0: number;
  u1: number;
  v1: number;
  /** Bitmap size in atlas px (includes BUFFER on all sides). */
  w: number;
  h: number;
  /** Placement relative to the pen position (BASE-px space):
   *  top-left of the bitmap = (penX + left, baselineY - top). */
  left: number;
  top: number;
  advance: number;
}

class SdfGlyphAtlas {
  /** Single-channel SDF backing store — uploaded as an R8 texture
   *  (2048² = 4.2 MB on both sides, vs 16.8 MB as RGBA canvas). The
   *  shader samples `.r`. */
  private readonly data: Uint8Array;
  readonly texture: Texture;
  private glyphs = new Map<string, AtlasGlyph | null>();
  private sdfs = new Map<string, TinySDF>();
  private penX = 0;
  private penY = 0;
  private rowH = 0;
  private dirty = false;
  private full = false;

  constructor() {
    this.data = new Uint8Array(ATLAS_SIZE * ATLAS_SIZE);
    const source = new BufferImageSource({
      resource: this.data,
      width: ATLAS_SIZE,
      height: ATLAS_SIZE,
      format: "r8unorm",
      alphaMode: "no-premultiply-alpha",
    });
    this.texture = new Texture({ source });
  }

  private sdfFor(weight: number, italic: boolean): TinySDF {
    const key = `${weight}${italic ? "i" : ""}`;
    let sdf = this.sdfs.get(key);
    if (!sdf) {
      sdf = new TinySDF({
        fontSize: BASE,
        buffer: BUFFER,
        radius: RADIUS,
        cutoff: CUTOFF,
        fontFamily: MONO,
        fontWeight: String(weight),
        fontStyle: italic ? "italic" : "normal",
      });
      this.sdfs.set(key, sdf);
    }
    return sdf;
  }

  /** Get (baking on demand) the glyph for `char` in the given font
   *  variant. Returns null for whitespace / unbakeable glyphs. */
  glyph(char: string, weight: number, italic: boolean): AtlasGlyph | null {
    const key = `${weight}${italic ? "i" : ""}:${char}`;
    const cached = this.glyphs.get(key);
    if (cached !== undefined) return cached;

    const g = this.sdfFor(weight, italic).draw(char);
    if (g.width === 0 || g.height === 0 || this.full) {
      // Whitespace still needs an advance for the pen.
      const entry: AtlasGlyph | null =
        g.glyphAdvance > 0
          ? { u0: 0, v0: 0, u1: 0, v1: 0, w: 0, h: 0, left: 0, top: 0, advance: g.glyphAdvance }
          : null;
      this.glyphs.set(key, entry);
      return entry;
    }

    // Shelf packing.
    if (this.penX + g.width + 1 > ATLAS_SIZE) {
      this.penX = 0;
      this.penY += this.rowH + 1;
      this.rowH = 0;
    }
    if (this.penY + g.height + 1 > ATLAS_SIZE) {
      // Atlas exhausted — degrade gracefully to invisible glyphs.
      console.warn("[sdf-text] glyph atlas full; further glyphs skipped");
      this.full = true;
      this.glyphs.set(key, null);
      return null;
    }

    const x = this.penX;
    const y = this.penY;
    this.penX += g.width + 1;
    this.rowH = Math.max(this.rowH, g.height);

    // Blit the glyph's SDF rows straight into the R8 backing buffer.
    for (let row = 0; row < g.height; row++) {
      this.data.set(
        g.data.subarray(row * g.width, row * g.width + g.width),
        (y + row) * ATLAS_SIZE + x,
      );
    }
    this.dirty = true;

    const entry: AtlasGlyph = {
      u0: x / ATLAS_SIZE,
      v0: y / ATLAS_SIZE,
      u1: (x + g.width) / ATLAS_SIZE,
      v1: (y + g.height) / ATLAS_SIZE,
      w: g.width,
      h: g.height,
      left: g.glyphLeft - BUFFER,
      top: g.glyphTop + BUFFER,
      advance: g.glyphAdvance,
    };
    this.glyphs.set(key, entry);
    return entry;
  }

  /** Upload pending atlas writes to the GPU. Call once per frame after
   *  a batch of mesh builds, not per glyph. */
  flush() {
    if (!this.dirty) return;
    this.dirty = false;
    this.texture.source.update();
  }
}

let _atlas: SdfGlyphAtlas | null = null;
export function getSdfAtlas(): SdfGlyphAtlas {
  if (!_atlas) _atlas = new SdfGlyphAtlas();
  return _atlas;
}

// ─── Shader ───────────────────────────────────────────────────────────

let _textShader: Shader | null = null;
let _textUniforms: UniformGroup | null = null;

function getTextShader(): Shader {
  if (_textShader) return _textShader;
  const atlas = getSdfAtlas();
  _textUniforms = new UniformGroup({
    uZoom: { value: 1, type: "f32" },
  });
  _textShader = Shader.from({
    gl: {
      vertex: `
        attribute vec2 aPosition;
        attribute vec2 aUV;
        attribute vec4 aColor;
        attribute float aScale;
        varying vec2 vUV;
        varying vec4 vColor;
        varying float vScale;
        uniform mat3 uProjectionMatrix;
        uniform mat3 uWorldTransformMatrix;
        uniform mat3 uTransformMatrix;
        void main() {
          mat3 mvp = uProjectionMatrix * uWorldTransformMatrix * uTransformMatrix;
          gl_Position = vec4((mvp * vec3(aPosition, 1.0)).xy, 0.0, 1.0);
          vUV = aUV;
          vColor = aColor;
          vScale = aScale;
        }
      `,
      fragment: `
        precision mediump float;
        varying vec2 vUV;
        varying vec4 vColor;
        varying float vScale;
        uniform sampler2D uTexture;
        uniform vec4 uColor;
        uniform float uZoom;
        void main() {
          float d = texture2D(uTexture, vUV).r;
          // vScale = world px spanned by one SDF alpha unit. A ~0.75
          // screen-px feather converted into alpha units, clamped so
          // heavily minified text degrades to a soft fade instead of
          // exploding the smoothstep window.
          float feather = clamp(0.75 / max(0.0001, vScale * uZoom), 0.02, 0.49);
          float alpha = smoothstep(${SDF_EDGE} - feather, ${SDF_EDGE} + feather, d);
          // Coverage fade for sub-pixel glyphs (mirrors the edge
          // shader's thin-stroke dimming): when a glyph's stroke
          // drops below ~1 screen px, dim instead of shimmering.
          alpha *= min(1.0, vScale * uZoom * 4.0);
          float a = vColor.a * alpha;
          gl_FragColor = vec4(vColor.rgb * a, a) * uColor;
        }
      `,
    },
    resources: {
      uTexture: atlas.texture.source,
      uSampler: atlas.texture.source.style,
      textUniforms: _textUniforms,
    },
  });
  return _textShader;
}

/** Ticker hook: keep the text feather in sync with the current zoom.
 *  Same convention as updateEdgeZoom. */
export function updateTextZoom(k: number) {
  if (!_textUniforms) return;
  (_textUniforms.uniforms as { uZoom: number }).uZoom = k;
  _textUniforms.update();
}

// ─── Color parsing ────────────────────────────────────────────────────

const colorCache = new Map<string, [number, number, number]>();
let colorProbeCtx: CanvasRenderingContext2D | null = null;

function parseColor(color: string): [number, number, number] {
  const cached = colorCache.get(color);
  if (cached) return cached;
  let rgb: [number, number, number];
  if (/^#[0-9a-fA-F]{6}$/.test(color)) {
    const v = parseInt(color.slice(1), 16);
    rgb = [((v >> 16) & 0xff) / 255, ((v >> 8) & 0xff) / 255, (v & 0xff) / 255];
  } else {
    if (!colorProbeCtx) {
      const c = document.createElement("canvas");
      c.width = 1;
      c.height = 1;
      colorProbeCtx = c.getContext("2d", { willReadFrequently: true })!;
    }
    colorProbeCtx.fillStyle = "#000";
    colorProbeCtx.fillStyle = color;
    colorProbeCtx.fillRect(0, 0, 1, 1);
    const d = colorProbeCtx.getImageData(0, 0, 1, 1).data;
    rgb = [d[0]! / 255, d[1]! / 255, d[2]! / 255];
  }
  colorCache.set(color, rgb);
  return rgb;
}

// ─── Mesh builder ─────────────────────────────────────────────────────

/**
 * Build one static Mesh containing every glyph quad for a node card.
 * Positions are card-local px (same space the Canvas 2D painter used),
 * so the mesh is positioned at the node's top-left like its sprite.
 * Returns null when the runs contain no visible glyphs.
 */
export function buildNodeTextMesh(
  runs: TextRun[],
  boundsW: number,
  boundsH: number,
): Container | null {
  const atlas = getSdfAtlas();

  // Count quads first so buffers are allocated exactly once.
  let quadCount = 0;
  for (const run of runs) {
    for (const ch of run.text) {
      if (ch !== " ") quadCount++;
    }
  }
  if (quadCount === 0) return null;

  const positions = new Float32Array(quadCount * 8);
  const uvs = new Float32Array(quadCount * 8);
  const colors = new Float32Array(quadCount * 16);
  const scales = new Float32Array(quadCount * 4);
  const indices = new Uint32Array(quadCount * 6);

  let q = 0;
  for (const run of runs) {
    const s = run.px / BASE;
    // World px per SDF alpha unit: the distance ramp spans RADIUS
    // atlas texels, each covering `s` world px at this run's size.
    const scale = RADIUS * s;
    const [cr, cg, cb] = parseColor(run.color);
    let penX = run.x;
    for (const ch of run.text) {
      const g = atlas.glyph(ch, run.weight, run.italic);
      if (!g) continue;
      if (g.w === 0) {
        penX += g.advance * s;
        continue;
      }
      const x0 = penX + g.left * s;
      const y0 = run.y - g.top * s;
      const x1 = x0 + g.w * s;
      const y1 = y0 + g.h * s;
      penX += g.advance * s;

      const pi = q * 8;
      positions[pi] = x0; positions[pi + 1] = y0;
      positions[pi + 2] = x1; positions[pi + 3] = y0;
      positions[pi + 4] = x1; positions[pi + 5] = y1;
      positions[pi + 6] = x0; positions[pi + 7] = y1;
      uvs[pi] = g.u0; uvs[pi + 1] = g.v0;
      uvs[pi + 2] = g.u1; uvs[pi + 3] = g.v0;
      uvs[pi + 4] = g.u1; uvs[pi + 5] = g.v1;
      uvs[pi + 6] = g.u0; uvs[pi + 7] = g.v1;
      for (let v = 0; v < 4; v++) {
        const ci = q * 16 + v * 4;
        colors[ci] = cr;
        colors[ci + 1] = cg;
        colors[ci + 2] = cb;
        colors[ci + 3] = run.alpha;
        scales[q * 4 + v] = scale;
      }
      const ii = q * 6;
      const base = q * 4;
      indices[ii] = base;
      indices[ii + 1] = base + 1;
      indices[ii + 2] = base + 2;
      indices[ii + 3] = base;
      indices[ii + 4] = base + 2;
      indices[ii + 5] = base + 3;
      q++;
    }
  }
  if (q === 0) return null;

  const geometry = new Geometry({
    attributes: {
      aPosition: { buffer: positions.subarray(0, q * 8), format: "float32x2" },
      aUV: { buffer: uvs.subarray(0, q * 8), format: "float32x2" },
      aColor: { buffer: colors.subarray(0, q * 16), format: "float32x4" },
      aScale: { buffer: scales.subarray(0, q * 4), format: "float32" },
    },
    indexBuffer: indices.subarray(0, q * 6),
  });
  const mesh = new Mesh({ geometry, shader: getTextShader() });
  mesh.once("destroyed", () => geometry.destroy());
  mesh.cullable = true;
  mesh.boundsArea = new Rectangle(0, 0, boundsW, boundsH);
  return mesh;
}

/** Upload any glyphs baked during this frame's mesh builds. */
export function flushSdfAtlas() {
  _atlas?.flush();
}
