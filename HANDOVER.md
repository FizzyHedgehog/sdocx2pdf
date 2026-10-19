# HANDOVER — sdocx2pdf 정렬/텍스트 레이아웃 작업

> 작성일: 2026-10-19  
> 작업 브랜치: `feature/png-support`  
> 테스트 파일: `/Users/kyungmolee01/Downloads/모든 테스트.sdocx` (정답: `모든 테스트.pdf`)

---

## 1. 오늘 완료된 변경사항

### 구현/수정 완료

| 항목           | 파일                                                                | 내용                                                                                                                         |
| ------------ | ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 텍스트 베이스라인 위치 | `sdocx2pdf/src/page.rs`                                           | `font_size * 0.8` → `font_size * BASELINE_EM(1.0)` 로 변경. 정답 PDF와 y 위치 1pt 이내 일치                                            |
| 이미지-텍스트 간격   | `sdocx2pdf/src/page.rs`                                           | `FLOATING_IMAGE_CLEARANCE` 35.0 → 6.7 (문서 유닛, ≈2.3pt). `position_below_image()` 헬퍼 추가로 glyph top이 이미지 하단+CLEARANCE에 정확히 위치 |
| 텍스트 상자 회전    | `sdocx2pdf/src/page.rs`                                           | `angle_of()`/`rotation_about_centre()` 추가. 텍스트 상자 회전(-27°, 270° 등) 반영                                                      |
| 밑줄/취소선 위치    | `sdocx2pdf/src/page.rs`                                           | `UNDERLINE_BELOW_BASELINE_EM(0.19)`, `STRIKETHROUGH_ABOVE_BASELINE_EM(0.21)` 상수 추가. 정답과 위치 일치                              |
| 문단 정렬 로직     | `sdocx2pdf/src/page.rs` + `sdocx/src/page/object/text{,_core}.rs` | `ParagraphAlignment` enum + `Text::alignment_at_paragraph()` 추가. 문단 인덱스 기준으로 조회, left(0)/right(1)/center(2) 매핑, 줄바꿈된 모든 시각적 라인에 정렬 적용                |
| 줄바꿈 규칙        | `sdocx2pdf/src/page.rs`                                           | `wrap_text_ranges()` 재작성. 한글 **음절 단위** 개행 허용(`is_cjk`), 금칙 처리(`cannot_start_line`/`cannot_end_line`), 인위적 여유 폭(`+25`) 제거, 공백은 줄 끝 유지                 |

### 테스트

- `cargo test` 전체 통과 (24 passed: sdocx 9 + sdocx2pdf 15, 신규 9)
- 이미지 충돌 2개 테스트는 `position_below_image` 새 수식에 맞게 기대값 수정
- 줄바꿈 신규 테스트: 음절 단위 개행 / 가용 폭 채움 / 구두점 금칙 / 공백 줄 끝 유지

---

## 2. 다음 작업자에게 넘기는 미완료 항목

### ✅ [해결됨] 페이지 8(0-indexed 7) 본문 문단 정렬

- **원인**: `TYPE_ALIGN` paragraph의 `start_pos`/`end_pos`는 **문자 위치가 아니라 문단 인덱스**였다.
  기존 `paragraph_alignment()`는 `Text::paragraph_start_at()`으로 얻은 *문자 위치*(페이지 8이면 539~752)와
  paragraph의 `start_pos`(문단 인덱스, 최대 187)를 비교해서 항상 마지막 레코드(`val=0`)를 골라 좌측 정렬로 떨어졌다.
  - 이 문서는 총 219문단(개행 218개)이고 `Alignment` 레코드는 `170-176 (val=2)`, `176-183 (val=1)`, `187-188 (val=0)`.
    즉 문단 170~175가 중앙, 176~182가 우측 정렬이며 페이지 8 본문과 정확히 일치한다.
  - 텍스트 상자는 `0-1` 레코드 하나뿐이라 `paragraph_start == 0` 조건에서 우연히 맞아떨어져 정상 동작했다.
- **수정**:
  1. `sdocx`: `ParagraphAlignment` enum과 `Common::alignment_at_paragraph()` / `Text::alignment_at_paragraph()` 추가
     (기존 `paragraph_covers()` 재사용, 겹치는 레코드 중 `start_pos`가 가장 큰 것 채택).
  2. `sdocx2pdf`: `paragraph_alignment()`가 `paragraph_index_at(pos)`로 문단 인덱스를 구해 조회하도록 변경.
  3. 정렬 값 매핑 정정: **1=right, 2=center** (기존 코드는 1=center, 2=right로 반대로 구현되어 있었음).
  4. 줄바꿈된 **모든** 시각적 라인에 정렬 적용 (기존에는 `range_index == 0`만 적용).
     정답 PDF도 중앙 정렬 문단의 두 번째 줄("볼게. 이제 넘어왔네")이 중앙에 놓인다.
