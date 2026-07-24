# 엣지 tessellation SIMD 실험

`SchemaCanvas.tsx`의 `buildEdgeBatchMesh` (엣지 리본 버텍스 빌드) hot loop이
typed array / SIMD로 빨라질 여지가 있는지 실측한 실험.

## 무엇을 재는가

폴리라인 각 점에서 접선→법선→±half-width offset을 계산해 리본 버텍스를
만드는 per-vertex 루프(대형 스키마에서 수백만 버텍스)를 동일 입력으로 5가지
구현으로 측정한다. 모든 변형의 출력은 baseline과 대조해 정확성을 검증한다.

- `js-baseline` — 현재 코드 경로 (`Math.hypot` per vertex)
- `js-sqrt` — `Math.hypot` → 인라인 `sqrt(dx*dx+dy*dy)` (무료 승리)
- `js-tight` — + 양쪽 side 언롤, offset 선계산
- `wasm-scalar` — `kernel.c`를 SIMD 없이 컴파일
- `wasm-simd` — `kernel.c`를 `-msimd128`로 컴파일 (`f32x4` 자동 벡터화, 바이너리에 0xfd SIMD opcode 178개 확인)

## 결과 (M-series, Bun 1.3.11, 40k edges → 3.9M 버텍스, 50 iters)

| 구현 | ms/iter | speedup | maxdiff |
|---|---|---|---|
| js-baseline | 19.2 | 1.00× | 0 |
| js-sqrt | 6.9 | **2.79×** | 0 |
| js-tight | 4.5 | 4.30× | 0 |
| wasm-scalar | 3.0 | 6.39× | 5.5e-3 (f32) |
| wasm-simd | 3.4 | 5.64× | 5.5e-3 (f32) |

## 결론

1. **SIMD는 이 루프에 도움이 안 된다.** `wasm-simd`가 `wasm-scalar`보다 오히려
   느리다. 루프가 이웃 점 gather + 인터리브 출력 scatter로 **메모리 바운드**라,
   `f32x4` 산술 처리량이 병목을 못 건드리고 shuffle 오버헤드만 더해진다.
2. **진짜 병목은 `Math.hypot`이었다.** 인라인 `sqrt`로 바꾸면 순수 JS에서
   2.79×, 출력은 비트 동일(maxdiff 0). WASM 툴체인·빌드스텝 없이 얻는 승리.
3. WASM scalar가 6.4×까지 가지만, 이 루프는 프레임마다가 아니라 레이아웃/뷰포트
   전환 시 1회 실행이고 `js-tight`(4.5ms)만으로도 프레임 예산 안에 들어온다.
   WASM 빌드 파이프라인을 추가할 ROI가 없다.

→ 실제 적용: `SchemaCanvas.tsx`의 해당 루프에서 `Math.hypot` → 인라인 `sqrt`
만 반영했다(검증된 무위험 2.8× 승리). SIMD/WASM은 도입하지 않았다.

## 재현

```sh
# wasm 커널 (선택 — .wasm은 커밋돼 있어 없어도 JS 변형은 돈다)
brew install zig
zig cc --target=wasm32-freestanding -O3 -ffast-math -nostdlib \
  -Wl,--no-entry -Wl,--export-dynamic -Wl,--export-memory \
  -Wl,--initial-memory=134217728 -o kernel_scalar.wasm kernel.c
zig cc --target=wasm32-freestanding -O3 -ffast-math -msimd128 -nostdlib \
  -Wl,--no-entry -Wl,--export-dynamic -Wl,--export-memory \
  -Wl,--initial-memory=134217728 -o kernel_simd.wasm kernel.c

# 벤치 (EDGES는 wasm 정적 버퍼상 ~40k가 상한; 그 이상은 wasm 자동 skip)
EDGES=40000 ITERS=50 bun run bench.ts
```
