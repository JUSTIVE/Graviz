import { Link } from "@tanstack/react-router";
import { Button } from "@/components/ui/button";

interface TechItem {
  title: string;
  description: string;
}

const RENDER_TECHNIQUES: TechItem[] = [
  {
    title: "PixiJS WebGL renderer",
    description:
      "The graph is drawn with pixi.js v8 on a WebGL2 context with framebuffer MSAA turned off — anti-aliasing is done analytically in shaders instead, freeing multisample fill-rate across the whole canvas (the biggest fixed GPU cost at 120 fps).",
  },
  {
    title: "Analytic-AA edge meshes",
    description:
      "Edges are triangle-strip ribbons with anti-aliasing computed in the fragment shader: a screen-space alpha feather derived from the live zoom uniform, so one static mesh stays crisp at every zoom level. Styles that used to be dashed are distinguished by alpha contrast instead — no per-dash vertex spam.",
  },
  {
    title: "Feathered arrowhead sprites",
    description:
      "Arrowheads are tinted sprites sharing a single feathered-triangle texture — smooth without MSAA, and coalesced into one draw by Pixi's sprite batcher regardless of arrow count.",
  },
  {
    title: "Sprite texture cache",
    description:
      "Each node card (header, name, field rows) is pre-rendered once to a Canvas 2D offscreen surface, uploaded as a WebGL texture, and displayed as a GPU Sprite. Cache is keyed by node ID + LOD tier + resolution bucket.",
  },
  {
    title: "Refresh-rate-aware frame budgets",
    description:
      "Sprite and edge-tile builds are budget-limited per frame, with the budget derived from the display's actual refresh rate — roughly 5 ms at 60 Hz, tightening to ~3 ms at 120 Hz. Nodes start as instant solid-color placeholders; real textures fill in across subsequent frames so no single frame blocks.",
  },
  {
    title: "Level of Detail (LOD) with hysteresis",
    description:
      "Three rendering tiers based on zoom: full (text + icons), bar (colored placeholder bars), chrome (solid-color box only). Tier boundaries carry hysteresis so parking the zoom on a threshold can't oscillate rebuilds.",
  },
  {
    title: "Zoom-proportional texture resolution",
    description:
      "Full-LOD texture resolution follows the effective zoom × device pixel ratio, bucketed to a ×2 ladder (0.5–8) with √2 midpoint thresholds — text progressively sharpens as you zoom in, ordinary zoom jitter never re-keys textures, and bar/chrome placeholders stay at DPR 1.",
  },
  {
    title: "World-space edge tiling",
    description:
      "Edges are partitioned into 2048-px world tiles; each tile's mesh is built lazily when it enters the viewport, a budgeted number of batches per frame. Sprites mirror the strategy with a node tile index, so viewport sweeps never touch off-screen work.",
  },
  {
    title: "Render-on-demand",
    description:
      "The ticker stops entirely after a run of idle frames — an untouched canvas costs zero GPU/CPU. Any interaction, animation, or pending build restarts it.",
  },
  {
    title: "Spatial hit-testing",
    description:
      "Edge hover uses the edge-tile index for early rejection, then squared point-to-polyline distance over a flattened-bezier polyline cached per edge in a WeakMap — per-mousemove hit tests without sqrt or re-flattening.",
  },
  {
    title: "TilingSprite dot grid",
    description:
      "The background dot grid is a GPU TilingSprite with a tiny dot texture. One draw call covers the entire viewport; tilePosition and tileScale sync with pan/zoom in the ticker.",
  },
  {
    title: "measureText cache",
    description:
      "Field type widths and fitText results are cached in module-level Maps keyed by string content. Cold sprite builds avoid redundant measureText calls — 8,000+ calls reduced to unique-type count.",
  },
];

