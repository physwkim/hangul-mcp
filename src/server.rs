//! MCP 서버 본체 — 도구 정의.
//!
//! 좌표계는 rhwp `DocumentCore`와 동일하다:
//! - 본문: (section, para, char_offset) — 모두 0-기반, char_offset은 char 단위
//! - 표: (section, para, control)로 표 컨트롤을 지정하고, 셀은 cell_idx(평면 인덱스)
//!   로 지정한다. cell_idx와 (row, col) 매핑은 get_table 결과에 포함된다.

use std::path::PathBuf;
use std::sync::Arc;

use rhwp::document_core::hyperlink::HyperlinkTarget;
use rhwp::model::control::{Control, FormObject, FormType};
use rhwp::model::document::Document;
use rhwp::DocumentCore;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cell_format::CellFormat;
use crate::format::{
    check_color, check_range, color_bgr, color_css, paragraph_at, CellTarget, CharFormat,
    ParaFormat,
};
use crate::page::{mm_to_hu, PageSetup, MAX_PAPER_MM};
use crate::placement::{PictureFormat, TableFormat};
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

/// 본문 문단, 또는 `cell` 이 있으면 그 표 셀 문단의 글자 수.
fn para_len(
    core: &DocumentCore,
    section: usize,
    para: usize,
    cell: Option<&CellTarget>,
) -> Result<usize, ErrorData> {
    Ok(paragraph_at(core.document(), section, para, cell)?
        .text
        .chars()
        .count())
}

/// rhwp 편집은 문단 길이를 넘는 char_offset 을 오류 없이 끝으로 당기거나 무시하므로,
/// 위치를 받는 도구는 모두 편집 전에 `end <= 문단 길이` 를 이 검사로 확인한다.
/// `cell` 이 있으면 그 표 셀 문단의 길이로 잰다.
fn check_offset(
    core: &DocumentCore,
    section: usize,
    para: usize,
    cell: Option<&CellTarget>,
    name: &str,
    end: usize,
) -> Result<(), ErrorData> {
    let len = para_len(core, section, para, cell)?;
    if end > len {
        return Err(ErrorData::invalid_params(
            format!("{name} {end} 가 문단 길이 {len} 를 넘는다"),
            None,
        ));
    }
    Ok(())
}

/// 글자 단위 위치를 rhwp 문단 분할 API(split/쪽·단 나누기/표 삽입)의 논리 위치로 바꾼다.
///
/// 서버 도구의 char_offset 은 문단 텍스트의 글자 위치다(get_text·insert_text 와 같은 자).
/// 반면 `Paragraph::split_at` 은 글자처럼 취급한 개체·각주 등을 한 칸씩 더 센 논리 위치를
/// 받으므로, 앞에 인라인 개체가 있으면 그대로 넘긴 위치가 개체 수만큼 앞당겨진다.
/// 같은 글자 위치에 놓인 개체는 분할 뒤쪽 문단으로 간다(insert_text 가 개체 앞에 넣는 것과 같다).
/// 셀 문단 분할(`split_paragraph_in_cell_native`)도 같은 `split_at` 을 쓰므로 `cell` 이면 그 문단으로 센다.
fn split_offset(
    core: &DocumentCore,
    section: usize,
    para: usize,
    cell: Option<&CellTarget>,
    char_offset: usize,
) -> Result<usize, ErrorData> {
    check_offset(core, section, para, cell, "char_offset", char_offset)?;
    let para = paragraph_at(core.document(), section, para, cell)?;
    // `split_at` 이 한 칸씩 세는 컨트롤(`Paragraph::occupies_split_slot`)과 같은 집합이다.
    // 칸 컨트롤은 텍스트 위치 순서이므로, 글자 위치 char_offset 앞의 칸 m 개만 더하면
    // split_at 의 `논리 위치 − 앞선 칸 수` 가 정확히 char_offset 으로 돌아온다.
    let slots_before = para
        .controls
        .iter()
        .zip(para.control_text_positions())
        .filter(|(c, pos)| {
            (c.is_logical_inline() || matches!(c, Control::CharOverlap(_))) && *pos < char_offset
        })
        .count();
    Ok(char_offset + slots_before)
}

/// get_structure 가 `objects` 로 알리고 delete_object 가 지우는 개체 갈래 — 두 도구가 이
/// 한 집합을 쓴다. 갈래마다 rhwp 삭제 경로가 달라(각주 번호 재매김, 그림의 누름틀 범위 보정 등)
/// 범용 `delete_control_native` 대신 갈래별 native 로 보낸다.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ObjectKind {
    Table,
    Picture,
    Shape,
    Equation,
    Footnote,
    Endnote,
}

impl ObjectKind {
    fn of(control: &Control) -> Option<Self> {
        Some(match control {
            Control::Table(_) => Self::Table,
            Control::Picture(_) => Self::Picture,
            Control::Shape(_) => Self::Shape,
            Control::Equation(_) => Self::Equation,
            Control::Footnote(_) => Self::Footnote,
            Control::Endnote(_) => Self::Endnote,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Picture => "picture",
            Self::Shape => "shape",
            Self::Equation => "equation",
            Self::Footnote => "footnote",
            Self::Endnote => "endnote",
        }
    }
}

/// 머리말/꼬리말 text 의 자리표시자와 rhwp 필드 종류(insert_field_in_hf_native) / 저장 문자.
const HF_FIELDS: [(&str, u8, char); 3] = [
    ("{page}", 1, '\u{0015}'),
    ("{total_pages}", 2, '\u{0016}'),
    ("{file_name}", 3, '\u{0017}'),
];

/// 한 줄 텍스트만 받는다. rhwp 머리말/꼬리말·주석 텍스트 삽입은 줄바꿈을 문단 나눔으로
/// 바꾸지 않고 문단 안 글자로 넣는다.
fn check_single_line(name: &str, text: &str) -> Result<(), ErrorData> {
    if text.contains(['\n', '\r']) {
        return Err(ErrorData::invalid_params(
            format!("{name} 은(는) 한 줄이어야 한다 (줄바꿈 불가)"),
            None,
        ));
    }
    Ok(())
}

/// "쪽 {page}/{total_pages}" 를 [글자, 필드, 글자, 필드] 조각으로 나눈다.
enum HfPiece<'a> {
    Text(&'a str),
    Field(u8),
}

fn hf_pieces(text: &str) -> Vec<HfPiece<'_>> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next = HF_FIELDS
            .iter()
            .filter_map(|&(token, code, _)| rest.find(token).map(|at| (at, token, code)))
            .min_by_key(|&(at, _, _)| at);
        match next {
            Some((at, token, code)) => {
                if at > 0 {
                    pieces.push(HfPiece::Text(&rest[..at]));
                }
                pieces.push(HfPiece::Field(code));
                rest = &rest[at + token.len()..];
            }
            None => {
                pieces.push(HfPiece::Text(rest));
                rest = "";
            }
        }
    }
    pieces
}

/// 머리말/꼬리말 문단을 set_header_footer 의 자리표시자 표기로 되돌린다.
///
/// 필드는 두 꼴로 존재한다. 편집 중에는 마커 한 글자(U+0015~U+0017)이고, 저장본을 다시
/// 열면 rhwp 저장기가 바꾼 컨트롤이다 — 쪽 번호·전체 쪽수는 자리표 공백 한 글자에 놓인
/// 자동 번호, 파일 이름은 그 범위 글자를 덮는 파일 경로 필드.
fn hf_display_text(para: &rhwp::model::paragraph::Paragraph) -> String {
    use rhwp::model::control::{AutoNumberType, FieldType};
    let token_at = |idx: usize| -> Option<(&'static str, usize)> {
        let positions = para.control_text_positions();
        for (ci, ctrl) in para.controls.iter().enumerate() {
            if positions.get(ci) != Some(&idx) {
                continue;
            }
            match ctrl {
                Control::AutoNumber(a) if a.number_type == AutoNumberType::Page => {
                    return Some(("{page}", idx + 1))
                }
                Control::AutoNumber(a) if a.number_type == AutoNumberType::TotalPage => {
                    return Some(("{total_pages}", idx + 1))
                }
                Control::Field(f) if f.field_type == FieldType::Path => {
                    let end = para
                        .field_ranges
                        .iter()
                        .find(|r| r.control_idx == ci)
                        .map_or(idx, |r| r.end_char_idx);
                    return Some(("{file_name}", end));
                }
                _ => {}
            }
        }
        None
    };
    let chars: Vec<char> = para.text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i <= chars.len() {
        if let Some((token, next)) = token_at(i) {
            out.push_str(token);
            if next > i {
                i = next;
                continue;
            }
        }
        let Some(&ch) = chars.get(i) else { break };
        match HF_FIELDS.iter().find(|&&(_, _, marker)| marker == ch) {
            Some(&(token, _, _)) => out.push_str(token),
            None => out.push(ch),
        }
        i += 1;
    }
    out
}

fn history_json(h: crate::store::HistoryStatus) -> String {
    json!({
        "undo_available": h.undo_available,
        "redo_available": h.redo_available,
        "dirty": h.dirty,
    })
    .to_string()
}

fn io_err(context: &str, e: std::io::Error) -> ErrorData {
    ErrorData::internal_error(format!("{context}: {e}"), None)
}

/// 한 쪽 Markdown 의 그림 토큰(`[[RHWP_IMAGE:n]]`, 쪽마다 1부터)을 `<md 파일이름>_assets/`
/// 에 쓴 그림 파일 링크로 바꾼다. 쓴 파일 경로는 `written` 에 문서 순서대로 쌓이고, 파일
/// 이름 번호도 문서 전체에서 이어진다. 그림 바이트를 찾지 못하면 토큰을 지우지 않고 오류로 낸다.
fn link_markdown_images(
    doc: &Document,
    md_path: &std::path::Path,
    mut md: String,
    bin_ids: &[u16],
    written: &mut Vec<String>,
) -> Result<String, ErrorData> {
    if bin_ids.is_empty() {
        return Ok(md);
    }
    let stem = md_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document");
    let dir_name = format!("{stem}_assets");
    let dir = md_path.with_file_name(&dir_name);
    std::fs::create_dir_all(&dir)
        .map_err(|e| io_err(&format!("폴더 만들기 실패 {}", dir.display()), e))?;
    for (i, &bin_id) in bin_ids.iter().enumerate() {
        let content = doc
            .bin_data_content
            .iter()
            .find(|c| c.id == bin_id)
            .ok_or_else(|| {
                ErrorData::internal_error(format!("그림 BinData {bin_id} 를 찾을 수 없다"), None)
            })?;
        let file = format!(
            "img{:03}.{}",
            written.len() + 1,
            content.extension.to_ascii_lowercase()
        );
        let path = dir.join(&file);
        std::fs::write(&path, content.data.load())
            .map_err(|e| io_err(&format!("파일 쓰기 실패 {}", path.display()), e))?;
        written.push(path.display().to_string());
        md = md.replace(
            &format!("[[RHWP_IMAGE:{}]]", i + 1),
            &format!("![image]({dir_name}/{file})"),
        );
    }
    Ok(md)
}

