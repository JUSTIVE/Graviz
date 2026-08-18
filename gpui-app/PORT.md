# GompassQL → GPUI 포팅 현황

브랜치: `gpui-port`. 웹앱(React + PixiJS/WebGL + GraphViz-WASM)을 Rust + GPUI로 재작성.
방침: **픽셀 동일 UI가 아니라 기능 동등성** (사용자 확인됨). 성능 기준: `schema.docs.graphql`(72k 라인, GitHub 스키마).

## 구조

```
gpui-app/
  core/            # gompass-core — GPUI 무관 순수 로직 (cargo test -p gompass-core)
    src/graph.rs   # sdl-to-graph.ts 포팅: SDL → ParsedGraph (+ until/reachable/components)
    src/layout.rs  # 계층형(Sugiyama) 레이아웃 — GraphViz WASM + 청킹 오케스트레이터 대체
  src/
    model.rs       # 뷰모델: 카드 지오메트리 단일 소스 (페인트/히트테스트 공유)
    canvas.rs      # GPUI 캔버스: 팬/줌/호버/클릭 + 페인팅 (텍스트 LOD, 컬링)
    main.rs        # 엔트리: SDL 로드 → 파싱 → 레이아웃 → 윈도우
```

## 웹앱 대비 의도적으로 버린 것 (WebGL 브라우저 한계 우회 코드)

- 텍스처 캐시/DPR 버킷/모션 게이트/스프라이트 LOD 플레이스홀더 — GPUI는 즉시 페인팅이라 불필요
- SDF 글리프 아틀라스 — GPUI 텍스트 시스템이 대체
- 레이아웃 워커 풀 + 컴포넌트 청킹(500노드 단위 이등분) — 네이티브 레이아웃은 한 방에 처리
- IndexedDB 레이아웃 캐시 — 네이티브 속도면 필요성 재평가 (필요시 파일 캐시)

## 개선(최적화) 사항

- 엣지가 dot 스플라인 대신 **필드 행 포트에 앵커된** 베지어 (소스 필드 행 중심에서 출발)
- 카드 행 지오메트리 수식이 한 곳(model.rs `Card::row_y`/`row_at`) — 웹앱은 같은 수식이 3벌
- 레이아웃/파싱 모두 네이티브 — WASM OOM, 워커 직렬화 오버헤드 소멸

## 상태

- [x] GPUI 빌드 환경 (Rust stable 업데이트, Metal Toolchain 설치)
- [x] core/layout.rs — 계층형 레이아웃 + 과높이 랭크 컬럼 분할 + 셸프 패킹 (테스트 포함)
- [x] core/graph/ — sdl-to-graph 충실 포팅 (68 테스트, GitHub 스키마 파싱 ~80ms)
- [x] core/search.rs — fuzzyScore/proseMatch/snippet/dotted-query (6 테스트)
- [x] canvas/model — 1차 렌더링 (카드, 엣지, 팬/줌/호버/클릭 내비게이션, LOD, 컬링, 상태바)
- [x] 트리 패널 — uniform_list + 키 캡처 검색, ↑/↓/Enter, 클릭 → 캔버스 포커스
- [x] Cmd+K 검색 포커스, Cmd+B 사이드바 토글
- [x] 모드 탭 (Reachable / Orphaned / Deprecated) — 모드 전환 = 슬라이스 후 전체 재레이아웃(~10ms)
- [ ] kind 필터 칩, referencedBy 패널, 포커스 스택/브레드크럼
- [ ] SDL 오버레이 (overlay.ts 포팅 + 하단 도크 + diff 하이라이트)
- [ ] 랜딩(파일 열기/드롭/히스토리), 테마, 설정 영속화
- [ ] 필드 툴팁/설명 표시, Investigate 모드, 엣지 번들 토글
