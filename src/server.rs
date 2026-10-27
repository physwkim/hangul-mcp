//! MCP 서버 본체 — 도구 정의.
//!
//! 좌표계는 rhwp `DocumentCore`와 동일하다:
//! - 본문: (section, para, char_offset) — 모두 0-기반, char_offset은 char 단위
//! - 표: (section, para, control)로 표 컨트롤을 지정하고, 셀은 cell_idx(평면 인덱스)
//!   로 지정한다. cell_idx와 (row, col) 매핑은 get_table 결과에 포함된다.

use std::path::PathBuf;
use std::sync::Arc;

use rhwp::model::control::Control;
use rhwp::DocumentCore;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde::Deserialize;
use serde_json::json;

use crate::format::{paragraph_at, CellTarget, CharFormat, ParaFormat};
use crate::store::DocumentStore;

/// 문단 미리보기 길이 (chars).
const PREVIEW_CHARS: usize = 80;

#[derive(Clone)]
pub struct HangulMcp {
    store: Arc<DocumentStore>,
    tool_router: ToolRouter<Self>,
}

impl HangulMcp {
    pub fn new() -> Self {
        Self {
            store: Arc::new(DocumentStore::new()),
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for HangulMcp {
    fn default() -> Self {
        Self::new()
    }
}

fn hwp_err(e: rhwp::HwpError) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

fn io_err(context: &str, e: std::io::Error) -> ErrorData {
    ErrorData::internal_error(format!("{context}: {e}"), None)
}

/// 양식 개체 타입 → 표시 문자열 (rhwp `form_type_to_str`와 동일 표기).
fn form_type_str(ft: rhwp::model::control::FormType) -> &'static str {
    use rhwp::model::control::FormType;
    match ft {
        FormType::PushButton => "PushButton",
        FormType::CheckBox => "CheckBox",
        FormType::RadioButton => "RadioButton",
        FormType::ComboBox => "ComboBox",
        FormType::Edit => "Edit",
    }
}

// ─── 파라미터 구조체 ──────────────────────────────────────────

#[derive(Deserialize, schemars::JsonSchema)]
pub struct OpenDocumentParams {
    /// 열 .hwpx (또는 .hwp) 파일의 절대 경로
    pub path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SaveDocumentParams {
    /// open_document가 반환한 문서 핸들
    pub doc_id: String,
    /// 저장 경로. 생략하면 원본 경로를 덮어쓴다. 확장자(.hwpx / .hwp)로 저장 형식을 고른다.
    pub output_path: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DocIdParams {
    /// open_document가 반환한 문서 핸들
    pub doc_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GetTextParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스 (0-기반)
    pub section: usize,
    /// 시작 문단 인덱스 (생략 시 0)
    pub para_start: Option<usize>,
    /// 끝 문단 인덱스(포함). 생략 시 구역 끝까지
    pub para_end: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GetTableParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스 (get_structure의 table control 값)
    pub control: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SearchTextParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 검색어
    pub query: String,
    /// 대소문자 구분 (기본 false)
    pub case_sensitive: Option<bool>,
    /// 표 셀 내부 포함 여부 (기본 true)
    pub include_cells: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ReplaceTextParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 찾을 텍스트
    pub query: String,
    /// 바꿀 텍스트
    pub replacement: String,
    /// true면 전체 치환, false면 첫 매치만 (기본 true)
    pub all: Option<bool>,
    /// 대소문자 구분 (기본 false)
    pub case_sensitive: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertTextParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스
    pub para: usize,
    /// 문단 내 삽입 위치 (char 단위, 0-기반)
    pub char_offset: usize,
    /// 삽입할 텍스트
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DeleteTextParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스
    pub para: usize,
    /// 삭제 시작 위치 (char 단위)
    pub char_offset: usize,
    /// 삭제할 문자 수
    pub count: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ParaParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스
    pub para: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SplitParagraphParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 분할할 문단 인덱스
    pub para: usize,
    /// 분할 지점 (char 단위)
    pub char_offset: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetCellTextParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 셀 평면 인덱스 (get_table의 cell_idx)
    pub cell: usize,
    /// 셀 내 문단 인덱스 (생략 시 0)
    pub cell_para: Option<usize>,
    /// 셀 문단에 설정할 텍스트 (기존 텍스트는 삭제됨)
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertTableRowParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 기준 행 인덱스 (0-기반)
    pub row: u16,
    /// true면 기준 행 아래에 삽입 (기본 true)
    pub below: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertTableColumnParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 기준 열 인덱스 (0-기반)
    pub col: u16,
    /// true면 기준 열 오른쪽에 삽입 (기본 true)
    pub right: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DeleteTableRowParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 삭제할 행 인덱스 (0-기반)
    pub row: u16,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DeleteTableColumnParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 삭제할 열 인덱스 (0-기반)
    pub col: u16,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct FitTableToPageParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetTableColumnWidthsParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 열별 폭 목록 (HWPUNIT, 길이는 표의 열 수와 같아야 함)
    pub widths: Vec<u32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct FieldByNameParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 필드(누름틀) 이름
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetFieldValueParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 필드(누름틀) 이름
    pub name: String,
    /// 설정할 값 (필드 범위의 기존 텍스트를 대체)
    pub value: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct FormAtParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스 (0-기반)
    pub section: usize,
    /// 문단 인덱스 (0-기반)
    pub para: usize,
    /// 문단 내 컨트롤 인덱스 (list_forms의 control 값)
    pub control: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetFormValueParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스 (0-기반)
    pub section: usize,
    /// 문단 인덱스 (0-기반)
    pub para: usize,
    /// 문단 내 컨트롤 인덱스 (list_forms의 control 값)
    pub control: usize,
    /// 체크 상태 (CheckBox/RadioButton: 0=해제, 1=선택)
    pub value: Option<i32>,
    /// 텍스트 내용 (ComboBox 선택값 / Edit 입력값)
    pub text: Option<String>,
    /// 캡션 (PushButton/CheckBox/RadioButton 라벨)
    pub caption: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ApplyCharFormatParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스 (cell을 지정하면 표가 속한 문단)
    pub para: usize,
    /// 서식 적용 시작 위치 (char 단위, 포함)
    pub start: usize,
    /// 서식 적용 끝 위치 (char 단위, 제외)
    pub end: usize,
    /// 지정하면 본문 대신 표 셀 문단에 적용한다
    pub cell: Option<CellTarget>,
    /// 적용할 서식
    pub format: CharFormat,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GetCharFormatParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스 (cell을 지정하면 표가 속한 문단)
    pub para: usize,
    /// 조회 시작 위치 (char 단위, 포함, 생략 시 0)
    pub start: Option<usize>,
    /// 조회 끝 위치 (char 단위, 제외, 생략 시 문단 끝)
    pub end: Option<usize>,
    /// 지정하면 본문 대신 표 셀 문단을 조회한다
    pub cell: Option<CellTarget>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ParaTargetParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스 (cell을 지정하면 표가 속한 문단)
    pub para: usize,
    /// 지정하면 본문 대신 표 셀 문단을 대상으로 한다
    pub cell: Option<CellTarget>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ApplyParaFormatParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스 (cell을 지정하면 표가 속한 문단)
    pub para: usize,
    /// 지정하면 본문 대신 표 셀 문단에 적용한다
    pub cell: Option<CellTarget>,
    /// 적용할 문단 서식
    pub format: ParaFormat,
}

/// 스타일 지정: list_styles의 id(숫자) 또는 이름(한글/영문, 문자열).
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum StyleRef {
    Id(usize),
    Name(String),
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ApplyStyleParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스 (cell을 지정하면 표가 속한 문단)
    pub para: usize,
    /// 지정하면 본문 대신 표 셀 문단에 적용한다
    pub cell: Option<CellTarget>,
    /// 적용할 스타일 (id 숫자 또는 이름 문자열, 예: 3 또는 "개요 1")
    pub style: StyleRef,
}

// ─── 도구 구현 ────────────────────────────────────────────────

#[tool_router]
impl HangulMcp {
    // ── 세션 ──

    #[tool(
        description = "HWPX/HWP 문서를 열어 doc_id 핸들을 발급한다. 이후 모든 도구는 이 doc_id로 문서를 지정한다."
    )]
    pub fn open_document(
        &self,
        Parameters(p): Parameters<OpenDocumentParams>,
    ) -> Result<String, ErrorData> {
        let path = PathBuf::from(&p.path);
        let bytes =
            std::fs::read(&path).map_err(|e| io_err(&format!("파일 읽기 실패 {}", p.path), e))?;
        let core = DocumentCore::from_bytes(&bytes).map_err(hwp_err)?;
        let paragraphs_per_section: Vec<usize> = core
            .document()
            .sections
            .iter()
            .map(|s| s.paragraphs.len())
            .collect();
        let page_count = core.page_count();
        let doc_id = self.store.insert(core, path);
        Ok(json!({
            "doc_id": doc_id,
            "sections": paragraphs_per_section.len(),
            "paragraphs_per_section": paragraphs_per_section,
            "page_count": page_count,
        })
        .to_string())
    }

    #[tool(
        description = "문서를 저장한다. 경로 확장자가 .hwpx면 HWPX, .hwp면 HWP 5.0 바이너리로 저장한다. output_path 생략 시 원본 경로를 덮어쓴다."
    )]
    pub fn save_document(
        &self,
        Parameters(p): Parameters<SaveDocumentParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_mut(&p.doc_id, |session| {
            let target = match &p.output_path {
                Some(out) => PathBuf::from(out),
                None => session.path.clone(),
            };
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            let bytes = match ext.as_deref() {
                Some("hwpx") => session.core.export_hwpx_native(),
                // HWPX 출처 IR 은 HWP 호환 형태로 바꿔야 직렬화가 맞는다 (rhwp #178).
                Some("hwp") => session.core.export_hwp_with_adapter(),
                _ => {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "저장 경로 확장자는 .hwpx 또는 .hwp여야 한다: {}",
                            target.display()
                        ),
                        None,
                    ))
                }
            }
            .map_err(hwp_err)?;
            std::fs::write(&target, &bytes)
                .map_err(|e| io_err(&format!("파일 쓰기 실패 {}", target.display()), e))?;
            session.dirty = false;
            Ok(json!({
                "saved_path": target.display().to_string(),
                "bytes": bytes.len(),
            })
            .to_string())
        })
    }

    #[tool(description = "문서 세션을 닫는다. 저장하지 않은 변경은 버려진다.")]
    pub fn close_document(
        &self,
        Parameters(p): Parameters<DocIdParams>,
    ) -> Result<String, ErrorData> {
        let (path, dirty) = self.store.remove(&p.doc_id)?;
        Ok(json!({
            "closed": p.doc_id,
            "path": path.display().to_string(),
            "discarded_unsaved_changes": dirty,
        })
        .to_string())
    }

    #[tool(description = "열려 있는 문서 핸들 목록과 미저장 변경 여부를 반환한다.")]
    pub fn list_documents(&self) -> Result<String, ErrorData> {
        let docs: Vec<_> = self
            .store
            .list()
            .into_iter()
            .map(|(id, path, dirty)| {
                json!({"doc_id": id, "path": path.display().to_string(), "dirty": dirty})
            })
            .collect();
        Ok(json!(docs).to_string())
    }

    #[tool(
        description = "서버 빌드 정보를 반환한다: hangul-mcp 버전과, 빌드에 쓰인 rhwp 의 버전·소스(git rev 또는 로컬 경로). 재빌드 후 서버가 실제로 갱신됐는지 확인할 때 쓴다."
    )]
    pub fn server_info(&self) -> Result<String, ErrorData> {
        Ok(json!({
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "rhwp_version": env!("RHWP_VERSION"),
            "rhwp_source": env!("RHWP_SOURCE"),
        })
        .to_string())
    }

    // ── 읽기 ──

    #[tool(
        description = "문서 구조를 반환한다: 구역별 문단 목록(인덱스, 텍스트 미리보기, 표 컨트롤). 편집 좌표(section/para/control)를 파악하는 시작점."
    )]
    pub fn get_structure(
        &self,
        Parameters(p): Parameters<DocIdParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let sections: Vec<_> = session
                .core
                .document()
                .sections
                .iter()
                .enumerate()
                .map(|(sec_idx, sec)| {
                    let paragraphs: Vec<_> = sec
                        .paragraphs
                        .iter()
                        .enumerate()
                        .map(|(para_idx, para)| {
                            let preview: String = para.text.chars().take(PREVIEW_CHARS).collect();
                            let truncated = para.text.chars().count() > PREVIEW_CHARS;
                            let tables: Vec<_> = para
                                .controls
                                .iter()
                                .enumerate()
                                .filter_map(|(ctrl_idx, ctrl)| match ctrl {
                                    Control::Table(t) => Some(json!({
                                        "control": ctrl_idx,
                                        "rows": t.row_count,
                                        "cols": t.col_count,
                                    })),
                                    _ => None,
                                })
                                .collect();
                            let mut entry = json!({
                                "para": para_idx,
                                "preview": preview,
                                "chars": para.text.chars().count(),
                            });
                            if truncated {
                                entry["preview_truncated"] = json!(true);
                            }
                            if !tables.is_empty() {
                                entry["tables"] = json!(tables);
                            }
                            if !para.controls.is_empty() {
                                entry["controls_total"] = json!(para.controls.len());
                            }
                            entry
                        })
                        .collect();
                    json!({"section": sec_idx, "paragraphs": paragraphs})
                })
                .collect();
            Ok(json!({"sections": sections}).to_string())
        })
    }

    #[tool(description = "구역의 문단 텍스트를 전문으로 반환한다 (para_start..=para_end 범위).")]
    pub fn get_text(&self, Parameters(p): Parameters<GetTextParams>) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let doc = session.core.document();
            let section = doc.sections.get(p.section).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("구역 {} 범위 초과 (총 {}개)", p.section, doc.sections.len()),
                    None,
                )
            })?;
            let start = p.para_start.unwrap_or(0);
            let end = p
                .para_end
                .unwrap_or(section.paragraphs.len().saturating_sub(1));
            if start >= section.paragraphs.len() || end < start {
                return Err(ErrorData::invalid_params(
                    format!(
                        "문단 범위 {start}..={end} 잘못됨 (총 {}개)",
                        section.paragraphs.len()
                    ),
                    None,
                ));
            }
            let end = end.min(section.paragraphs.len() - 1);
            let paragraphs: Vec<_> = section.paragraphs[start..=end]
                .iter()
                .enumerate()
                .map(|(i, para)| json!({"para": start + i, "text": para.text}))
                .collect();
            Ok(json!({"section": p.section, "paragraphs": paragraphs}).to_string())
        })
    }

    #[tool(
        description = "표의 크기와 모든 셀 내용을 반환한다. 각 셀의 cell_idx는 set_cell_text의 cell 인자로 쓴다."
    )]
    pub fn get_table(
        &self,
        Parameters(p): Parameters<GetTableParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let doc = session.core.document();
            let para = doc
                .sections
                .get(p.section)
                .and_then(|s| s.paragraphs.get(p.para))
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        format!("구역 {} 문단 {} 없음", p.section, p.para),
                        None,
                    )
                })?;
            let table = match para.controls.get(p.control) {
                Some(Control::Table(t)) => t,
                Some(_) => {
                    return Err(ErrorData::invalid_params(
                        format!("컨트롤 {}은(는) 표가 아님", p.control),
                        None,
                    ))
                }
                None => {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "컨트롤 인덱스 {} 범위 초과 (총 {}개)",
                            p.control,
                            para.controls.len()
                        ),
                        None,
                    ))
                }
            };
            let cells: Vec<_> = table
                .cells
                .iter()
                .enumerate()
                .map(|(cell_idx, cell)| {
                    let text: Vec<&str> =
                        cell.paragraphs.iter().map(|cp| cp.text.as_str()).collect();
                    let mut entry = json!({
                        "cell_idx": cell_idx,
                        "row": cell.row,
                        "col": cell.col,
                        "text": text.join("\n"),
                        "cell_paragraphs": cell.paragraphs.len(),
                    });
                    if cell.row_span > 1 || cell.col_span > 1 {
                        entry["row_span"] = json!(cell.row_span);
                        entry["col_span"] = json!(cell.col_span);
                    }
                    entry
                })
                .collect();
            Ok(json!({
                "rows": table.row_count,
                "cols": table.col_count,
                "cells": cells,
            })
            .to_string())
        })
    }

    // ── 검색/치환 ──

    #[tool(description = "문서 전체에서 텍스트를 검색해 모든 매치 위치를 반환한다.")]
    pub fn search_text(
        &self,
        Parameters(p): Parameters<SearchTextParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            session
                .core
                .search_all_text_native(
                    &p.query,
                    p.case_sensitive.unwrap_or(false),
                    p.include_cells.unwrap_or(true),
                )
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "텍스트를 치환한다. all=true(기본)면 본문 전체, false면 첫 매치만. 표 셀 내부는 본문 매치에 포함되지 않을 수 있으니 셀은 set_cell_text 사용."
    )]
    pub fn replace_text(
        &self,
        Parameters(p): Parameters<ReplaceTextParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let case_sensitive = p.case_sensitive.unwrap_or(false);
            if p.all.unwrap_or(true) {
                session
                    .core
                    .replace_all_native(&p.query, &p.replacement, case_sensitive)
                    .map_err(hwp_err)
            } else {
                session
                    .core
                    .replace_one_native(&p.query, &p.replacement, case_sensitive)
                    .map_err(hwp_err)
            }
        })
    }

    // ── 텍스트/문단 편집 ──

    #[tool(description = "문단의 지정 위치에 텍스트를 삽입한다.")]
    pub fn insert_text(
        &self,
        Parameters(p): Parameters<InsertTextParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .insert_text_native(p.section, p.para, p.char_offset, &p.text)
                .map_err(hwp_err)
        })
    }

    #[tool(description = "문단의 지정 위치부터 count자의 텍스트를 삭제한다.")]
    pub fn delete_text(
        &self,
        Parameters(p): Parameters<DeleteTextParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .delete_text_native(p.section, p.para, p.char_offset, p.count)
                .map_err(hwp_err)
        })
    }

    #[tool(description = "지정 인덱스 위치에 빈 문단을 삽입한다 (기존 문단은 뒤로 밀림).")]
    pub fn insert_paragraph(
        &self,
        Parameters(p): Parameters<ParaParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .insert_paragraph_native(p.section, p.para)
                .map_err(hwp_err)
        })
    }

    #[tool(description = "문단을 통째로 삭제한다.")]
    pub fn delete_paragraph(
        &self,
        Parameters(p): Parameters<ParaParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .delete_paragraph_native(p.section, p.para)
                .map_err(hwp_err)
        })
    }

    #[tool(description = "문단을 char_offset 지점에서 두 문단으로 분할한다.")]
    pub fn split_paragraph(
        &self,
        Parameters(p): Parameters<SplitParagraphParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .split_paragraph_native(p.section, p.para, p.char_offset, None)
                .map_err(hwp_err)
        })
    }

    #[tool(description = "문단을 이전 문단과 병합한다.")]
    pub fn merge_paragraph(
        &self,
        Parameters(p): Parameters<ParaParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .merge_paragraph_native(p.section, p.para)
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "문단(또는 표 셀 문단)의 [start, end) 범위에 글자 서식을 적용한다. 색, 진하게/기울임/밑줄/취소선, 크기, 글꼴, 음영, 그림자, 위/아래첨자, 장평/자간 등. format에 지정한 항목만 바뀐다."
    )]
    pub fn apply_char_format(
        &self,
        Parameters(p): Parameters<ApplyCharFormatParams>,
    ) -> Result<String, ErrorData> {
        p.format.validate()?;
        self.store.with_session_edit(&p.doc_id, |session| {
            let cell = p.cell.as_ref().map(|c| (c.control, c.cell, c.cell_para()));
            let len = match cell {
                Some((control, cell, cp)) => session
                    .core
                    .get_cell_paragraph_length_native(p.section, p.para, control, cell, cp),
                None => session.core.get_paragraph_length_native(p.section, p.para),
            }
            .map_err(hwp_err)?;
            // rhwp 는 범위 밖이나 빈 범위를 오류 없이 무시하므로 여기서 거른다.
            if p.start >= p.end || p.end > len {
                return Err(ErrorData::invalid_params(
                    format!(
                        "범위 [{}, {}) 가 잘못됐다 (문단 길이 {len}, start < end <= 길이여야 함)",
                        p.start, p.end
                    ),
                    None,
                ));
            }
            let font_ids = match p.format.font_name.as_deref() {
                Some(name) => Some(crate::format::font_ids_for(&mut session.core, name)?),
                None => None,
            };
            let props = p.format.to_rhwp_props(font_ids)?;
            match cell {
                Some((control, cell, cp)) => session.core.apply_char_format_in_cell_native(
                    p.section, p.para, control, cell, cp, p.start, p.end, &props,
                ),
                None => session
                    .core
                    .apply_char_format_native(p.section, p.para, p.start, p.end, &props),
            }
            .map_err(hwp_err)
        })
    }

    #[tool(
        description = "문단(또는 표 셀 문단)의 글자 서식을 조회한다. [start, end) 범위를 서식이 같은 구간(runs)으로 나눠 각 구간의 서식을 apply_char_format의 format과 같은 형태로 돌려준다."
    )]
    pub fn get_char_format(
        &self,
        Parameters(p): Parameters<GetCharFormatParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let doc = session.core.document();
            let para = paragraph_at(doc, p.section, p.para, p.cell.as_ref())?;
            let len = para.text.chars().count();
            let start = p.start.unwrap_or(0);
            let end = p.end.unwrap_or(len);
            if start > end || end > len {
                return Err(ErrorData::invalid_params(
                    format!("범위 [{start}, {end}) 가 잘못됐다 (문단 길이 {len})"),
                    None,
                ));
            }
            let mut runs = crate::format::char_shape_runs(para, start, end);
            if runs.is_empty() {
                // 빈 범위도 그 위치에 입력될 글자의 서식을 알려준다.
                runs.push((start, end, para.char_shape_id_at(start).unwrap_or(0)));
            }
            let runs: Vec<_> = runs
                .into_iter()
                .map(|(s, e, id)| {
                    let cs = doc.doc_info.char_shapes.get(id as usize).ok_or_else(|| {
                        ErrorData::internal_error(format!("글자 모양 {id} 이 문서에 없다"), None)
                    })?;
                    let (format, per_language) = CharFormat::from_shape(doc, cs);
                    Ok(json!({
                        "start": s,
                        "end": e,
                        "format": format,
                        "per_language": per_language,
                    }))
                })
                .collect::<Result<_, ErrorData>>()?;
            Ok(json!({ "length": len, "runs": runs }).to_string())
        })
    }

    #[tool(
        description = "문단(또는 표 셀 문단)의 문단 서식(정렬, 줄 간격, 여백, 들여쓰기, 문단 간격, 줄 나눔 등)을 apply_para_format의 format과 같은 형태로 조회한다. 문단에 적용된 스타일과 개요/번호 머리(heading: type None/Outline/Number/Bullet, level 1~7)도 함께 돌려준다."
    )]
    pub fn get_para_format(
        &self,
        Parameters(p): Parameters<ParaTargetParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let doc = session.core.document();
            let para = paragraph_at(doc, p.section, p.para, p.cell.as_ref())?;
            let id = para.para_shape_id;
            let ps = doc.doc_info.para_shapes.get(id as usize).ok_or_else(|| {
                ErrorData::internal_error(format!("문단 모양 {id} 이 문서에 없다"), None)
            })?;
            let style = doc.doc_info.styles.get(para.style_id as usize).map(|st| {
                json!({"id": para.style_id, "name": st.local_name, "english_name": st.english_name})
            });
            // 개요/번호/글머리표. 번호 정의가 문서마다 달라 적용 입력에는 두지 않고
            // 조회로만 알려준다 (개요 수준은 apply_style 로 바꾼다).
            let heading = json!({
                "type": format!("{:?}", ps.head_type),
                "level": ps.para_level + 1,
            });
            Ok(json!({
                "format": ParaFormat::from_shape(ps),
                "style": style,
                "heading": heading,
            })
            .to_string())
        })
    }

    #[tool(
        description = "문단(또는 표 셀 문단)에 문단 서식을 적용한다. 정렬, 줄 간격(%/pt), 왼쪽·오른쪽 여백, 첫 줄 들여쓰기/내어쓰기, 문단 위·아래 간격, 다음 문단과 함께, 문단 보호, 쪽 나눔, 줄 나눔 단위. format에 지정한 항목만 바뀐다."
    )]
    pub fn apply_para_format(
        &self,
        Parameters(p): Parameters<ApplyParaFormatParams>,
    ) -> Result<String, ErrorData> {
        p.format.validate()?;
        let props = p.format.to_rhwp_props()?;
        self.store.with_session_edit(&p.doc_id, |session| {
            match &p.cell {
                Some(c) => session.core.apply_para_format_in_cell_native(
                    p.section,
                    p.para,
                    c.control,
                    c.cell,
                    c.cell_para(),
                    &props,
                ),
                None => session
                    .core
                    .apply_para_format_native(p.section, p.para, &props),
            }
            .map_err(hwp_err)
        })
    }

    #[tool(
        description = "문서에 정의된 스타일 목록을 반환한다: id, 이름(한글/영문), 종류(para=문단 스타일, char=글자 스타일)."
    )]
    pub fn list_styles(&self, Parameters(p): Parameters<DocIdParams>) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let styles: Vec<_> = session
                .core
                .document()
                .doc_info
                .styles
                .iter()
                .enumerate()
                .map(|(id, st)| {
                    json!({
                        "id": id,
                        "name": st.local_name,
                        "english_name": st.english_name,
                        "kind": if st.style_type == 1 { "char" } else { "para" },
                    })
                })
                .collect();
            Ok(json!({ "styles": styles }).to_string())
        })
    }

    #[tool(
        description = "문단(또는 표 셀 문단)에 스타일을 적용한다. style은 list_styles의 id 또는 이름. 문단 스타일은 문단 모양과 글자 모양을, 글자 스타일은 문단 전체 글자 모양을 바꾼다. 문단에 직접 준 문단 서식(스타일과 다른 문단 모양)이 있으면 그 문단 모양은 유지되고, 직접 준 글자 서식도 유지된다."
    )]
    pub fn apply_style(
        &self,
        Parameters(p): Parameters<ApplyStyleParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let styles = &session.core.document().doc_info.styles;
            let style_id = match &p.style {
                StyleRef::Id(id) if *id < styles.len() => *id,
                StyleRef::Id(id) => {
                    return Err(ErrorData::invalid_params(
                        format!("스타일 id {id} 가 없다 (스타일 {}개)", styles.len()),
                        None,
                    ))
                }
                StyleRef::Name(name) => styles
                    .iter()
                    .position(|st| st.local_name == *name || st.english_name == *name)
                    .ok_or_else(|| {
                        ErrorData::invalid_params(
                            format!("스타일 {name:?} 이 없다 (list_styles 로 확인)"),
                            None,
                        )
                    })?,
            };
            match &p.cell {
                Some(c) => session.core.apply_cell_style_native(
                    p.section,
                    p.para,
                    c.control,
                    c.cell,
                    c.cell_para(),
                    style_id,
                ),
                None => session.core.apply_style_native(p.section, p.para, style_id),
            }
            .map_err(hwp_err)
        })
    }

    // ── 표 편집 ──

    #[tool(
        description = "표 셀 문단의 텍스트를 통째로 교체한다 (기존 텍스트 삭제 후 삽입). 셀에 문단이 여러 개면 cell_para로 지정."
    )]
    pub fn set_cell_text(
        &self,
        Parameters(p): Parameters<SetCellTextParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let cell_para = p.cell_para.unwrap_or(0);
            let old_len = session
                .core
                .get_cell_paragraph_length_native(p.section, p.para, p.control, p.cell, cell_para)
                .map_err(hwp_err)?;
            if old_len > 0 {
                session
                    .core
                    .delete_text_in_cell_native(
                        p.section, p.para, p.control, p.cell, cell_para, 0, old_len,
                    )
                    .map_err(hwp_err)?;
            }
            session
                .core
                .insert_text_in_cell_native(
                    p.section, p.para, p.control, p.cell, cell_para, 0, &p.text,
                )
                .map_err(hwp_err)
        })
    }

    #[tool(description = "표에 행을 삽입한다 (기준 행의 아래 또는 위).")]
    pub fn insert_table_row(
        &self,
        Parameters(p): Parameters<InsertTableRowParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .insert_table_row_native(
                    p.section,
                    p.para,
                    p.control,
                    p.row,
                    p.below.unwrap_or(true),
                )
                .map_err(hwp_err)
        })
    }

    #[tool(description = "표에 열을 삽입한다 (기준 열의 오른쪽 또는 왼쪽).")]
    pub fn insert_table_column(
        &self,
        Parameters(p): Parameters<InsertTableColumnParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .insert_table_column_native(
                    p.section,
                    p.para,
                    p.control,
                    p.col,
                    p.right.unwrap_or(true),
                )
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "표를 페이지 본문 폭에 맞춰 비례 축소한다. 열 폭 합이 본문 폭(페이지 폭 − 여백)을 넘을 때만 줄인다(축소 전용). insert_table_column으로 표가 페이지를 넘쳤을 때 호출한다."
    )]
    pub fn fit_table_to_page(
        &self,
        Parameters(p): Parameters<FitTableToPageParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .fit_table_to_page_native(p.section, p.para, p.control)
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "표의 열별 폭을 절대값(HWPUNIT)으로 설정한다. widths 길이는 표의 열 수와 같아야 하며, 표 전체 폭은 입력한 폭들의 합이 된다."
    )]
    pub fn set_table_column_widths(
        &self,
        Parameters(p): Parameters<SetTableColumnWidthsParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .set_table_column_widths_native(p.section, p.para, p.control, p.widths)
                .map_err(hwp_err)
        })
    }

    #[tool(description = "표에서 행을 삭제한다.")]
    pub fn delete_table_row(
        &self,
        Parameters(p): Parameters<DeleteTableRowParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .delete_table_row_native(p.section, p.para, p.control, p.row)
                .map_err(hwp_err)
        })
    }

    #[tool(description = "표에서 열을 삭제한다.")]
    pub fn delete_table_column(
        &self,
        Parameters(p): Parameters<DeleteTableColumnParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .delete_table_column_native(p.section, p.para, p.control, p.col)
                .map_err(hwp_err)
        })
    }

    // ── 누름틀(필드) ──

    #[tool(
        description = "문서의 모든 누름틀(필드)을 목록으로 반환한다: 이름, 종류, 안내문, 현재 값, 위치. 양식 문서를 채우기 전 필드 이름을 파악하는 시작점."
    )]
    pub fn list_fields(&self, Parameters(p): Parameters<DocIdParams>) -> Result<String, ErrorData> {
        self.store
            .with_session(&p.doc_id, |session| Ok(session.core.get_field_list_json()))
    }

    #[tool(description = "이름으로 누름틀(필드)의 현재 값을 조회한다.")]
    pub fn get_field_value(
        &self,
        Parameters(p): Parameters<FieldByNameParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            session
                .core
                .get_field_value_by_name(&p.name)
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "이름으로 누름틀(필드)의 값을 설정한다 (필드 범위의 기존 텍스트를 대체). 양식 문서 자동 채우기의 핵심 도구. 셀 안의 필드도 지원."
    )]
    pub fn set_field_value(
        &self,
        Parameters(p): Parameters<SetFieldValueParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .set_field_value_by_name(&p.name, &p.value)
                .map_err(hwp_err)
        })
    }

    // ── 양식 개체(폼) ──

    #[tool(
        description = "문서 본문의 모든 양식 개체(버튼/체크박스/라디오/콤보박스/에디트)를 목록으로 반환한다: 위치(section/para/control), 종류, 이름, 값, 캡션, 텍스트. 폼을 채우기 전 좌표를 파악하는 시작점. (표 셀 안의 폼은 미포함)"
    )]
    pub fn list_forms(&self, Parameters(p): Parameters<DocIdParams>) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let mut forms = Vec::new();
            for (sec_idx, sec) in session.core.document().sections.iter().enumerate() {
                for (para_idx, para) in sec.paragraphs.iter().enumerate() {
                    for (ci, ctrl) in para.controls.iter().enumerate() {
                        if let Control::Form(f) = ctrl {
                            forms.push(json!({
                                "section": sec_idx,
                                "para": para_idx,
                                "control": ci,
                                "formType": form_type_str(f.form_type),
                                "name": f.name.as_str(),
                                "value": f.value,
                                "caption": f.caption.as_str(),
                                "text": f.text.as_str(),
                            }));
                        }
                    }
                }
            }
            Ok(json!({ "forms": forms }).to_string())
        })
    }

    #[tool(
        description = "양식 개체의 현재 값을 조회한다 (종류/이름/값/텍스트/캡션/활성). 위치(section/para/control)는 list_forms로 파악한다."
    )]
    pub fn get_form_value(
        &self,
        Parameters(p): Parameters<FormAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            session
                .core
                .get_form_value_native(p.section, p.para, p.control)
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "양식 개체의 상세 정보를 반환한다: 값/텍스트/캡션 + 크기·색·속성(properties), 콤보박스는 항목 목록 포함."
    )]
    pub fn get_form_info(
        &self,
        Parameters(p): Parameters<FormAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            session
                .core
                .get_form_object_info_native(p.section, p.para, p.control)
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "양식 개체의 값을 설정한다. value(체크 상태 0/1), text(콤보 선택값·에디트 입력값), caption(버튼 라벨) 중 최소 하나를 지정한다. 본문 폼 대상. 위치는 list_forms로 파악한다."
    )]
    pub fn set_form_value(
        &self,
        Parameters(p): Parameters<SetFormValueParams>,
    ) -> Result<String, ErrorData> {
        if p.value.is_none() && p.text.is_none() && p.caption.is_none() {
            return Err(ErrorData::invalid_params(
                "value, text, caption 중 최소 하나를 지정해야 한다".to_string(),
                None,
            ));
        }
        let mut obj = serde_json::Map::new();
        if let Some(v) = p.value {
            obj.insert("value".to_string(), json!(v));
        }
        if let Some(t) = p.text.as_ref() {
            obj.insert("text".to_string(), json!(t));
        }
        if let Some(c) = p.caption.as_ref() {
            obj.insert("caption".to_string(), json!(c));
        }
        let value_json = serde_json::Value::Object(obj).to_string();
        self.store.with_session_edit(&p.doc_id, |session| {
            let result = session
                .core
                .set_form_value_native(p.section, p.para, p.control, &value_json)
                .map_err(hwp_err)?;
            // 네이티브는 폼이 아니어도 Ok({"ok":false})를 돌려준다 — 이때 Err로 바꿔
            // dirty가 잘못 서지 않게 한다 (with_session_edit는 Ok일 때만 dirty 설정).
            if result.contains(r#""ok":true"#) {
                Ok(result)
            } else {
                Err(ErrorData::invalid_params(
                    format!(
                        "({}, {}, {}) 위치에 양식 개체 없음",
                        p.section, p.para, p.control
                    ),
                    None,
                ))
            }
        })
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HangulMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "rhwp 기반 HWPX 문서 편집 서버. 워크플로: open_document로 doc_id를 얻고 → \
                 get_structure로 좌표(section/para/control)를 파악하고 → 편집 도구로 수정한 뒤 → \
                 save_document로 저장한다. 양식(누름틀) 문서는 list_fields로 필드 이름을 보고 \
                 set_field_value(name, value)로 채운다. 양식 개체(버튼/체크박스/콤보박스/에디트)는 \
                 list_forms로 좌표를 보고 set_form_value(section, para, control, ...)로 채운다. \
                 글자 서식(색/굵게/크기/글꼴 등)은 apply_char_format, 문단 서식(정렬/줄 간격/여백 등)은 \
                 apply_para_format으로 바꾸고, 현재 서식은 get_char_format/get_para_format으로 본다. \
                 스타일(바탕글/개요 1 등)은 list_styles로 보고 apply_style로 건다. \
                 모든 인덱스는 0-기반, 문자 위치는 char 단위다. \
                 편집은 메모리에서만 일어나며 save_document 전에는 파일이 바뀌지 않는다.",
            )
    }
}
