# HANDOVER — sdocx2pdf 정렬/텍스트 레이아웃 작업

> 작성일: 2026-10-19
> 작업 브랜치: `feature/png-support`
> 테스트 파일: `/Users/kyungmolee01/Downloads/모든 테스트.sdocx` (정답: `모든 테스트.pdf`)

---

## 1. 오늘 완료된 변경사항

### 구현/수정 완료
| 항목 | 파일 | 내용 |
|---|---|---|
| 텍스트 베이스라인 위치 | `sdocx2pdf/src/page.rs` | `font_size * 0.8` → `font_size * BASELINE_EM(1.0)` 로 변경. 정답 PDF와 y 위치 1pt 이내 일치 |
| 이미지-텍스트 간격 | `sdocx2pdf/src/page.rs` | `FLOATING_IMAGE_CLEARANCE` 35.0 → 6.7 (문서 유닛, ≈2.3pt). `position_below_image()` 헬퍼 추가로 glyph top이 이미지 하단+CLEARANCE에 정확히 위치 |
| 텍스트 상자 회전 | `sdocx2pdf/src/page.rs` | `angle_of()`/`rotation_about_centre()` 추가. 텍스트 상자 회전(-27°, 270° 등) 반영 |
| 밑줄/취소선 위치 | `sdocx2pdf/src/page.rs` | `UNDERLINE_BELOW_BASELINE_EM(0.19)`, `STRIKETHROUGH_ABOVE_BASELINE_EM(0.21)` 상수 추가. 정답과 위치 일치 |
| 문단 정렬 로직 | `sdocx2pdf/src/page.rs` + `sdocx/src/page/object/text{,_core}.rs` | `paragraph_alignment()` 함수 추가. `Text::paragraph_start_at()` 메서드 추가. left(0)/center(1)/right(2) 판별 로직 구현 |

### 테스트
- `cargo test` 전체 통과 (10 passed, 기존 8 + 신규 0)
- 이미지 충돌 2개 테스트는 `position_below_image` 새 수식에 맞게 기대값 수정

---

## 2. 다음 작업자에게 넘기는 미완료 항목

### 🔴 [최우선] 페이지 7 문단 정렬이 실제로 적용되지 않음
- **증상**: 정답 PDF 페이지 7의 본문 텍스트는 중앙 정렬/우측 정렬이 명확히 보이나, 우리 출력은 전부 좌측 정렬.
- **원인 분석**: sdocx 파일의 `body_text.text_common().paragraphs()` 에서 `Alignment` 타입 paragraph가 **위치 187까지(문서 앞부분)에만 존재**하고, 페이지 7의 본문 텍스트는 위치 539~752에 위치해 정렬 데이터가 아예 없음.
  - 텍스트 상자(text box)의 Alignment는 정상적으로 파싱됨 (`val=1`→right, `val=2`→center 확인).
  - 본문(body text)의 경우 paged 문서 후반부 정렬이 다른 방식으로 저장되거나, `sdocx` 라이브러리 파서가 누락하고 있을 가능성.
- **확인 필요**:
  1. `sdocx` 크레이트의 `text_common` 파싱이 모든 섹션의 paragraph를 읽는지 확인
  2. 혹은 Samsung Notes가 paged 문서 본문 정렬을 text object 외 다른 위치(예: page metadata)에 저장하는지 역공학 필요
  3. 임시 우회책: 텍스트 상자 방식으로 본문도 처리하도록 구조 변경 고려

### 🟡 페이지 8 — 이미지 회전 미적용
- sdocx 객체에 `angle` (회전 각도)이 있으나 이미지 렌더링 시 미적용. `draw_image` 경로에 `push_transform(rotation_about_centre(...))` 추가 필요.
- 텍스트 상자 회전은 이미 구현되어 있으니 동일 패턴 차용.

### 🟡 페이지 10 — 이미지 크롭(Clipping)
- 이미지가 용지 경계를 넘어갈 때 삼성노트는 용지 내 범위만 보임. 현재는 전체 이미지가 찌그러지거나 넘침.
- `page.rect` 기준 clip 적용 필요.

### 🟡 점선/파선 (페이지 4)
- 구현은 되어 있으나 정답과 간격/길이가 미세하게 다름. 정답 PDF 픽셀 단위 비교 후 보정 필요.

### 🟠 스티커 (SPI 파일)
- 7페이지 등에 스티커가 있으나 `*.spi`는 독점 포맷이라 미지원. `ERROR ... unsupported format` 출력됨.
- 사용자에게 sdocx에서 스티커가 보이는지 확인 후, 안 보이면 제거하겠다고 함. 추적 필요.

---

## 3. 작업 팁 / 알아둘 것

1. **정답 비교 방법**:
   ```bash
   ./target/release/sdocx2pdf "/Users/kyungmolee01/Downloads/모든 테스트.sdocx" /tmp/out.pdf
   # pymupdf로 페이지별 그레이스케일 diff 퍼센트 측정 (기존 /tmp/cmp.py 참조)
   ```

2. **정렬 값 매핑** (확인됨): `0=left`, `1=right`, `2=center`
   - 주의: 텍스트 상자와 본문 모두 동일 매핑이나 본문은 데이터 누락 문제 있음.

3. **디버그 빌드 바이너리**: `sdocx2pdf/src/bin/` 에 `inspect2`, `dump_align`, `dump_body`, `dump_sections2`, `find_text`, `dump_p7_para` 등 임시 인스펙터 있음. 커밋 전 정리 권장.

4. **테스트 파일 위치**: `/Users/kyungmolee01/Downloads/` (이전 `텍스트 test file_261019_053515.sdocx` 는 더 이상 사용 안 함).

---

## 4. 커밋 체크리스트
- [x] 디버그 `eprintln!` 제거 완료
- [x] `cargo test` 통과
- [ ] `sdocx2pdf/src/bin/` 임시 인스펙터 정리 (옵션)
- [ ] 정렬 데이터 누락 원인 분석 (미완료)