/// 본문 문단, 또는 그 문단 속 표 셀 문단의 주소. `cell` 은 (표 컨트롤, 셀, 셀 문단).
#[derive(Clone, Copy)]
struct ParaLoc {
    section: usize,
    para: usize,
    cell: Option<(usize, usize, usize)>,
}

impl ParaLoc {
    fn of(section: usize, para: usize, cell: Option<&CellTarget>) -> Self {
        Self {
            section,
            para,
            cell: cell.map(|c| (c.control, c.cell, c.cell_para())),
        }
    }

    /// `{section, para}` 에 셀 문단이면 `cell: {control, cell, cell_para}` 를 더한 위치 객체.
    fn json(self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("section".into(), json!(self.section));
        m.insert("para".into(), json!(self.para));
        if let Some((control, cell, cell_para)) = self.cell {
            m.insert(
                "cell".into(),
                json!({"control": control, "cell": cell, "cell_para": cell_para}),
            );
        }
        m
    }

    fn hyperlink_target(self) -> HyperlinkTarget {
        HyperlinkTarget {
            section: self.section,
            para: self.para,
            cell_path: self.cell.into_iter().collect(),
        }
    }
}

/// 문서 순서로 모든 본문 문단과 그 문단 속 표 셀 문단을 방문한다.
fn for_each_para(doc: &Document, mut f: impl FnMut(ParaLoc, &rhwp::model::paragraph::Paragraph)) {
    for (section, sec) in doc.sections.iter().enumerate() {
        for (para, p) in sec.paragraphs.iter().enumerate() {
            f(
                ParaLoc {
                    section,
                    para,
                    cell: None,
                },
                p,
            );
            for (ci, ctrl) in p.controls.iter().enumerate() {
                if let Control::Table(t) = ctrl {
                    for (cell_idx, cell) in t.cells.iter().enumerate() {
                        for (cp, cell_para) in cell.paragraphs.iter().enumerate() {
                            let cell = Some((ci, cell_idx, cp));
                            f(
                                ParaLoc {
                                    section,
                                    para,
                                    cell,
                                },
                                cell_para,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// 책갈피 이름: 비어 있지 않은 한 줄.
fn check_bookmark_name(name: &str, field: &str) -> Result<(), ErrorData> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(ErrorData::invalid_params(
            format!("{field} 은(는) 비어 있지 않은 한 줄이어야 한다"),
            None,
        ));
    }
    Ok(())
}

/// rhwp 책갈피 native 는 거부를 `Ok({"ok":false,"error":..})` 로 돌려주므로 오류로 바꾼다.
fn bookmark_result(out: String) -> Result<(), ErrorData> {
    let v: Value = serde_json::from_str(&out).map_err(|e| {
        ErrorData::internal_error(format!("책갈피 결과 해석 실패: {e}: {out}"), None)
    })?;
    if v["ok"] == json!(true) {
        Ok(())
    } else {
        Err(ErrorData::invalid_params(
            v["error"].as_str().unwrap_or(&out).to_string(),
            None,
        ))
    }
}

/// 이름이 `name` 인 본문 문단 책갈피의 (section, para, control).
/// rhwp 책갈피 삭제·이름 변경은 본문 문단만 다루므로 셀 안 책갈피는 거부한다.
fn body_bookmark(doc: &Document, name: &str) -> Result<(usize, usize, usize), ErrorData> {
    let mut found = None;
    for_each_para(doc, |loc, p| {
        for (ci, c) in p.controls.iter().enumerate() {
            if matches!(c, Control::Bookmark(b) if b.name == name) && found.is_none() {
                found = Some((loc, ci));
            }
        }
    });
    match found {
        Some((
            ParaLoc {
                section,
                para,
                cell: None,
            },
            ci,
        )) => Ok((section, para, ci)),
        Some(_) => Err(ErrorData::invalid_params(
            format!("책갈피 '{name}' 은 표 셀 안에 있어 바꿀 수 없다 (본문 책갈피만 지원)"),
            None,
        )),
        None => Err(ErrorData::invalid_params(
            format!("책갈피 '{name}' 이 없다"),
            None,
        )),
    }
}

/// 본문 문단, 또는 cell 이면 그 표 셀 문단의 수식.
fn equation_at<'a>(
    doc: &'a Document,
    section: usize,
    para: usize,
    cell: Option<&CellTarget>,
    control: usize,
) -> Result<&'a rhwp::model::control::Equation, ErrorData> {
    match paragraph_at(doc, section, para, cell)?
        .controls
        .get(control)
    {
        Some(Control::Equation(eq)) => Ok(eq),
        _ => Err(ErrorData::invalid_params(
            format!("문단 {section}/{para} 의 컨트롤 {control} 은 수식이 아니다"),
            None,
        )),
    }
}

fn equation_json(eq: &rhwp::model::control::Equation) -> Value {
    json!({
        "script": eq.script,
        "font_size_pt": f64::from(eq.font_size) / 100.0,
        "color": color_css(eq.color),
    })
}

/// 수식 스크립트와 글자 크기·색을 검사해 rhwp 값(HWPUNIT, 0x00BBGGRR)으로 바꾼다.
/// rhwp 속성 JSON 의 문자열 해석은 \n·\r·\t 외의 이스케이프를 풀지 않는다.
fn check_equation(
    script: Option<&str>,
    font_size_pt: Option<f64>,
    color: Option<&str>,
) -> Result<(Option<u32>, Option<u32>), ErrorData> {
    if let Some(s) = script {
        if s.trim().is_empty()
            || s.chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err(ErrorData::invalid_params(
                "script 는 비어 있지 않아야 하고 줄바꿈·탭 외의 제어 문자를 쓸 수 없다",
                None,
            ));
        }
    }
    let size = font_size_pt
        .map(|v| check_range("font_size_pt", v, 1.0, 1000.0).map(|_| (v * 100.0).round() as u32))
        .transpose()?;
    let color = color
        .map(|c| check_color("color", c).map(|_| color_bgr(c)))
        .transpose()?;
    Ok((size, color))
}

/// 본문 문단 (section, para) 의 컨트롤 control 인 표.
fn table_at(
    doc: &Document,
    section: usize,
    para: usize,
    control: usize,
) -> Result<&rhwp::model::table::Table, ErrorData> {
    match paragraph_at(doc, section, para, None)?
        .controls
        .get(control)
    {
        Some(Control::Table(t)) => Ok(t),
        _ => Err(ErrorData::invalid_params(
            format!("문단 {section}/{para} 의 컨트롤 {control} 은 표가 아니다"),
            None,
        )),
    }
}

/// 본문 문단 (section, para) 의 컨트롤 control 인 그림 (묶음 안 그림 개체 포함,
/// rhwp `set_picture_properties_native` 가 찾는 범위와 같다).
fn picture_at(
    doc: &Document,
    section: usize,
    para: usize,
    control: usize,
) -> Result<&rhwp::model::image::Picture, ErrorData> {
    match paragraph_at(doc, section, para, None)?
        .controls
        .get(control)
    {
        Some(Control::Picture(pic)) => Ok(pic),
        Some(Control::Shape(shape)) => match shape.as_ref() {
            rhwp::model::shape::ShapeObject::Picture(pic) => Ok(pic),
            _ => Err(not_picture(section, para, control)),
        },
        _ => Err(not_picture(section, para, control)),
    }
}

fn not_picture(section: usize, para: usize, control: usize) -> ErrorData {
    ErrorData::invalid_params(
        format!("문단 {section}/{para} 의 컨트롤 {control} 은 그림이 아니다"),
        None,
    )
}

/// 본문 문단, 또는 cell 이면 그 표 셀 문단의 양식 개체.
fn form_at<'a>(
    doc: &'a Document,
    section: usize,
    para: usize,
    cell: Option<&CellTarget>,
    control: usize,
) -> Result<&'a FormObject, ErrorData> {
    match paragraph_at(doc, section, para, cell)?
        .controls
        .get(control)
    {
        Some(Control::Form(f)) => Ok(f),
        _ => Err(ErrorData::invalid_params(
            format!("({section}, {para}, {control}) 위치에 양식 개체 없음"),
            None,
        )),
    }
}

/// list_forms 항목과 get_form_value 결과의 공통 값 필드 (rhwp get_form_value_native 와 같은 키).
fn form_value_json(f: &FormObject) -> Value {
    json!({
        "formType": form_type_str(f.form_type),
        "name": f.name,
        "value": f.value,
        "text": f.text,
        "caption": f.caption,
        "enabled": f.enabled,
    })
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
pub struct RenderPageParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 쪽 번호 (0-기반, open_document의 page_count 미만)
    pub page: u32,
    /// 출력 파일 경로. 확장자 .svg 또는 .pdf로 형식을 고른다
    pub output_path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ExportDocumentParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 출력 파일 경로. 확장자 .pdf / .md / .txt 로 형식을 고른다
    pub output_path: String,
    /// 첫 쪽 (0-기반, 포함). 생략하면 0
    pub page_start: Option<u32>,
    /// 끝 쪽 (0-기반, 포함). 생략하면 마지막 쪽
    pub page_end: Option<u32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct AddBookmarkParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스
    pub para: usize,
    /// 문단 내 위치 (char 단위)
    pub char_offset: usize,
    /// 책갈피 이름 (문서 안에서 유일)
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BookmarkNameParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 책갈피 이름
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RenameBookmarkParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 지금 이름
    pub name: String,
    /// 새 이름 (문서 안에서 유일)
    pub new_name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertHyperlinkParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스 (cell 이 있으면 그 표가 속한 문단)
    pub para: usize,
    /// 표 셀 안 문단이면 그 셀
    pub cell: Option<CellTarget>,
    /// 링크를 걸 글자 범위 시작 (char 단위, 포함)
    pub start: usize,
    /// 링크를 걸 글자 범위 끝 (char 단위, 제외)
    pub end: usize,
    /// http:// 또는 https:// 절대 주소
    pub url: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct HyperlinkAtParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스 (cell 이 있으면 그 표가 속한 문단)
    pub para: usize,
    /// 표 셀 안 문단이면 그 셀
    pub cell: Option<CellTarget>,
    /// list_hyperlinks 의 field_id
    pub field_id: u32,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UpdateHyperlinkParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스 (cell 이 있으면 그 표가 속한 문단)
    pub para: usize,
    /// 표 셀 안 문단이면 그 셀
    pub cell: Option<CellTarget>,
    /// list_hyperlinks 의 field_id
    pub field_id: u32,
    /// 새 주소 (http:// 또는 https://)
    pub url: Option<String>,
    /// 새 표시 글자 (한 줄)
    pub text: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertEquationParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스
    pub para: usize,
    /// 문단 내 삽입 위치 (char 단위)
    pub char_offset: usize,
    /// 한글 수식 스크립트 (예: "1 over 2", "x^2 + y^2 = r^2", "sqrt {a+b}")
    pub script: String,
    /// 글자 크기 (pt, 기본 10)
    pub font_size_pt: Option<f64>,
    /// 글자 색 "#RRGGBB" (기본 검정)
    pub color: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct EquationAtParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스 (cell 이 있으면 그 표가 속한 문단)
    pub para: usize,
    /// 표 셀 안 문단이면 그 셀
    pub cell: Option<CellTarget>,
    /// 문단(셀이면 셀 문단) 안 수식 컨트롤 인덱스
    pub control: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetEquationParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스 (cell 이 있으면 그 표가 속한 문단)
    pub para: usize,
    /// 표 셀 안 문단이면 그 셀
    pub cell: Option<CellTarget>,
    /// 문단(셀이면 셀 문단) 안 수식 컨트롤 인덱스
    pub control: usize,
    /// 새 수식 스크립트
    pub script: Option<String>,
    /// 새 글자 크기 (pt)
    pub font_size_pt: Option<f64>,
    /// 새 글자 색 "#RRGGBB"
    pub color: Option<String>,
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
    /// 지정하면 본문 대신 표 셀 문단을 편집한다 (para 는 표가 속한 문단)
    pub cell: Option<CellTarget>,
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
    /// 지정하면 본문 대신 표 셀 문단을 편집한다 (para 는 표가 속한 문단)
    pub cell: Option<CellTarget>,
}

/// 나누기 종류.
#[derive(Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum BreakKind {
    /// 쪽 나누기 (Ctrl+Enter)
    Page,
    /// 단 나누기 (Ctrl+Shift+Enter). 1단 문서에서는 쪽 나누기처럼 동작한다
    Column,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertBreakParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 문단 인덱스
    pub para: usize,
    /// 나눌 위치 (char 단위). 이 위치 뒤 글자가 새 쪽(단)의 첫 문단이 된다
    pub char_offset: usize,
    /// 나누기 종류 (기본 page)
    pub kind: Option<BreakKind>,
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
    /// 지정하면 본문 대신 표 셀 문단을 편집한다 (para 는 표가 속한 문단)
    pub cell: Option<CellTarget>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MergeParagraphParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 이전 문단에 합칠 문단 인덱스 (cell 이면 표가 속한 문단, 합칠 셀 문단은 cell.cell_para ≥ 1)
    pub para: usize,
    /// 지정하면 본문 대신 표 셀 문단을 편집한다 (para 는 표가 속한 문단)
    pub cell: Option<CellTarget>,
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
pub struct CreateTableParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표를 삽입할 문단 인덱스
    pub para: usize,
    /// 삽입 위치 (char 단위). 문단 중간이면 문단을 나눠 그 사이에 표 문단을 넣는다
    pub char_offset: usize,
    /// 행 수 (1 이상)
    pub rows: u16,
    /// 열 수 (1~256)
    pub cols: u16,
    /// 열 폭 비율(%) 목록. 길이는 cols, 합은 100. column_widths와 함께 쓸 수 없다
    pub column_widths_percent: Option<Vec<f64>>,
    /// 열 폭 절대값(HWPUNIT) 목록. 길이는 cols. 생략하면 본문 폭을 균등 분할
    pub column_widths: Option<Vec<u32>>,
    /// true면 첫 행을 쪽마다 반복되는 머리행으로 지정한다
    pub repeat_header: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MergeTableCellsParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 병합 시작 행 (포함)
    pub start_row: u16,
    /// 병합 시작 열 (포함)
    pub start_col: u16,
    /// 병합 끝 행 (포함)
    pub end_row: u16,
    /// 병합 끝 열 (포함)
    pub end_col: u16,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ApplyTableFormatParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 적용할 표 서식 (get_table_format 결과와 같은 모양, 지정한 항목만 바뀐다)
    pub format: TableFormat,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ApplyPictureFormatParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 그림이 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스 (get_structure objects 의 control 값)
    pub control: usize,
    /// 적용할 그림 서식 (get_picture_format 결과와 같은 모양, 지정한 항목만 바뀐다)
    pub format: PictureFormat,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GetCellFormatParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 셀 행 (병합 셀이면 왼쪽 위 칸)
    pub row: u16,
    /// 셀 열 (병합 셀이면 왼쪽 위 칸)
    pub col: u16,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ApplyCellFormatParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 범위 시작 행 (포함)
    pub start_row: u16,
    /// 범위 시작 열 (포함)
    pub start_col: u16,
    /// 범위 끝 행 (포함, 생략 시 start_row)
    pub end_row: Option<u16>,
    /// 범위 끝 열 (포함, 생략 시 start_col)
    pub end_col: Option<u16>,
    /// 적용할 셀 서식 (get_cell_format 결과와 같은 모양, 지정한 항목만 바뀐다)
    pub format: CellFormat,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SplitTableCellParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 표가 속한 문단 인덱스
    pub para: usize,
    /// 문단 내 컨트롤 인덱스
    pub control: usize,
    /// 대상 셀의 행 (병합 셀이면 왼쪽 위 행)
    pub row: u16,
    /// 대상 셀의 열 (병합 셀이면 왼쪽 위 열)
    pub col: u16,
    /// 나눌 줄 수. rows/cols를 모두 생략하면 병합만 해제한다
    pub rows: Option<u16>,
    /// 나눌 칸 수. rows/cols를 모두 생략하면 병합만 해제한다
    pub cols: Option<u16>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertPictureParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 그림을 넣을 본문 문단 인덱스
    pub para: usize,
    /// 문단 내 삽입 위치 (char 단위)
    pub char_offset: usize,
    /// 그림 파일의 절대 경로 (PNG/JPEG/GIF/BMP/TIFF)
    pub image_path: String,
    /// 그림 폭 (mm). 생략하면 height_mm 비율 또는 원본 크기(96 DPI)
    pub width_mm: Option<f64>,
    /// 그림 높이 (mm). 생략하면 width_mm 비율 또는 원본 크기(96 DPI)
    pub height_mm: Option<f64>,
    /// true(기본)면 글자처럼 취급해 문장 흐름 안에 놓고, false면 쪽 기준 떠 있는 그림으로 놓는다
    pub inline: Option<bool>,
    /// 떠 있는 그림의 가로 위치 (용지 왼쪽에서 mm, inline=false일 때만). 생략 시 본문 왼쪽
    pub x_mm: Option<f64>,
    /// 떠 있는 그림의 세로 위치 (용지 위쪽에서 mm, inline=false일 때만). 생략 시 본문 위쪽
    pub y_mm: Option<f64>,
    /// 그림 설명(대체 텍스트)
    pub description: Option<String>,
}