- **검증**: `/tmp/test_p8.pdf` 8페이지에서 중앙 정렬 4줄의 중심이 모두 x=300.0pt, 우측 정렬 4줄의 우단이 모두 x=573.33pt로 정답 PDF와 일치.

### ✅ [해결됨] 한글 줄바꿈이 정답과 다름

- **원인**: `wrap_text_ranges()`가 공백으로 끝나는 "단어" 단위로만 끊어서 한글 단어 내부에서는 개행이 불가능했고,
  게다가 `wrap_limit = max_width + 25.0` 이라는 근거 없는 여유 폭을 두고 있었다.
- **정답 PDF의 규칙** (8페이지 실측): 그리디 채우기 + **음절 단위 개행**, 여유 폭 없음.
  가용 폭 546.66pt에 대해 정답 라인 폭이 539.81 / 540.09 → 다음 음절(≈16.3pt)이 안 들어가서 끊긴다.
  `…넘어가게 해|볼게`, `…해볼게. 이|제` 처럼 단어 중간에서도 끊는다.
- **수정**: `can_break_before()`(CJK 사이·공백 뒤에서 개행 허용, 닫는 구두점 앞/여는 괄호 뒤 금지) +
  그리디 채우기 루프로 교체. 공백은 다음 줄로 넘기지 않고 줄 끝에 남긴다(그려질 때 trim).
- **검증**: 8페이지 13줄, 2페이지 7줄이 정답과 동일한 내용·위치로 끊긴다.
  (`/tmp/test_p8.pdf`, `pdfplumber` 비교)
- **한계**: 폰트가 달라(SamsungKorean+Roboto vs NanumBarunGothic) 한글 advance가 +0.22%, 라틴이 +2~3% 크다.
  경계선에 걸친 줄은 한 음절/한 단어 일찍 끊길 수 있다. 라틴을 Roboto로 바꾸면 대부분 해소된다(`/Library/Fonts/Roboto-Regular.ttf` 존재).

### 🟡 목록 마커(불릿/번호) 위치가 정답과 다름

- 정답 PDF는 마커를 본문 텍스트 레이어가 아닌 별도 위치에 그리고, 심볼 불릿(`TYPE_BULLET` 값 8)은 텍스트로 추출되지 않는다.
  (3페이지 `호홓`: 정답은 x=70.00에서 텍스트 시작, 우리는 x=26.67에서 `• 호홓`)
- 우리는 `• ` / `1. ` 를 텍스트로 그려서 마커 폭만큼만 들여쓴다 → 들여쓰기 폭과 마커 모양이 다르다.
- 개선하려면 마커를 텍스트가 아닌 별도 드로잉으로 분리하고, 들여쓰기를 `INDENT_STEP_EM` 기준으로 맞춰야 한다.

### ✅ 페이지 8 — 이미지 회전 (검증 결과: **이미 구현되어 정상 동작**)

- 2026-10-20 확인: 9페이지(0-indexed 8) 이미지는 `-33°`로 정상 회전 출력된다. 이 항목은 오기였음.
- 경로: `page.rs draw_single_object` → `shape.rs draw_shape` → `draw_image`의
  `Transform::from_rotate_at(template.rotation, ...)`. krilla는 **도(degree)** 단위를 받는다.
  `template.rotation`은 파싱 시 `original_angle`에서 채워진다(`sdocx/src/page/object/shape.rs`).
- 정량 검증(정답 PDF vs 출력, 이미지 CTM): `356.4356 231.4720 -148.1736 228.1673 202.1016 …`
  → 회전 33.000°, 스케일 425.0007 × 272.0583pt, 2.8376 px/pt (양축 균일). 사실상 동일.
- 확대는 별도 배율 필드가 아니라 객체 `rect` 자체에 반영되어 있다(`original_rect == rect`, `crop_rect = None`).
  우리는 `rect` 크기로 그리므로 **확대 + 회전이 함께** 재현된다.