const GRAPH_TECHNIQUES: TechItem[] = [
  {
    title: "GraphViz WASM layout",
    description:
      "Layout runs the dot engine (network-simplex ranker) compiled to WebAssembly via @viz-js/viz. The native C implementation handles 400+ nodes in under a second — the JS port (dagre) stalled at that scale.",
  },
  {
    title: "Parallel component layout",
    description:
      "The schema is split into weakly-connected components — no edges cross component boundaries, so laying them out independently is exact, not an approximation. Non-trivial components are dispatched across a pool of Web Workers; singleton types skip dot entirely and get a grid placement; the per-component bounding boxes are shelf-packed at the end.",
  },
  {
    title: "Recursive chunking for hub-heavy schemas",
    description:
      "Components beyond ~500 nodes / 4,000 edges are recursively bisected into sub-chunks, each laid out as its own dot invocation, with cross-chunk edges drawn as straight segments after packing. This keeps GraphViz's WASM heap inside the browser's per-tab budget — Relay-style Node interfaces with 100+ implementors used to abort here.",
  },
  {
    title: "Persistent layout cache",
    description:
      "Finished layouts are stored in IndexedDB keyed by a content hash of the layout-relevant inputs. Pasting the same SDL again returns the identical, already-laid-out result in ~0–2 ms.",
  },
  {
    title: "Native cubic bezier edges",
    description:
      "Edge paths use GraphViz's cubic bezier control points directly via bezierCurveTo — no polyline sampling. Canvas dashing works cleanly on bezier segments without the arcTo artifacts that sampled polylines produced.",
  },
  {
    title: "BFS reachability with implements back-traversal",
    description:
      "Reachable types are found via BFS over field/union/arg/implements edges. Implements edges flow Interface → ConcreteType, so visiting an interface surfaces its implementors directly; a reverse adjacency handles the case where a concrete type is visited first and needs to climb back to its interface.",
  },
  {
    title: "Relay Connection unwrapping",
    description:
      "Connection, Edge, PageInfo, and Node boilerplate types are detected structurally and collapsed. Field edges skip straight to the underlying payload type so the graph stays readable.",
  },
  {
    title: "Union member adjacency hints",
    description:
      "For each Union type, member pairs are emitted as invisible non-constraining GraphViz edges (constraint=false, style=invis). This biases crossing reduction to place union members adjacent without affecting ranks.",
  },
  {
    title: "Root type override",
    description:
      "An explicit schema { query: QueryRoot } definition (or extend schema) wins outright over the default Query / Mutation / Subscription names — schemas with custom root operation types map correctly.",
  },
  {
    title: "Deprecation sunset parsing",
    description:
      "@deprecated reasons carrying an [until YYYY-MM-DD] marker are parsed as sunset dates. Fields past their date are flagged expired (red), dated ones upcoming, and the rest plain deprecated — all collected in the Deprecated tab.",
  },
];

function Section({ title, items }: { title: string; items: TechItem[] }) {
  return (
    <section className="mt-8">
      <h2 className="text-base font-semibold">{title}</h2>
      <ul className="mt-3 space-y-4">
        {items.map((item) => (
          <li key={item.title} className="grid grid-cols-[1fr] gap-0.5 sm:grid-cols-[200px_1fr] sm:gap-4">
            <span className="font-mono text-xs font-medium text-foreground pt-0.5">{item.title}</span>
            <span className="text-sm text-muted-foreground leading-relaxed">{item.description}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function AboutRoute() {
  return (
    <div className="mx-auto w-full max-w-2xl flex-1 overflow-auto px-6 py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Graviz</h1>
      <p className="mt-3 text-muted-foreground">
        Paste a GraphQL SDL and get an interactive map of your schema.
        The renderer is a PixiJS WebGL engine — no DOM nodes per type,
        no React re-renders per frame.
      </p>

      <div className="mt-6 grid gap-2 text-sm">
        <div>
          <span className="font-medium">Stack — </span>
          <span className="text-muted-foreground">
            Bun · React 19 · TanStack Router · Tailwind v4 · graphql (SDL parser)
          </span>
        </div>
        <div>
          <span className="font-medium">Rendering — </span>
          <span className="text-muted-foreground">
            PixiJS v8 (WebGL2, shader-AA edge meshes) · Canvas 2D sprite textures · GraphViz WASM layout (worker pool)
          </span>
        </div>
      </div>

      <Section title="Rendering optimizations" items={RENDER_TECHNIQUES} />
      <Section title="Graph & layout techniques" items={GRAPH_TECHNIQUES} />

      <section className="mt-8">
        <h2 className="text-base font-semibold">Tips</h2>
        <ul className="mt-3 list-inside list-disc space-y-1.5 text-sm text-muted-foreground">
          <li>Pick a root operation in the left panel, then click field types to drill in.</li>
          <li>Breadcrumbs let you jump back up the navigation stack.</li>
          <li>Scroll to zoom, drag to pan. Pinch-zoom works on touch devices.</li>
          <li><kbd className="font-mono text-xs">Cmd+K</kbd> focuses the search bar. Use <code className="font-mono text-xs">Type.field</code> syntax for two-phase matching.</li>
          <li>Toggle "Hide primitives" to collapse scalar fields and reduce visual noise.</li>
          <li>The Orphaned tab shows types unreachable from any root operation.</li>
          <li>The Deprecated tab lists every <code className="font-mono text-xs">@deprecated</code> field — expired <code className="font-mono text-xs">[until]</code> dates in red, upcoming ones in amber.</li>
          <li>Drag the sidebar's right edge to resize it, or collapse it entirely for a full-width canvas.</li>
          <li>Deprecated fields and enum values get a yellow highlight in the tree panel.</li>
          <li>The real-time FPS chart in the bottom-right corner shows rendering performance.</li>
        </ul>
      </section>

      <div className="mt-10 flex gap-2">
        <Button asChild>
          <Link to="/">Start visualizing</Link>
        </Button>
      </div>
    </div>
  );
}