/// 주석 종류.
#[derive(Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// 각주 (쪽 아래)
    Footnote,
    /// 미주 (문서 끝)
    Endnote,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InsertNoteParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// 본문 문단 인덱스
    pub para: usize,
    /// 주석 번호를 넣을 위치 (char 단위)
    pub char_offset: usize,
    /// 주석 내용 (한 줄)
    pub text: String,
    /// 각주(footnote, 기본) 또는 미주(endnote)
    pub kind: Option<NoteKind>,
}

/// 머리말/꼬리말 구분.
#[derive(Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum HeaderFooterKind {
    Header,
    Footer,
}

/// 머리말/꼬리말이 적용될 쪽.
#[derive(Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum HeaderFooterPages {
    /// 양쪽 (기본)
    #[default]
    Both,
    /// 짝수 쪽
    Even,
    /// 홀수 쪽
    Odd,
}

impl HeaderFooterPages {
    /// rhwp `header_footer_apply_from_u8` 의 숫자 (0=양쪽, 1=짝수, 2=홀수).
    fn rhwp_code(self) -> u8 {
        match self {
            Self::Both => 0,
            Self::Even => 1,
            Self::Odd => 2,
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct HeaderFooterParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// header 또는 footer
    pub kind: HeaderFooterKind,
    /// 적용 쪽: both(기본) / even / odd
    pub pages: Option<HeaderFooterPages>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetHeaderFooterParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스
    pub section: usize,
    /// header 또는 footer
    pub kind: HeaderFooterKind,
    /// 적용 쪽: both(기본) / even / odd
    pub pages: Option<HeaderFooterPages>,
    /// 내용 (한 줄). {page}=현재 쪽 번호, {total_pages}=전체 쪽 수, {file_name}=파일 이름
    pub text: String,
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
    /// 문단 내 컨트롤 인덱스 (list_forms의 control 값). cell 이면 셀 문단 안의 인덱스
    pub control: usize,
    /// 표 셀 안의 양식이면 그 셀 (list_forms의 cell 값). para 는 표가 속한 문단
    pub cell: Option<CellTarget>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SectionParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스 (0-기반)
    pub section: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetPageSetupParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스 (0-기반)
    pub section: usize,
    /// 바꿀 쪽 설정 (지정한 항목만 바뀐다. get_page_setup 결과와 같은 모양)
    pub setup: PageSetup,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ObjectAtParams {
    /// 문서 핸들
    pub doc_id: String,
    /// 구역 인덱스 (0-기반)
    pub section: usize,
    /// 문단 인덱스 (0-기반)
    pub para: usize,
    /// 문단 내 컨트롤 인덱스 (get_structure objects 의 control 값)
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
    /// 문단 내 컨트롤 인덱스 (list_forms의 control 값). cell 이면 셀 문단 안의 인덱스
    pub control: usize,
    /// 표 셀 안의 양식이면 그 셀 (list_forms의 cell 값). para 는 표가 속한 문단
    pub cell: Option<CellTarget>,
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
        self.store.with_session_save(&p.doc_id, |session| {
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
            Ok(json!({
                "saved_path": target.display().to_string(),
                "bytes": bytes.len(),
            })
            .to_string())
        })
    }

    #[tool(
        description = "구역의 쪽 설정(용지 크기·방향, 여백, 제본)을 mm 단위로 반환한다. 결과는 set_page_setup의 setup 입력과 같은 모양."
    )]
    pub fn get_page_setup(
        &self,
        Parameters(p): Parameters<SectionParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let section = session
                .core
                .document()
                .sections
                .get(p.section)
                .ok_or_else(|| {
                    ErrorData::invalid_params(format!("구역 {} 이 없다", p.section), None)
                })?;
            Ok(json!(PageSetup::from_page_def(&section.section_def.page_def)).to_string())
        })
    }

    #[tool(
        description = "구역의 쪽 설정을 바꾼다: paper_width_mm/paper_height_mm(세로 기준, A4=210×297), orientation(portrait/landscape), margin_*_mm(left/right/top/bottom/header/footer/gutter), binding(single/duplex/top_flip). 지정한 항목만 바뀌고, 여백이 용지를 넘으면 오류."
    )]
    pub fn set_page_setup(
        &self,
        Parameters(p): Parameters<SetPageSetupParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let section = session
                .core
                .document()
                .sections
                .get(p.section)
                .ok_or_else(|| {
                    ErrorData::invalid_params(format!("구역 {} 이 없다", p.section), None)
                })?;
            let props = p.setup.to_rhwp_json(&section.section_def.page_def)?;
            session
                .core
                .set_page_def_native(p.section, &props)
                .map_err(hwp_err)?;
            let pd = &session.core.document().sections[p.section]
                .section_def
                .page_def;
            Ok(json!(PageSetup::from_page_def(pd)).to_string())
        })
    }

    #[tool(
        description = "문서의 한 쪽을 조판해 SVG(.svg) 또는 PDF(.pdf) 파일로 쓴다. 편집 결과를 저장 전에 눈으로 확인할 때 쓴다. page는 0-기반."
    )]
    pub fn render_page(
        &self,
        Parameters(p): Parameters<RenderPageParams>,
    ) -> Result<String, ErrorData> {
        let target = PathBuf::from(&p.output_path);
        let ext = target
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        self.store.with_session(&p.doc_id, |session| {
            let page_count = session.core.page_count();
            if p.page >= page_count {
                return Err(ErrorData::invalid_params(
                    format!(
                        "page {} 가 범위를 넘는다 (전체 {page_count}쪽, 0-기반)",
                        p.page
                    ),
                    None,
                ));
            }
            let bytes = match ext.as_deref() {
                Some("svg") => session
                    .core
                    .render_page_svg_native(p.page)
                    .map(String::into_bytes),
                Some("pdf") => session.core.render_pages_pdf_native(&[p.page]),
                _ => {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "출력 경로 확장자는 .svg 또는 .pdf여야 한다: {}",
                            target.display()
                        ),
                        None,
                    ))
                }
            }
            .map_err(hwp_err)?;
            std::fs::write(&target, &bytes)
                .map_err(|e| io_err(&format!("파일 쓰기 실패 {}", target.display()), e))?;
            Ok(json!({
                "page": p.page,
                "page_count": page_count,
                "path": target.display().to_string(),
                "bytes": bytes.len(),
            })
            .to_string())
        })
    }

    #[tool(
        description = "문서(또는 page_start..=page_end 쪽)를 조판 결과대로 한 파일로 내보낸다. 확장자로 형식을 고른다: .pdf(여러 쪽 PDF), .md(Markdown: 표는 Markdown 표, 그림은 <파일이름>_assets/ 폴더에 써서 링크), .txt(쪽에 보이는 줄 순서의 평문). page는 0-기반."
    )]
    pub fn export_document(
        &self,
        Parameters(p): Parameters<ExportDocumentParams>,
    ) -> Result<String, ErrorData> {
        let target = PathBuf::from(&p.output_path);
        let ext = target
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if !matches!(ext.as_deref(), Some("pdf" | "md" | "txt")) {
            return Err(ErrorData::invalid_params(
                format!(
                    "출력 경로 확장자는 .pdf / .md / .txt 여야 한다: {}",
                    target.display()
                ),
                None,
            ));
        }
        self.store.with_session(&p.doc_id, |session| {
            let core = &session.core;
            let page_count = core.page_count();
            let start = p.page_start.unwrap_or(0);
            let end = p.page_end.unwrap_or(page_count - 1);
            if start > end || end >= page_count {
                return Err(ErrorData::invalid_params(
                    format!("쪽 범위 {start}..={end} 잘못됨 (전체 {page_count}쪽, 0-기반)"),
                    None,
                ));
            }
            let pages: Vec<u32> = (start..=end).collect();
            let mut images = Vec::new();
            let bytes = match ext.as_deref() {
                Some("pdf") => core.render_pages_pdf_native(&pages).map_err(hwp_err)?,
                Some("txt") => pages
                    .iter()
                    .map(|&n| core.extract_page_text_native(n))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(hwp_err)?
                    .join("\n")
                    .into_bytes(),
                _ => {
                    let mut out = Vec::new();
                    for &n in &pages {
                        let (md, refs) = core
                            .extract_page_markdown_with_images_native(n)
                            .map_err(hwp_err)?;
                        out.push(link_markdown_images(
                            core.document(),
                            &target,
                            md,
                            &refs.iter().map(|r| r.3).collect::<Vec<_>>(),
                            &mut images,
                        )?);
                    }
                    out.join("\n\n").into_bytes()
                }
            };
            std::fs::write(&target, &bytes)
                .map_err(|e| io_err(&format!("파일 쓰기 실패 {}", target.display()), e))?;
            Ok(json!({
                "pages": [start, end],
                "page_count": page_count,
                "path": target.display().to_string(),
                "bytes": bytes.len(),
                "images": images,
            })
            .to_string())
        })
    }

    #[tool(
        description = "문서의 책갈피를 문서 순서로 나열한다: name, section/para/control, char_offset (표 셀 안이면 cell)."
    )]
    pub fn list_bookmarks(
        &self,
        Parameters(p): Parameters<DocIdParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let mut out = Vec::new();
            for_each_para(session.core.document(), |loc, para| {
                for ((ci, c), at) in para
                    .controls
                    .iter()
                    .enumerate()
                    .zip(para.control_text_positions())
                {
                    if let Control::Bookmark(b) = c {
                        let mut entry = loc.json();
                        entry.insert("name".into(), json!(b.name));
                        entry.insert("control".into(), json!(ci));
                        entry.insert("char_offset".into(), json!(at));
                        out.push(Value::Object(entry));
                    }
                }
            });
            Ok(json!({ "bookmarks": out }).to_string())
        })
    }

    #[tool(
        description = "본문 문단의 char_offset 위치에 책갈피를 넣는다. 이름은 문서 안에서 유일해야 한다."
    )]
    pub fn add_bookmark(
        &self,
        Parameters(p): Parameters<AddBookmarkParams>,
    ) -> Result<String, ErrorData> {
        check_bookmark_name(&p.name, "name")?;
        self.store.with_session_edit(&p.doc_id, |session| {
            check_offset(
                &session.core,
                p.section,
                p.para,
                None,
                "char_offset",
                p.char_offset,
            )?;
            bookmark_result(
                session
                    .core
                    .add_bookmark_native(p.section, p.para, p.char_offset, &p.name)
                    .map_err(hwp_err)?,
            )?;
            Ok(json!({"name": p.name}).to_string())
        })
    }

    #[tool(description = "이름으로 본문 책갈피를 지운다.")]
    pub fn delete_bookmark(
        &self,
        Parameters(p): Parameters<BookmarkNameParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let (s, para, c) = body_bookmark(session.core.document(), &p.name)?;
            bookmark_result(
                session
                    .core
                    .delete_bookmark_native(s, para, c)
                    .map_err(hwp_err)?,
            )?;
            Ok(json!({"deleted": p.name}).to_string())
        })
    }

    #[tool(description = "본문 책갈피 이름을 바꾼다. 새 이름은 문서 안에서 유일해야 한다.")]
    pub fn rename_bookmark(
        &self,
        Parameters(p): Parameters<RenameBookmarkParams>,
    ) -> Result<String, ErrorData> {
        check_bookmark_name(&p.new_name, "new_name")?;
        self.store.with_session_edit(&p.doc_id, |session| {
            let (s, para, c) = body_bookmark(session.core.document(), &p.name)?;
            bookmark_result(
                session
                    .core
                    .rename_bookmark_native(s, para, c, &p.new_name)
                    .map_err(hwp_err)?,
            )?;
            Ok(json!({"name": p.new_name}).to_string())
        })
    }

    #[tool(
        description = "문서의 하이퍼링크를 문서 순서로 나열한다: section/para(표 셀 안이면 cell), field_id, start/end(char 범위, end 제외), text, url."
    )]
    pub fn list_hyperlinks(
        &self,
        Parameters(p): Parameters<DocIdParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let mut out = Vec::new();
            let mut failed = None;
            for_each_para(session.core.document(), |loc, _| {
                match session.core.hyperlinks_native(&loc.hyperlink_target()) {
                    Ok(links) => out.extend(links.into_iter().map(|l| {
                        let mut entry = loc.json();
                        entry.insert("field_id".into(), json!(l.field_id));
                        entry.insert("start".into(), json!(l.start));
                        entry.insert("end".into(), json!(l.end));
                        entry.insert("text".into(), json!(l.text));
                        entry.insert("url".into(), json!(l.uri));
                        Value::Object(entry)
                    })),
                    Err(e) if failed.is_none() => failed = Some(e),
                    Err(_) => {}
                }
            });
            if let Some(e) = failed {
                return Err(hwp_err(e));
            }
            Ok(json!({ "hyperlinks": out }).to_string())
        })
    }

    #[tool(
        description = "문단(표 셀 안이면 cell)의 글자 범위 start..end(end 제외, char 단위)에 웹 링크(http/https)를 건다. 글자와 서식은 그대로 둔다(파란 밑줄 등은 apply_char_format). 다른 필드와 겹치면 거부한다. 결과의 field_id 로 고치거나 지운다."
    )]
    pub fn insert_hyperlink(
        &self,
        Parameters(p): Parameters<InsertHyperlinkParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            check_offset(
                &session.core,
                p.section,
                p.para,
                p.cell.as_ref(),
                "end",
                p.end,
            )?;
            let target = ParaLoc::of(p.section, p.para, p.cell.as_ref()).hyperlink_target();
            let field_id = session
                .core
                .insert_hyperlink_native(&target, p.start, p.end, &p.url)
                .map_err(hwp_err)?;
            Ok(json!({"field_id": field_id}).to_string())
        })
    }

    #[tool(
        description = "하이퍼링크의 주소(url)와/또는 표시 글자(text)를 바꾼다. 위치와 field_id 는 list_hyperlinks 로 파악한다."
    )]
    pub fn update_hyperlink(
        &self,
        Parameters(p): Parameters<UpdateHyperlinkParams>,
    ) -> Result<String, ErrorData> {
        if p.url.is_none() && p.text.is_none() {
            return Err(ErrorData::invalid_params(
                "url 과 text 중 하나는 있어야 한다",
                None,
            ));
        }
        self.store.with_session_edit(&p.doc_id, |session| {
            paragraph_at(session.core.document(), p.section, p.para, p.cell.as_ref())?;
            let target = ParaLoc::of(p.section, p.para, p.cell.as_ref()).hyperlink_target();
            if let Some(url) = &p.url {
                session
                    .core
                    .update_hyperlink_native(&target, p.field_id, url)
                    .map_err(hwp_err)?;
            }
            if let Some(text) = &p.text {
                session
                    .core
                    .replace_hyperlink_text_native(&target, p.field_id, text)
                    .map_err(hwp_err)?;
            }
            Ok(json!({"field_id": p.field_id}).to_string())
        })
    }

    #[tool(
        description = "하이퍼링크를 푼다. 표시 글자와 서식은 남는다. 위치와 field_id 는 list_hyperlinks 로 파악한다."
    )]
    pub fn remove_hyperlink(
        &self,
        Parameters(p): Parameters<HyperlinkAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            paragraph_at(session.core.document(), p.section, p.para, p.cell.as_ref())?;
            let target = ParaLoc::of(p.section, p.para, p.cell.as_ref()).hyperlink_target();
            session
                .core
                .remove_hyperlink_native(&target, p.field_id)
                .map_err(hwp_err)?;
            Ok(json!({"removed": p.field_id}).to_string())
        })
    }

    #[tool(
        description = "본문 문단의 char_offset 위치에 수식을 글자처럼 넣는다. script 는 한글 수식 문법(예: \"1 over 2\", \"x^2\", \"sqrt {a+b}\", \"sum from {i=1} to n\"). 크기는 스크립트로 자동 계산된다."
    )]
    pub fn insert_equation(
        &self,
        Parameters(p): Parameters<InsertEquationParams>,
    ) -> Result<String, ErrorData> {
        let (size, color) = check_equation(
            Some(&p.script),
            Some(p.font_size_pt.unwrap_or(10.0)),
            p.color.as_deref(),
        )?;
        self.store.with_session_edit(&p.doc_id, |session| {
            check_offset(
                &session.core,
                p.section,
                p.para,
                None,
                "char_offset",
                p.char_offset,
            )?;
            let out = session
                .core
                .insert_equation_native(
                    p.section,
                    p.para,
                    p.char_offset,
                    &p.script,
                    size.expect("기본 크기"),
                    color.unwrap_or(0),
                )
                .map_err(hwp_err)?;
            let v: Value = serde_json::from_str(&out).map_err(|e| {
                ErrorData::internal_error(format!("insert_equation 결과 해석 실패: {e}"), None)
            })?;
            Ok(json!({"para": v["paraIdx"], "control": v["controlIdx"]}).to_string())
        })
    }

    #[tool(
        description = "수식의 script, font_size_pt, color 를 반환한다 (표 셀 안이면 cell). 위치는 get_structure objects 로 파악한다."
    )]
    pub fn get_equation(
        &self,
        Parameters(p): Parameters<EquationAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let eq = equation_at(
                session.core.document(),
                p.section,
                p.para,
                p.cell.as_ref(),
                p.control,
            )?;
            Ok(equation_json(eq).to_string())
        })
    }

    #[tool(
        description = "수식의 script / font_size_pt / color 를 바꾼다 (지정한 것만). 스크립트나 크기를 바꾸면 수식 크기가 다시 계산된다. 표 셀 안(cell)은 그 셀 문단의 첫 수식만 바꿀 수 있다."
    )]
    pub fn set_equation(
        &self,
        Parameters(p): Parameters<SetEquationParams>,
    ) -> Result<String, ErrorData> {
        let (size, color) =
            check_equation(p.script.as_deref(), p.font_size_pt, p.color.as_deref())?;
        let mut props = serde_json::Map::new();
        if let Some(s) = &p.script {
            props.insert("script".into(), json!(s));
        }
        if let Some(v) = size {
            props.insert("fontSize".into(), json!(v));
        }
        if let Some(v) = color {
            props.insert("color".into(), json!(v));
        }
        if props.is_empty() {
            return Err(ErrorData::invalid_params(
                "script / font_size_pt / color 중 하나는 있어야 한다",
                None,
            ));
        }
        let props = Value::Object(props).to_string();
        self.store.with_session_edit(&p.doc_id, |session| {
            let doc = session.core.document();
            equation_at(doc, p.section, p.para, p.cell.as_ref(), p.control)?;
            // rhwp 는 셀 문단에서 컨트롤 번호 없이 첫 수식을 고친다.
            let (table_ctrl, cell_idx, cell_para) = match &p.cell {
                Some(c) => {
                    let first = paragraph_at(doc, p.section, p.para, Some(c))?
                        .controls
                        .iter()
                        .position(|ctrl| matches!(ctrl, Control::Equation(_)));
                    if first != Some(p.control) {
                        return Err(ErrorData::invalid_params(
                            "표 셀 문단에서는 첫 수식만 바꿀 수 있다 (rhwp 제약)",
                            None,
                        ));
                    }
                    (c.control, Some(c.cell), Some(c.cell_para()))
                }
                None => (p.control, None, None),
            };
            session
                .core
                .set_equation_properties_native(
                    p.section, p.para, table_ctrl, cell_idx, cell_para, &props,
                )
                .map_err(hwp_err)?;
            let eq = equation_at(
                session.core.document(),
                p.section,
                p.para,
                p.cell.as_ref(),
                p.control,
            )?;
            Ok(equation_json(eq).to_string())
        })
    }

    #[tool(
        description = "이 문서에서 마지막으로 성공한 편집 도구 호출 하나를 통째로 되돌린다 (최대 20단계). 저장한 상태로 돌아오면 dirty=false가 된다."
    )]
    pub fn undo(&self, Parameters(p): Parameters<DocIdParams>) -> Result<String, ErrorData> {
        self.store.undo(&p.doc_id).map(history_json)
    }

    #[tool(
        description = "undo로 되돌린 편집을 다시 적용한다. undo 뒤 새 편집을 하면 다시 할 기록은 사라진다."
    )]
    pub fn redo(&self, Parameters(p): Parameters<DocIdParams>) -> Result<String, ErrorData> {
        self.store.redo(&p.doc_id).map(history_json)
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
        description = "문서 구조를 반환한다: 구역별 문단 목록(인덱스, 텍스트 미리보기, 표 컨트롤, 개체 objects: 표/그림/도형/수식/각주/미주의 control·kind·char_offset). 편집 좌표(section/para/control)를 파악하는 시작점."
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
                            let objects: Vec<_> = para
                                .controls
                                .iter()
                                .zip(para.control_text_positions())
                                .enumerate()
                                .filter_map(|(ctrl_idx, (ctrl, pos))| {
                                    ObjectKind::of(ctrl).map(|kind| {
                                        json!({
                                            "control": ctrl_idx,
                                            "kind": kind.name(),
                                            "char_offset": pos,
                                        })
                                    })
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
                            if !objects.is_empty() {
                                entry["objects"] = json!(objects);
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
        description = "텍스트를 치환한다 (본문과 표 셀 모두, search_text와 같은 매치). all=true(기본)면 전체, false면 문서 순서 첫 매치만. 일치하는 텍스트가 없으면 오류."
    )]
    pub fn replace_text(
        &self,
        Parameters(p): Parameters<ReplaceTextParams>,
    ) -> Result<String, ErrorData> {
        if p.query.is_empty() {
            return Err(ErrorData::invalid_params("query는 비어 있을 수 없음", None));
        }
        self.store.with_session_edit(&p.doc_id, |session| {
            let case_sensitive = p.case_sensitive.unwrap_or(false);
            // replace_one_native 는 본문만 찾으므로(search_first_body) 첫 매치도 전체 치환과
            // 같은 경로(search_all — 셀 포함, 문서 순서)의 0번째로 고른다.
            let out = if p.all.unwrap_or(true) {
                session
                    .core
                    .replace_all_native(&p.query, &p.replacement, case_sensitive)
            } else {
                session
                    .core
                    .replace_nth_native(&p.query, &p.replacement, case_sensitive, 0)
            }
            .map_err(hwp_err)?;
            let v: Value = serde_json::from_str(&out).map_err(|e| {
                ErrorData::internal_error(format!("replace_text 결과 해석 실패: {e}"), None)
            })?;
            if v["count"].as_u64() == Some(0) {
                // 바뀐 것이 없으면 되돌리기 기록·dirty 를 남기지 않도록 오류로 돌려준다.
                return Err(ErrorData::invalid_params(
                    format!("일치하는 텍스트 없음: {:?}", p.query),
                    None,
                ));
            }
            Ok(out)
        })
    }

    // ── 텍스트/문단 편집 ──

    #[tool(description = "문단(또는 cell을 주면 표 셀 문단)의 지정 위치에 텍스트를 삽입한다.")]
    pub fn insert_text(
        &self,
        Parameters(p): Parameters<InsertTextParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let cell = p.cell.as_ref();
            check_offset(
                &session.core,
                p.section,
                p.para,
                cell,
                "char_offset",
                p.char_offset,
            )?;
            match cell {
                Some(c) => session.core.insert_text_in_cell_native(
                    p.section,
                    p.para,
                    c.control,
                    c.cell,
                    c.cell_para(),
                    p.char_offset,
                    &p.text,
                ),
                None => session
                    .core
                    .insert_text_native(p.section, p.para, p.char_offset, &p.text),
            }
            .map_err(hwp_err)
        })
    }

    #[tool(
        description = "문단(또는 cell을 주면 표 셀 문단)의 지정 위치부터 count자의 텍스트를 삭제한다."
    )]
    pub fn delete_text(
        &self,
        Parameters(p): Parameters<DeleteTextParams>,
    ) -> Result<String, ErrorData> {
        if p.count == 0 {
            return Err(ErrorData::invalid_params("count는 1 이상이어야 함", None));
        }
        self.store.with_session_edit(&p.doc_id, |session| {
            let end = p
                .char_offset
                .checked_add(p.count)
                .ok_or_else(|| ErrorData::invalid_params("char_offset + count 가 넘친다", None))?;
            let cell = p.cell.as_ref();
            check_offset(
                &session.core,
                p.section,
                p.para,
                cell,
                "char_offset + count",
                end,
            )?;
            match cell {
                Some(c) => session.core.delete_text_in_cell_native(
                    p.section,
                    p.para,
                    c.control,
                    c.cell,
                    c.cell_para(),
                    p.char_offset,
                    p.count,
                ),
                None => session
                    .core
                    .delete_text_native(p.section, p.para, p.char_offset, p.count),
            }
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

    #[tool(
        description = "문단(또는 cell을 주면 표 셀 문단)을 char_offset 지점에서 두 문단으로 분할한다."
    )]
    pub fn split_paragraph(
        &self,
        Parameters(p): Parameters<SplitParagraphParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let cell = p.cell.as_ref();
            let offset = split_offset(&session.core, p.section, p.para, cell, p.char_offset)?;
            match cell {
                Some(c) => session.core.split_paragraph_in_cell_native(
                    p.section,
                    p.para,
                    c.control,
                    c.cell,
                    c.cell_para(),
                    offset,
                    None,
                ),
                None => session
                    .core
                    .split_paragraph_native(p.section, p.para, offset, None),
            }
            .map_err(hwp_err)
        })
    }

    #[tool(
        description = "문단을 char_offset에서 나누고 뒤 문단을 새 쪽(kind=page, 기본) 또는 새 단(kind=column)에서 시작하게 한다. 반환값의 paraIdx가 새 문단이다."
    )]
    pub fn insert_break(
        &self,
        Parameters(p): Parameters<InsertBreakParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let offset = split_offset(&session.core, p.section, p.para, None, p.char_offset)?;
            match p.kind.unwrap_or(BreakKind::Page) {
                BreakKind::Page => session
                    .core
                    .insert_page_break_native(p.section, p.para, offset),
                BreakKind::Column => session
                    .core
                    .insert_column_break_native(p.section, p.para, offset),
            }
            .map_err(hwp_err)
        })
    }

    #[tool(
        description = "문단을 이전 문단과 병합한다. cell을 주면 표 셀 안의 cell.cell_para 문단(1 이상)을 그 앞 셀 문단과 병합한다."
    )]
    pub fn merge_paragraph(
        &self,
        Parameters(p): Parameters<MergeParagraphParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            match p.cell.as_ref() {
                Some(c) => session.core.merge_paragraph_in_cell_native(
                    p.section,
                    p.para,
                    c.control,
                    c.cell,
                    c.cell_para(),
                ),
                None => session.core.merge_paragraph_native(p.section, p.para),
            }
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
            let len = para_len(&session.core, p.section, p.para, p.cell.as_ref())?;
            let cell = p.cell.as_ref().map(|c| (c.control, c.cell, c.cell_para()));
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
        description = "문단의 char_offset 위치에 rows×cols 새 표를 삽입한다. 반환값의 paraIdx/controlIdx가 표의 para/control 좌표다. 열 폭은 column_widths_percent(합 100) 또는 column_widths(HWPUNIT) 중 하나로 지정, 생략 시 균등."
    )]
    pub fn create_table(
        &self,
        Parameters(p): Parameters<CreateTableParams>,
    ) -> Result<String, ErrorData> {
        let column_widths = match (p.column_widths_percent, p.column_widths) {
            (Some(_), Some(_)) => {
                return Err(ErrorData::invalid_params(
                    "column_widths_percent와 column_widths는 함께 쓸 수 없음",
                    None,
                ))
            }
            (Some(pct), None) => Some(rhwp::document_core::TableColumnWidths::Percent(pct)),
            (None, Some(abs)) => Some(rhwp::document_core::TableColumnWidths::Absolute(abs)),
            (None, None) => None,
        };
        let options = rhwp::document_core::TableCreationOptions {
            column_widths,
            column_alignments: None,
            repeat_header: p.repeat_header,
        };
        self.store.with_session_edit(&p.doc_id, |session| {
            let offset = split_offset(&session.core, p.section, p.para, None, p.char_offset)?;
            session
                .core
                .create_table_with_options_native(
                    p.section, p.para, offset, p.rows, p.cols, &options,
                )
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "표 서식을 반환한다: 배치(inline 글자처럼 취급, text_wrap, horz/vert_rel_to·align·offset_mm, 바깥 margin_*_mm), cell_padding_*_mm(셀 기본 안 여백), cell_spacing_mm, repeat_header. 결과는 apply_table_format의 format 입력과 같은 모양."
    )]
    pub fn get_table_format(
        &self,
        Parameters(p): Parameters<GetTableParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let table = table_at(session.core.document(), p.section, p.para, p.control)?;
            Ok(json!(TableFormat::from_table(table)).to_string())
        })
    }

    #[tool(
        description = "표 서식을 바꾼다: inline(글자처럼 취급), text_wrap(square/top_and_bottom/behind_text/in_front_of_text), horz_rel_to(paper/page/column/para)·horz_align(left/center/right/inside/outside)·horz_offset_mm, vert_rel_to(paper/page/para)·vert_align(top/center/bottom/inside/outside)·vert_offset_mm, margin_*_mm(바깥 여백), cell_padding_*_mm, cell_spacing_mm, repeat_header. 지정한 항목만 바뀐다."
    )]
    pub fn apply_table_format(
        &self,
        Parameters(p): Parameters<ApplyTableFormatParams>,
    ) -> Result<String, ErrorData> {
        let props = p.format.to_rhwp_json()?;
        self.store.with_session_edit(&p.doc_id, |session| {
            table_at(session.core.document(), p.section, p.para, p.control)?;
            session
                .core
                .set_table_properties_native(p.section, p.para, p.control, &props)
                .map_err(hwp_err)?;
            let table = table_at(session.core.document(), p.section, p.para, p.control)?;
            Ok(json!(TableFormat::from_table(table)).to_string())
        })
    }

    #[tool(
        description = "그림 서식을 반환한다: 배치(inline, text_wrap, horz/vert_rel_to·align·offset_mm, 바깥 margin_*_mm), width_mm/height_mm, description, brightness, contrast, transparency, effect, rotation_deg, horz_flip/vert_flip. 결과는 apply_picture_format의 format 입력과 같은 모양."
    )]
    pub fn get_picture_format(
        &self,
        Parameters(p): Parameters<ObjectAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let pic = picture_at(session.core.document(), p.section, p.para, p.control)?;
            Ok(json!(PictureFormat::from_picture(pic)).to_string())
        })
    }

    #[tool(
        description = "그림 서식을 바꾼다: inline(글자처럼 취급), text_wrap(square/tight/through/top_and_bottom/behind_text/in_front_of_text; tight/through는 HWPX에만 저장되고 .hwp 저장에서는 square가 된다), horz_rel_to(paper/page/column/para)·horz_align(left/center/right)·horz_offset_mm, vert_rel_to(paper/page/para)·vert_align(top/center/bottom)·vert_offset_mm, margin_*_mm(바깥 여백), width_mm/height_mm, description, brightness/contrast(-100..100), transparency(0..100), effect(original/gray_scale/black_white/pattern8x8), rotation_deg, horz_flip/vert_flip. 지정한 항목만 바뀐다."
    )]
    pub fn apply_picture_format(
        &self,
        Parameters(p): Parameters<ApplyPictureFormatParams>,
    ) -> Result<String, ErrorData> {
        let props = p.format.to_rhwp_json()?;
        self.store.with_session_edit(&p.doc_id, |session| {
            picture_at(session.core.document(), p.section, p.para, p.control)?;
            session
                .core
                .set_picture_properties_native(p.section, p.para, p.control, &props)
                .map_err(hwp_err)?;
            let pic = picture_at(session.core.document(), p.section, p.para, p.control)?;
            Ok(json!(PictureFormat::from_picture(pic)).to_string())
        })
    }

    #[tool(
        description = "표 셀의 서식을 반환한다: background, border_left/right/top/bottom(style·width_mm·color), vertical_align, padding_*_mm(실제 쓰이는 안 여백), header. 결과는 apply_cell_format의 format 입력과 같은 모양."
    )]
    pub fn get_cell_format(
        &self,
        Parameters(p): Parameters<GetCellFormatParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let doc = session.core.document();
            let table = table_at(doc, p.section, p.para, p.control)?;
            let cell = table
                .cells
                .iter()
                .find(|c| c.row == p.row && c.col == p.col)
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        format!("({}, {}) 에서 시작하는 셀이 없다", p.row, p.col),
                        None,
                    )
                })?;
            Ok(json!(CellFormat::from_cell(doc, table, cell)).to_string())
        })
    }

    #[tool(
        description = "표의 (start_row,start_col)~(end_row,end_col) 범위 셀(끝 포함, 생략 시 한 셀)에 셀 서식을 적용한다: background(\"#RRGGBB\"/\"none\"), border(네 면 공통) 또는 border_left/right/top/bottom {style, width_mm, color}, vertical_align(top/center/bottom), padding_*_mm, header. 지정한 항목만 바뀐다. 이웃 셀과 맞닿은 테두리 면은 한글처럼 한 줄이라 이웃 셀 쪽 면도 함께 바뀐다."
    )]
    pub fn apply_cell_format(
        &self,
        Parameters(p): Parameters<ApplyCellFormatParams>,
    ) -> Result<String, ErrorData> {
        p.format.validate()?;
        let end_row = p.end_row.unwrap_or(p.start_row);
        let end_col = p.end_col.unwrap_or(p.start_col);
        if end_row < p.start_row || end_col < p.start_col {
            return Err(ErrorData::invalid_params(
                "end_row/end_col 은 start_row/start_col 이상이어야 한다",
                None,
            ));
        }
        self.store.with_session_edit(&p.doc_id, |session| {
            let doc = session.core.document();
            let table = table_at(doc, p.section, p.para, p.control)?;
            if end_row >= table.row_count || end_col >= table.col_count {
                return Err(ErrorData::invalid_params(
                    format!(
                        "범위 끝 ({end_row}, {end_col}) 이 표 크기 {}×{} 를 넘는다",
                        table.row_count, table.col_count
                    ),
                    None,
                ));
            }
            let mut targets = Vec::new();
            for (idx, cell) in table.cells.iter().enumerate() {
                let inside = (p.start_row..=end_row).contains(&cell.row)
                    && (p.start_col..=end_col).contains(&cell.col);
                if inside {
                    targets.push((idx, p.format.to_rhwp_props(doc, table, cell)?));
                }
            }
            if targets.is_empty() {
                return Err(ErrorData::invalid_params(
                    "범위 안에서 시작하는 셀이 없다 (병합 셀은 왼쪽 위 칸으로 지정)",
                    None,
                ));
            }
            for (idx, (layout, paint)) in &targets {
                for props in [layout, paint].into_iter().flatten() {
                    session
                        .core
                        .set_cell_properties_native(p.section, p.para, p.control, *idx, props)
                        .map_err(hwp_err)?;
                }
            }
            Ok(json!({"cells": targets.len()}).to_string())
        })
    }

    #[tool(
        description = "표의 (start_row,start_col)~(end_row,end_col) 직사각형 범위 셀을 하나로 병합한다 (끝 포함). 범위가 기존 병합 셀과 일부만 겹치면 거부된다."
    )]
    pub fn merge_table_cells(
        &self,
        Parameters(p): Parameters<MergeTableCellsParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .merge_table_cells_native(
                    p.section,
                    p.para,
                    p.control,
                    p.start_row,
                    p.start_col,
                    p.end_row,
                    p.end_col,
                )
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "표 셀을 나눈다. rows/cols를 생략하면 병합 셀의 병합을 해제하고, 지정하면 셀을 rows줄 × cols칸으로 나눈다 (병합 셀은 먼저 해제)."
    )]
    pub fn split_table_cell(
        &self,
        Parameters(p): Parameters<SplitTableCellParams>,
    ) -> Result<String, ErrorData> {
        let split_into = match (p.rows, p.cols) {
            (None, None) => None,
            (rows, cols) => {
                let (rows, cols) = (rows.unwrap_or(1), cols.unwrap_or(1));
                if rows == 0 || cols == 0 || (rows == 1 && cols == 1) {
                    return Err(ErrorData::invalid_params(
                        format!("rows×cols={rows}×{cols}: 1 이상이고 1×1이 아니어야 함"),
                        None,
                    ));
                }
                Some((rows, cols))
            }
        };
        self.store.with_session_edit(&p.doc_id, |session| {
            match split_into {
                None => session
                    .core
                    .split_table_cell_native(p.section, p.para, p.control, p.row, p.col),
                Some((rows, cols)) => session.core.split_table_cell_into_native(
                    p.section, p.para, p.control, p.row, p.col, rows, cols, true, true,
                ),
            }
            .map_err(hwp_err)
        })
    }

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

    // ── 개체 삽입 ──

    #[tool(
        description = "본문 문단의 개체(표/그림/도형/수식/각주/미주) 하나를 지운다. control은 get_structure objects의 control 값. 각주/미주를 지우면 뒤 번호가 당겨진다."
    )]
    pub fn delete_object(
        &self,
        Parameters(p): Parameters<ObjectAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session_edit(&p.doc_id, |session| {
            let ctrl = paragraph_at(session.core.document(), p.section, p.para, None)?
                .controls
                .get(p.control)
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        format!(
                            "문단 {}/{} 에 컨트롤 {} 이 없다",
                            p.section, p.para, p.control
                        ),
                        None,
                    )
                })?;
            let kind = ObjectKind::of(ctrl).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!(
                        "컨트롤 {} 은 지울 수 있는 개체(표/그림/도형/수식/각주/미주)가 아니다",
                        p.control
                    ),
                    None,
                )
            })?;
            let core = &mut session.core;
            let (s, para, c) = (p.section, p.para, p.control);
            match kind {
                ObjectKind::Table => core.delete_table_control_native(s, para, c),
                ObjectKind::Picture => core.delete_picture_control_native(s, para, c),
                ObjectKind::Shape => core.delete_shape_control_native(s, para, c),
                ObjectKind::Equation => core.delete_equation_control_native(s, para, c),
                ObjectKind::Footnote | ObjectKind::Endnote => {
                    core.delete_footnote_native(s, para, c)
                }
            }
            .map_err(hwp_err)
        })
    }

    #[tool(
        description = "본문 문단의 char_offset 위치에 그림 파일(PNG/JPEG/GIF/BMP/TIFF)을 넣는다. 크기는 width_mm/height_mm(하나만 주면 비율 유지, 생략 시 원본 96 DPI). inline=true(기본)면 글자처럼 취급, false면 x_mm/y_mm 위치(용지 기준)에 떠 있는 그림."
    )]
    pub fn insert_picture(
        &self,
        Parameters(p): Parameters<InsertPictureParams>,
    ) -> Result<String, ErrorData> {
        let inline = p.inline.unwrap_or(true);
        for (name, v) in [("width_mm", p.width_mm), ("height_mm", p.height_mm)] {
            if let Some(v) = v {
                check_range(name, v, 0.1, MAX_PAPER_MM)?;
            }
        }
        for (name, v) in [("x_mm", p.x_mm), ("y_mm", p.y_mm)] {
            if let Some(v) = v {
                check_range(name, v, 0.0, MAX_PAPER_MM)?;
            }
        }
        if inline && (p.x_mm.is_some() || p.y_mm.is_some()) {
            return Err(ErrorData::invalid_params(
                "x_mm/y_mm은 inline=false일 때만 쓸 수 있음",
                None,
            ));
        }
        let data = std::fs::read(&p.image_path).map_err(|e| io_err(&p.image_path, e))?;
        let reader = image::ImageReader::new(std::io::Cursor::new(&data))
            .with_guessed_format()
            .map_err(|e| io_err(&p.image_path, e))?;
        let extension = match reader.format() {
            Some(image::ImageFormat::Png) => "png",
            Some(image::ImageFormat::Jpeg) => "jpg",
            Some(image::ImageFormat::Gif) => "gif",
            Some(image::ImageFormat::Bmp) => "bmp",
            Some(image::ImageFormat::Tiff) => "tif",
            other => {
                return Err(ErrorData::invalid_params(
                    format!("지원하지 않는 그림 형식: {other:?} (PNG/JPEG/GIF/BMP/TIFF만)"),
                    None,
                ))
            }
        };
        let (px_w, px_h) = reader.into_dimensions().map_err(|e| {
            ErrorData::invalid_params(format!("그림 크기를 읽을 수 없음: {e}"), None)
        })?;
        // 96 DPI 기준 1px = 75 HWPUNIT
        let (nat_w, nat_h) = (f64::from(px_w) * 75.0, f64::from(px_h) * 75.0);
        let (width, height) = match (p.width_mm.map(mm_to_hu), p.height_mm.map(mm_to_hu)) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * nat_h / nat_w),
            (None, Some(h)) => (h * nat_w / nat_h, h),
            (None, None) => (nat_w, nat_h),
        };
        let (width, height) = (
            width.round().max(1.0) as u32,
            height.round().max(1.0) as u32,
        );

        self.store.with_session_edit(&p.doc_id, |session| {
            check_offset(
                &session.core,
                p.section,
                p.para,
                None,
                "char_offset",
                p.char_offset,
            )?;
            let section = session
                .core
                .document()
                .sections
                .get(p.section)
                .ok_or_else(|| {
                    ErrorData::invalid_params(format!("구역 {} 없음", p.section), None)
                })?;
            let (x, y) = if inline {
                (None, None)
            } else {
                let body =
                    rhwp::model::page::PageAreas::from_page_def(&section.section_def.page_def)
                        .body_area;
                (
                    Some(p.x_mm.map_or(body.left, |v| mm_to_hu(v).round() as i32)),
                    Some(p.y_mm.map_or(body.top, |v| mm_to_hu(v).round() as i32)),
                )
            };
            let out = session
                .core
                .insert_picture_native(
                    p.section,
                    p.para,
                    p.char_offset,
                    &[],
                    &data,
                    width,
                    height,
                    px_w,
                    px_h,
                    extension,
                    p.description.as_deref().unwrap_or(""),
                    x,
                    y,
                )
                .map_err(hwp_err)?;
            if inline {
                let v: Value = serde_json::from_str(&out).map_err(|e| {
                    ErrorData::internal_error(format!("insert_picture 결과 해석 실패: {e}"), None)
                })?;
                let ctrl = v["controlIdx"].as_u64().ok_or_else(|| {
                    ErrorData::internal_error(
                        format!("insert_picture 결과에 controlIdx 없음: {out}"),
                        None,
                    )
                })? as usize;
                session
                    .core
                    .set_picture_properties_native(
                        p.section,
                        p.para,
                        ctrl,
                        r#"{"treatAsChar":true}"#,
                    )
                    .map_err(hwp_err)?;
            }
            Ok(out)
        })
    }

    #[tool(
        description = "본문 문단의 char_offset 위치에 각주(kind=footnote, 기본) 또는 미주(kind=endnote)를 넣고 내용 text를 채운다. 번호는 문서 순서대로 자동으로 매겨진다."
    )]
    pub fn insert_note(
        &self,
        Parameters(p): Parameters<InsertNoteParams>,
    ) -> Result<String, ErrorData> {
        check_single_line("text", &p.text)?;
        self.store.with_session_edit(&p.doc_id, |session| {
            check_offset(
                &session.core,
                p.section,
                p.para,
                None,
                "char_offset",
                p.char_offset,
            )?;
            let out = match p.kind.unwrap_or(NoteKind::Footnote) {
                NoteKind::Footnote => {
                    session
                        .core
                        .insert_footnote_native(p.section, p.para, p.char_offset)
                }
                NoteKind::Endnote => {
                    session
                        .core
                        .insert_endnote_native(p.section, p.para, p.char_offset)
                }
            }
            .map_err(hwp_err)?;
            if !p.text.is_empty() {
                let v: Value = serde_json::from_str(&out).map_err(|e| {
                    ErrorData::internal_error(format!("insert_note 결과 해석 실패: {e}"), None)
                })?;
                let ctrl = v["controlIdx"].as_u64().ok_or_else(|| {
                    ErrorData::internal_error(
                        format!("insert_note 결과에 controlIdx 없음: {out}"),
                        None,
                    )
                })? as usize;
                // 새 주석 문단은 번호 자리표시 공백 두 칸("  ")으로 시작한다. 내용은 그 뒤에 넣는다.
                let note_para = &session.core.document().sections[p.section].paragraphs[p.para];
                let start = match note_para.controls.get(ctrl) {
                    Some(Control::Footnote(f)) => f.paragraphs.first(),
                    Some(Control::Endnote(e)) => e.paragraphs.first(),
                    _ => None,
                }
                .map(|np| np.text.chars().count())
                .ok_or_else(|| {
                    ErrorData::internal_error(
                        format!("새 주석 컨트롤 {ctrl} 을 찾을 수 없음"),
                        None,
                    )
                })?;
                session
                    .core
                    .insert_text_in_footnote_native(p.section, p.para, ctrl, 0, start, &p.text)
                    .map_err(hwp_err)?;
            }
            Ok(out)
        })
    }

    #[tool(
        description = "구역의 머리말(kind=header) 또는 꼬리말(kind=footer) 내용을 text 한 줄로 설정한다 (기존 것은 교체). text 안의 {page}, {total_pages}, {file_name}은 쪽 번호·전체 쪽 수·파일 이름 필드가 된다. pages=both(기본)/even/odd."
    )]
    pub fn set_header_footer(
        &self,
        Parameters(p): Parameters<SetHeaderFooterParams>,
    ) -> Result<String, ErrorData> {
        check_single_line("text", &p.text)?;
        let is_header = p.kind == HeaderFooterKind::Header;
        let apply = p.pages.unwrap_or_default().rhwp_code();
        self.store.with_session_edit(&p.doc_id, |session| {
            let core = &mut session.core;
            let existing: Value = serde_json::from_str(
                &core
                    .get_header_footer_native(p.section, is_header, apply)
                    .map_err(hwp_err)?,
            )
            .map_err(|e| {
                ErrorData::internal_error(format!("머리말/꼬리말 조회 해석 실패: {e}"), None)
            })?;
            if existing["exists"].as_bool() == Some(true) {
                core.delete_header_footer_native(p.section, is_header, apply)
                    .map_err(hwp_err)?;
            }
            core.create_header_footer_native(p.section, is_header, apply)
                .map_err(hwp_err)?;
            let mut offset = 0;
            for piece in hf_pieces(&p.text) {
                match piece {
                    HfPiece::Text(t) => {
                        core.insert_text_in_header_footer_native(
                            p.section, is_header, apply, 0, offset, t,
                        )
                        .map_err(hwp_err)?;
                        offset += t.chars().count();
                    }
                    HfPiece::Field(code) => {
                        core.insert_field_in_hf_native(
                            p.section, is_header, apply, 0, offset, code,
                        )
                        .map_err(hwp_err)?;
                        offset += 1;
                    }
                }
            }
            core.get_header_footer_native(p.section, is_header, apply)
                .map_err(hwp_err)
        })
    }

    #[tool(
        description = "구역의 머리말/꼬리말 내용을 반환한다. 필드는 {page}, {total_pages}, {file_name}으로 표시된다. 없으면 exists=false."
    )]
    pub fn get_header_footer(
        &self,
        Parameters(p): Parameters<HeaderFooterParams>,
    ) -> Result<String, ErrorData> {
        let is_header = p.kind == HeaderFooterKind::Header;
        let apply = p.pages.unwrap_or_default().rhwp_code();
        self.store.with_session(&p.doc_id, |session| {
            let mut v: Value = serde_json::from_str(
                &session
                    .core
                    .get_header_footer_native(p.section, is_header, apply)
                    .map_err(hwp_err)?,
            )
            .map_err(|e| {
                ErrorData::internal_error(format!("머리말/꼬리말 조회 해석 실패: {e}"), None)
            })?;
            // rhwp 의 text 는 저장본을 다시 연 문서에서 필드를 자리표 공백으로만 보여 주므로,
            // 컨트롤에서 직접 다시 그린다.
            if let (Some(pi), Some(ci)) = (v["paraIndex"].as_u64(), v["controlIndex"].as_u64()) {
                let host = &session.core.document().sections[p.section].paragraphs[pi as usize];
                let paragraphs = match host.controls.get(ci as usize) {
                    Some(Control::Header(h)) => &h.paragraphs,
                    Some(Control::Footer(f)) => &f.paragraphs,
                    _ => {
                        return Err(ErrorData::internal_error(
                            format!("머리말/꼬리말 컨트롤 ({pi},{ci}) 을 찾을 수 없음"),
                            None,
                        ))
                    }
                };
                let text: Vec<String> = paragraphs.iter().map(hf_display_text).collect();
                v["text"] = json!(text.join("\n"));
            }
            Ok(v.to_string())
        })
    }

    #[tool(description = "구역의 머리말/꼬리말을 삭제한다.")]
    pub fn delete_header_footer(
        &self,
        Parameters(p): Parameters<HeaderFooterParams>,
    ) -> Result<String, ErrorData> {
        let is_header = p.kind == HeaderFooterKind::Header;
        let apply = p.pages.unwrap_or_default().rhwp_code();
        self.store.with_session_edit(&p.doc_id, |session| {
            session
                .core
                .delete_header_footer_native(p.section, is_header, apply)
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
        description = "문서의 모든 양식 개체(버튼/체크박스/라디오/콤보박스/에디트)를 목록으로 반환한다: 위치(section/para/control, 표 셀 안이면 cell), 종류, 이름, 값, 캡션, 텍스트. 폼을 채우기 전 좌표를 파악하는 시작점."
    )]
    pub fn list_forms(&self, Parameters(p): Parameters<DocIdParams>) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let mut forms = Vec::new();
            for_each_para(session.core.document(), |loc, para| {
                for (ci, ctrl) in para.controls.iter().enumerate() {
                    if let Control::Form(f) = ctrl {
                        let mut entry = loc.json();
                        entry.insert("control".into(), json!(ci));
                        entry.extend(
                            form_value_json(f)
                                .as_object()
                                .expect("form_value_json 은 객체")
                                .clone(),
                        );
                        forms.push(Value::Object(entry));
                    }
                }
            });
            Ok(json!({ "forms": forms }).to_string())
        })
    }

    #[tool(
        description = "양식 개체의 현재 값을 조회한다 (종류/이름/값/텍스트/캡션/활성). 위치(section/para/control, 표 셀 안이면 cell)는 list_forms로 파악한다."
    )]
    pub fn get_form_value(
        &self,
        Parameters(p): Parameters<FormAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let f = form_at(
                session.core.document(),
                p.section,
                p.para,
                p.cell.as_ref(),
                p.control,
            )?;
            Ok(form_value_json(f).to_string())
        })
    }

    #[tool(
        description = "양식 개체의 상세 정보를 반환한다: 값/텍스트/캡션 + 크기·색·속성(properties), 콤보박스는 항목 목록 포함. 표 셀 안의 콤보박스는 항목 목록을 읽을 수 없어 오류."
    )]
    pub fn get_form_info(
        &self,
        Parameters(p): Parameters<FormAtParams>,
    ) -> Result<String, ErrorData> {
        self.store.with_session(&p.doc_id, |session| {
            let f = form_at(
                session.core.document(),
                p.section,
                p.para,
                p.cell.as_ref(),
                p.control,
            )?;
            if p.cell.is_none() {
                return session
                    .core
                    .get_form_object_info_native(p.section, p.para, p.control)
                    .map_err(hwp_err);
            }
            // 콤보박스 항목은 rhwp 가 문서 스크립트에서 private 함수로만 뽑는다
            // (get_form_object_info_native 는 본문 좌표만 받는다).
            if f.form_type == FormType::ComboBox {
                return Err(ErrorData::invalid_params(
                    "표 셀 안 콤보박스의 항목 목록은 rhwp가 노출하지 않는다 (값은 get_form_value)",
                    None,
                ));
            }
            let mut info = form_value_json(f);
            let properties: std::collections::BTreeMap<_, _> = f.properties.iter().collect();
            info.as_object_mut()
                .expect("form_value_json 은 객체")
                .extend([
                    ("width".to_string(), json!(f.width)),
                    ("height".to_string(), json!(f.height)),
                    ("foreColor".to_string(), json!(f.fore_color)),
                    ("backColor".to_string(), json!(f.back_color)),
                    ("properties".to_string(), json!(properties)),
                    ("items".to_string(), json!([])),
                ]);
            Ok(info.to_string())
        })
    }

    #[tool(
        description = "양식 개체의 값을 설정한다. value(체크 상태 0/1), text(콤보 선택값·에디트 입력값), caption(버튼 라벨) 중 최소 하나를 지정한다. 위치(section/para/control, 표 셀 안이면 cell)는 list_forms로 파악한다."
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
            // 네이티브는 폼이 아니어도 Ok({"ok":false})를 돌려주므로 위치를 먼저 확인한다.
            form_at(
                session.core.document(),
                p.section,
                p.para,
                p.cell.as_ref(),
                p.control,
            )?;
            match p.cell.as_ref() {
                Some(c) => session.core.set_form_value_in_cell_native(
                    p.section,
                    p.para,
                    c.control,
                    c.cell,
                    c.cell_para(),
                    p.control,
                    &value_json,
                ),
                None => {
                    session
                        .core
                        .set_form_value_native(p.section, p.para, p.control, &value_json)
                }
            }
            .map_err(hwp_err)
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
                 표는 create_table/merge_table_cells/split_table_cell, 그림은 insert_picture, \
                 쪽 나누기는 insert_break, 각주/미주는 insert_note, 머리말/꼬리말은 set_header_footer로 넣는다. \
                 편집 결과는 render_page로 SVG/PDF를 그려 확인하고, 잘못된 편집은 undo로 되돌린다. \
                 모든 인덱스는 0-기반, 문자 위치는 문단 텍스트의 char 단위다. \
                 편집은 메모리에서만 일어나며 save_document 전에는 파일이 바뀌지 않는다. \
                 편집 도구가 오류를 내면 문서는 호출 전 그대로다.",
            )
    }
}
