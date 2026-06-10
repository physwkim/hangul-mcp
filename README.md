# hangul-mcp

[rhwp](../rhwp) 기반 HWPX 문서 편집 MCP(Model Context Protocol) 서버.
.hwpx(또는 .hwp) 파일을 열어 메모리에서 편집하고 HWPX로 저장한다.

## 빌드 / 등록

`../rhwp` 체크아웃이 필요하다 (path 의존성).

```bash
cargo build --release

# Claude Code에 등록
claude mcp add hangul-mcp -- /Users/stevek/codes/hangul-mcp/target/release/hangul-mcp
```

## 워크플로

1. `open_document(path)` → `doc_id` 발급
2. `get_structure(doc_id)` → 편집 좌표(section/para/control) 파악
3. 편집 도구 호출 (메모리에서만 변경)
4. `save_document(doc_id, output_path?)` → HWPX 저장

좌표는 모두 0-기반이고 문자 위치는 char 단위다. rhwp `DocumentCore`와 동일한 좌표계를 쓴다.

## 도구 목록 (20개)

**세션**

| 도구 | 설명 |
|------|------|
| `open_document(path)` | 문서 열기 → `{doc_id, sections, paragraphs_per_section, page_count}` |
| `save_document(doc_id, output_path?)` | HWPX 저장. `output_path` 생략 시 원본 덮어쓰기 |
| `close_document(doc_id)` | 세션 닫기. 미저장 변경은 버려지며 결과에 표시 |
| `list_documents()` | 열린 문서와 dirty 상태 |

**읽기**

| 도구 | 설명 |
|------|------|
| `get_structure(doc_id)` | 구역→문단 인덱스, 미리보기, 표 컨트롤 위치 |
| `get_text(doc_id, section, para_start?, para_end?)` | 문단 텍스트 전문 |
| `get_table(doc_id, section, para, control)` | 표 크기 + 셀별 `cell_idx`/`row`/`col`/텍스트 |

**검색/치환**

| 도구 | 설명 |
|------|------|
| `search_text(doc_id, query, case_sensitive?, include_cells?)` | 전체 매치 위치 |
| `replace_text(doc_id, query, replacement, all?, case_sensitive?)` | 본문 치환 (전체 또는 첫 매치) |

**텍스트/문단 편집**

| 도구 | 설명 |
|------|------|
| `insert_text(doc_id, section, para, char_offset, text)` | 텍스트 삽입 |
| `delete_text(doc_id, section, para, char_offset, count)` | 텍스트 삭제 |
| `insert_paragraph(doc_id, section, para)` | 빈 문단 삽입 |
| `delete_paragraph(doc_id, section, para)` | 문단 삭제 |
| `split_paragraph(doc_id, section, para, char_offset)` | 문단 분할 |
| `merge_paragraph(doc_id, section, para)` | 이전 문단과 병합 |

**표 편집**

| 도구 | 설명 |
|------|------|
| `set_cell_text(doc_id, section, para, control, cell, cell_para?, text)` | 셀 문단 텍스트 교체 |
| `insert_table_row(doc_id, section, para, control, row, below?)` | 행 삽입 |
| `insert_table_column(doc_id, section, para, control, col, right?)` | 열 삽입 |
| `delete_table_row(doc_id, section, para, control, row)` | 행 삭제 |
| `delete_table_column(doc_id, section, para, control, col)` | 열 삭제 |

## 테스트

```bash
cargo nextest run
```

`tests/roundtrip.rs`가 픽스처(`tests/fixtures/*.hwpx`, rhwp samples에서 복사)로
open → 편집 → save → `parse_hwpx` 재파싱 라운드트립을 단언한다.

## 비범위 (후속 후보)

- 서식(char/para format, style), 머리말/꼬리말, 각주, HTML import — rhwp `DocumentCore`에 API가 이미 있어 도구만 추가하면 됨
- HWP 5.0 저장(`export_hwp_with_adapter`), PDF 내보내기
- 편집 배치 최적화(`begin_batch_native`/`end_batch_native`)