- **남은 구멍(잠재적)**: `draw_single_object`가 Image/Shape에 대해 `object_base().angle()`을 읽지 않는다.
  - `template`이 없으면(`path_bin_size == 0`) `NothingDrawn` → **이미지가 아예 안 그려짐**.
  - `template.rotation == 0`인데 `angle`만 있으면 수평으로 그려짐.
  - 수정 시 `draw_image` 내부 회전과 **이중 적용**되지 않도록 `draw_image`에는 rotation 0을 넘겨야 한다.
  - 현재 테스트 파일의 이미지·도형 7개는 전부 `template`이 있으므로 실측 재현 사례는 없음.

### 🟡 페이지 10 — 이미지 크롭(Clipping)

- 이미지가 용지 경계를 넘어갈 때 삼성노트는 용지 내 범위만 보임. 현재는 전체 이미지가 찌그러지거나 넘침.
- `page.rect` 기준 clip 적용 필요.

### 🟡 페이지 높이 0.29pt 차이 (전 페이지 공통)

- 출력 페이지 높이 **848.28766pt** vs 정답 **848.0pt** → 모든 콘텐츠가 0.29pt(2배 렌더링 시 0.58px) 아래로 밀린다.
- 9페이지 이미지 픽셀 diff가 이미지 가장자리에만 얇게 나타나는 원인이 이것. 정답은 배경 Form을 y=-0.2877만큼 올려서 맞춘다.

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
   - `Alignment` paragraph의 `start_pos`/`end_pos`는 **문단 인덱스**다. 문자 위치와 혼동하면 후반부 페이지 정렬이 전부 어긋난다.
   - 조회는 `Text::alignment_at_paragraph(paragraph_index)` 사용 (직접 `paragraphs()`를 뒤지지 말 것).
3. **디버그 빌드 바이너리**: `sdocx2pdf/src/bin/` 에 `inspect2`, `dump_align`, `dump_body`, `dump_sections2`, `find_text`, `dump_p7_para` 등 임시 인스펙터 있음. 커밋 전 정리 권장.
4. **테스트 파일 위치**: `/Users/kyungmolee01/Downloads/` (이전 `텍스트 test file_261019_053515.sdocx` 는 더 이상 사용 안 함).

---

## 4. 커밋 체크리스트

- [x] 디버그 `eprintln!` 제거 완료
- [x] `cargo test` 통과
- [ ] `sdocx2pdf/src/bin/` 임시 인스펙터 정리 (옵션)
- [x] 정렬 데이터 누락 원인 분석 (문단 인덱스 vs 문자 위치 혼동 → 수정 완료)

---

## 5. `.spi` 포맷 리버싱 (진행 중)

11페이지(0-indexed 10)의 올가미 크롭 이미지 2개가 `.spi`라서 통째로 사라지는 문제.
**외곽선/clipping 문제가 아니라 포맷 미지원**이다. 상세 분석은 `docs/spi-format.md` 참고.

### 확정된 것

- 파일 헤더 28바이트: `0x00`=20, `0x04`=426, `0x08`=0x1400, `0x0c..0x17` 프로파일(A/B 두 종류), `0x18`=파일크기−28 (15개 전부 검증)
- 이후 **20바이트 블록 헤더 + 페이로드**가 연속: `AA 02 | 00 00 | u32 | u16 index | 43 02 E0 A0 2C 00 00 00 3F FF`
- 블록 수 1~4개, 페이로드 1133/1105는 평평한(단색) 플레인
- 페이로드는 **자체 비트패킹 코덱**. zlib/deflate/bz2/lzma/zstd/lz4/JPEG/PNG/WebP/JPEG2000 전부 아님
- 평평한 영역은 28비트 주기(1의 위치 0·7·21) 코드가 반복됨
- 정답 PDF는 디코딩된 RGB를 **crop_rect 크기 그대로**, SMask 없이 임베드 (올가미 알파가 흰 배경에 합성된 상태)

### 막힌 지점

엔트로피 코더의 규칙을 바이트 관찰만으로 특정하지 못함. 공개 스펙 없음.

### 다음 단계

통제된 샘플이 있으면 코더 프리미티브를 분리할 수 있다 (사용자 요청 필요):
단색 영역만 20×20 / 64×64 / 200×200으로 올가미 크롭한 노트 + 글자 경계를 포함한 크롭 1개.

### 도구

```bash
python3 tools/spi_probe.py <file.spi>           # 헤더 + 블록 목록
python3 tools/spi_probe.py <file.spi> --dump 0  # 블록 헥스덤프
```
