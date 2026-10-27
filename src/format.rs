//! 글자/문단 서식 — MCP 입출력 타입과 rhwp 속성 JSON·IR 사이의 변환.
//!
//! 적용(`apply_*`)은 rhwp `apply_*_format*_native` 의 속성 JSON 으로, 조회(`get_*`)는
//! IR(`CharShape`/`ParaShape`)에서 직접 읽어 같은 타입으로 돌려준다. 조회 결과를 그대로
//! 적용 입력으로 쓸 수 있게 입출력이 한 타입·한 단위(pt, %, "#RRGGBB")를 공유한다.
//!
//! rhwp 의 속성 파서는 공백 없는 `"key":` 문자열 검색이라, 범위를 벗어난 정수는
//! `as u8` 로 잘리고 형식이 틀린 값(색 이름, 소수 등)은 오류 없이 무시된다. 그래서
//! 값 검증은 문서를 건드리기 전에 여기서 끝내고, 직렬화는 compact JSON 정수로 한다.

use rhwp::model::control::Control;
use rhwp::model::document::Document;
use rhwp::model::paragraph::Paragraph;
use rhwp::model::style::{
    Alignment as RAlignment, CharShape, LineSpacingType as RLineSpacingType, ParaShape,
    UnderlineType as RUnderlineType,
};
use rmcp::ErrorData;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// 문단 여백·간격·고정 줄 간격의 IR 값은 HWPUNIT 의 2배 스케일이다 (1pt = 200).
/// rhwp `style_resolver` 가 `/ 2` 로 환산해 그리는 것과 같은 단위.
const PARA_UNITS_PER_PT: f64 = 200.0;
/// 글자 크기 단위는 1/100 pt.
const CHAR_SIZE_UNITS_PER_PT: f64 = 100.0;

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CellTarget {
    /// 표가 속한 문단의 컨트롤 인덱스 (get_structure의 table control 값)
    pub control: usize,
    /// 셀 평면 인덱스 (get_table의 cell_idx)
    pub cell: usize,
    /// 셀 내 문단 인덱스 (생략 시 0)
    pub cell_para: Option<usize>,
}

impl CellTarget {
    pub fn cell_para(&self) -> usize {
        self.cell_para.unwrap_or(0)
    }
}

/// 본문 문단, 또는 `cell` 이 있으면 (section, para) 문단의 표 셀 안 문단을 찾는다.
pub fn paragraph_at<'a>(
    doc: &'a Document,
    section: usize,
    para: usize,
    cell: Option<&CellTarget>,
) -> Result<&'a Paragraph, ErrorData> {
    let not_found = |what: String| ErrorData::invalid_params(what, None);
    let p = doc
        .sections
        .get(section)
        .ok_or_else(|| not_found(format!("구역 {section} 이 없다")))?
        .paragraphs
        .get(para)
        .ok_or_else(|| not_found(format!("문단 {section}/{para} 이 없다")))?;
    let Some(c) = cell else {
        return Ok(p);
    };
    let Some(Control::Table(table)) = p.controls.get(c.control) else {
        return Err(not_found(format!(
            "문단 {section}/{para} 의 컨트롤 {} 은 표가 아니다",
            c.control
        )));
    };
    table
        .cells
        .get(c.cell)
        .ok_or_else(|| not_found(format!("셀 {} 이 없다", c.cell)))?
        .paragraphs
        .get(c.cell_para())
        .ok_or_else(|| not_found(format!("셀 {} 의 문단 {} 이 없다", c.cell, c.cell_para())))
}

fn invalid(msg: String) -> ErrorData {
    ErrorData::invalid_params(msg, None)
}

/// "#RRGGBB" 만 받는다.
fn check_color(key: &str, value: &str) -> Result<(), ErrorData> {
    let ok = value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "{key} 는 \"#RRGGBB\" 형식이어야 한다: {value:?}"
        )))
    }
}

fn check_range<T: PartialOrd + std::fmt::Display>(
    key: &str,
    value: T,
    min: T,
    max: T,
) -> Result<(), ErrorData> {
    // NaN 은 어느 비교도 참이 아니므로 `!(min <= v <= max)` 로 써야 함께 걸러진다.
    if !(value >= min && value <= max) {
        return Err(invalid(format!(
            "{key} 는 {min}~{max} 범위여야 한다: {value}"
        )));
    }
    Ok(())
}

/// HWP ColorRef(0x00BBGGRR) → "#rrggbb".
fn color_css(c: u32) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        c & 0xFF,
        (c >> 8) & 0xFF,
        (c >> 16) & 0xFF
    )
}

/// 속성 JSON 맵. 비면 적용할 것이 없다는 뜻이다.
struct Props(serde_json::Map<String, serde_json::Value>);

impl Props {
    fn new() -> Self {
        Self(serde_json::Map::new())
    }

    fn put<T: Serialize>(&mut self, key: &str, v: Option<T>) {
        if let Some(v) = v {
            self.0.insert(key.to_string(), json!(v));
        }
    }

    fn finish(self, what: &str) -> Result<String, ErrorData> {
        if self.0.is_empty() {
            return Err(invalid(format!("{what} 에 적용할 서식이 하나도 없다")));
        }
        Ok(serde_json::Value::Object(self.0).to_string())
    }
}

// ─── 글자 서식 ────────────────────────────────────────────────

#[derive(Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub enum UnderlineType {
    /// 밑줄 없음
    None,
    /// 글자 아래 밑줄
    Bottom,
    /// 글자 위 윗줄
    Top,
}

/// 글자 서식. 적용할 때는 지정한 항목만 바뀌고 나머지는 원래 서식을 유지한다.
/// 색상은 모두 "#RRGGBB" 형식이다.
#[derive(Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct CharFormat {
    /// 진하게
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    /// 기울임
    #[serde(skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// 밑줄 켜기/끄기 (켜면 글자 아래 밑줄). 위치를 고르려면 underline_type을 쓴다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    /// 밑줄 위치 (underline보다 우선)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline_type: Option<UnderlineType>,
    /// 밑줄 모양 (0~10, HWP 선 종류: 0=실선, 1=긴 점선, 2=점선 …)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline_shape: Option<u8>,
    /// 밑줄 색
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline_color: Option<String>,
    /// 취소선
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<bool>,
    /// 취소선 모양 (0~10, HWP 선 종류)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strike_shape: Option<u8>,
    /// 취소선 색
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strike_color: Option<String>,
    /// 글자 크기 (pt, 예: 10.5)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size_pt: Option<f64>,
    /// 글꼴 이름 (예: "맑은 고딕"). 적용 시 문서에 없으면 새로 등록하고 모든 언어 글꼴에
    /// 쓴다. 조회 결과는 한글 글꼴이다 (언어별 값은 per_language).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_name: Option<String>,
    /// 글자 색
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
    /// 음영(형광펜) 색. 조회 결과에 없으면 음영 없음이다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shade_color: Option<String>,
    /// 외곽선 종류 (0=없음, 1~6 HWP 외곽선 종류)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_type: Option<u8>,
    /// 그림자 종류 (0=없음, 1=비연속, 2=연속)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_type: Option<u8>,
    /// 그림자 색
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_color: Option<String>,
    /// 그림자 X 간격 (-100~100%)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_offset_x: Option<i8>,
    /// 그림자 Y 간격 (-100~100%)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_offset_y: Option<i8>,
    /// 위첨자 (subscript와 동시에 true일 수 없음)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superscript: Option<bool>,
    /// 아래첨자 (superscript와 동시에 true일 수 없음)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscript: Option<bool>,
    /// 양각
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emboss: Option<bool>,
    /// 음각
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engrave: Option<bool>,
    /// 강조점 종류 (0=없음, 1~6)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emphasis_dot: Option<u8>,
    /// 커닝
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kerning: Option<bool>,
    /// 장평 (50~200%). 적용 시 모든 언어에 쓴다. 조회 결과는 한글 값이다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio_percent: Option<u8>,
    /// 자간 (-50~50%). 적용 시 모든 언어에 쓴다. 조회 결과는 한글 값이다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing_percent: Option<i8>,
    /// 상대 크기 (10~250%). 적용 시 모든 언어에 쓴다. 조회 결과는 한글 값이다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_size_percent: Option<u8>,
    /// 글자 위치 (-100~100%). 적용 시 모든 언어에 쓴다. 조회 결과는 한글 값이다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub char_offset_percent: Option<i8>,
}

/// 언어별(한글/영문/한자/일어/기타/기호/사용자) 7칸 값. 조회 전용.
#[derive(Serialize)]
pub struct PerLanguage {
    pub font_names: Vec<String>,
    pub ratios: [u8; 7],
    pub spacings: [i8; 7],
    pub relative_sizes: [u8; 7],
    pub char_offsets: [i8; 7],
}

impl CharFormat {
    pub fn validate(&self) -> Result<(), ErrorData> {
        for (key, color) in [
            ("text_color", &self.text_color),
            ("shade_color", &self.shade_color),
            ("underline_color", &self.underline_color),
            ("strike_color", &self.strike_color),
            ("shadow_color", &self.shadow_color),
        ] {
            if let Some(c) = color {
                check_color(key, c)?;
            }
        }
        if let Some(pt) = self.font_size_pt {
            check_range("font_size_pt", pt, 1.0, 4096.0)?;
        }
        if let Some(name) = &self.font_name {
            if name.trim().is_empty() {
                return Err(invalid("font_name 이 비어 있다".to_string()));
            }
        }
        if let Some(v) = self.underline_shape {
            check_range("underline_shape", v, 0, 10)?;
        }
        if let Some(v) = self.strike_shape {
            check_range("strike_shape", v, 0, 10)?;
        }
        if let Some(v) = self.outline_type {
            check_range("outline_type", v, 0, 6)?;
        }
        if let Some(v) = self.shadow_type {
            check_range("shadow_type", v, 0, 2)?;
        }
        if let Some(v) = self.shadow_offset_x {
            check_range("shadow_offset_x", v, -100, 100)?;
        }
        if let Some(v) = self.shadow_offset_y {
            check_range("shadow_offset_y", v, -100, 100)?;
        }
        if let Some(v) = self.emphasis_dot {
            check_range("emphasis_dot", v, 0, 6)?;
        }
        if let Some(v) = self.ratio_percent {
            check_range("ratio_percent", v, 50, 200)?;
        }
        if let Some(v) = self.spacing_percent {
            check_range("spacing_percent", v, -50, 50)?;
        }
        if let Some(v) = self.relative_size_percent {
            check_range("relative_size_percent", v, 10, 250)?;
        }
        if let Some(v) = self.char_offset_percent {
            check_range("char_offset_percent", v, -100, 100)?;
        }
        if self.superscript == Some(true) && self.subscript == Some(true) {
            return Err(invalid(
                "superscript 와 subscript 는 동시에 true일 수 없다".to_string(),
            ));
        }
        Ok(())
    }

    /// rhwp `apply_char_format*_native` 가 받는 속성 JSON. `font_id` 는 `font_name` 을
    /// 문서에 등록해 얻은 ID 다.
    pub fn to_rhwp_props(&self, font_id: Option<u16>) -> Result<String, ErrorData> {
        let mut p = Props::new();
        p.put("bold", self.bold);
        p.put("italic", self.italic);
        p.put("underline", self.underline);
        p.put(
            "underlineType",
            self.underline_type.map(|t| match t {
                UnderlineType::None => "None",
                UnderlineType::Bottom => "Bottom",
                UnderlineType::Top => "Top",
            }),
        );
        p.put("underlineShape", self.underline_shape);
        p.put("underlineColor", self.underline_color.as_deref());
        p.put("strikethrough", self.strikethrough);
        p.put("strikeShape", self.strike_shape);
        p.put("strikeColor", self.strike_color.as_deref());
        p.put(
            "fontSize",
            self.font_size_pt
                .map(|pt| (pt * CHAR_SIZE_UNITS_PER_PT).round() as i32),
        );
        p.put("fontId", font_id);
        p.put("textColor", self.text_color.as_deref());
        p.put("shadeColor", self.shade_color.as_deref());
        p.put("outlineType", self.outline_type);
        p.put("shadowType", self.shadow_type);
        p.put("shadowColor", self.shadow_color.as_deref());
        p.put("shadowOffsetX", self.shadow_offset_x);
        p.put("shadowOffsetY", self.shadow_offset_y);
        p.put("superscript", self.superscript);
        p.put("subscript", self.subscript);
        p.put("emboss", self.emboss);
        p.put("engrave", self.engrave);
        p.put("emphasisDot", self.emphasis_dot);
        p.put("kerning", self.kerning);
        p.put("ratios", self.ratio_percent.map(|v| [v; 7]));
        p.put("spacings", self.spacing_percent.map(|v| [v; 7]));
        p.put("relativeSizes", self.relative_size_percent.map(|v| [v; 7]));
        p.put("charOffsets", self.char_offset_percent.map(|v| [v; 7]));
        p.finish("format")
    }

    /// IR 글자 모양을 같은 타입으로 읽는다 (모든 항목이 채워진다).
    pub fn from_shape(doc: &Document, cs: &CharShape) -> (Self, PerLanguage) {
        let font_names: Vec<String> = (0..7)
            .map(|lang| {
                doc.doc_info
                    .font_faces
                    .get(lang)
                    .and_then(|faces| faces.get(cs.font_ids[lang] as usize))
                    .map(|f| f.name.clone())
                    .unwrap_or_default()
            })
            .collect();
        // 상위 바이트가 0 이 아니면 '색 없음' (한컴 0xFFFFFFFF).
        let shade = (cs.shade_color >> 24 == 0).then(|| color_css(cs.shade_color));
        let format = Self {
            bold: Some(cs.bold),
            italic: Some(cs.italic),
            underline: Some(!matches!(cs.underline_type, RUnderlineType::None)),
            underline_type: Some(match cs.underline_type {
                RUnderlineType::None => UnderlineType::None,
                RUnderlineType::Bottom => UnderlineType::Bottom,
                RUnderlineType::Top => UnderlineType::Top,
            }),
            underline_shape: Some(cs.underline_shape),
            underline_color: Some(color_css(cs.underline_color)),
            strikethrough: Some(cs.strikethrough),
            strike_shape: Some(cs.strike_shape),
            strike_color: Some(color_css(cs.strike_color)),
            font_size_pt: Some(cs.base_size as f64 / CHAR_SIZE_UNITS_PER_PT),
            font_name: Some(font_names[0].clone()),
            text_color: Some(color_css(cs.text_color)),
            shade_color: shade,
            outline_type: Some(cs.outline_type),
            shadow_type: Some(cs.shadow_type),
            shadow_color: Some(color_css(cs.shadow_color)),
            shadow_offset_x: Some(cs.shadow_offset_x),
            shadow_offset_y: Some(cs.shadow_offset_y),
            superscript: Some(cs.superscript),
            subscript: Some(cs.subscript),
            emboss: Some(cs.emboss),
            engrave: Some(cs.engrave),
            emphasis_dot: Some(cs.emphasis_dot),
            kerning: Some(cs.kerning),
            ratio_percent: Some(cs.ratios[0]),
            spacing_percent: Some(cs.spacings[0]),
            relative_size_percent: Some(cs.relative_sizes[0]),
            char_offset_percent: Some(cs.char_offsets[0]),
        };
        let per_language = PerLanguage {
            font_names,
            ratios: cs.ratios,
            spacings: cs.spacings,
            relative_sizes: cs.relative_sizes,
            char_offsets: cs.char_offsets,
        };
        (format, per_language)
    }
}

/// 문단의 [start, end) 를 글자 모양이 같은 구간들로 나눈다 → (start, end, char_shape_id).
pub fn char_shape_runs(para: &Paragraph, start: usize, end: usize) -> Vec<(usize, usize, u32)> {
    let mut runs: Vec<(usize, usize, u32)> = Vec::new();
    for i in start..end {
        let id = para.char_shape_id_at(i).unwrap_or(0);
        match runs.last_mut() {
            Some((_, e, last)) if *last == id => *e = i + 1,
            _ => runs.push((i, i + 1, id)),
        }
    }
    runs
}

// ─── 문단 서식 ────────────────────────────────────────────────

#[derive(Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Alignment {
    /// 양쪽 정렬
    Justify,
    /// 왼쪽 정렬
    Left,
    /// 오른쪽 정렬
    Right,
    /// 가운데 정렬
    Center,
    /// 배분 정렬
    Distribute,
    /// 나눔 정렬
    Split,
}

#[derive(Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub enum LineSpacingKind {
    /// 글자에 따라 (%, value는 퍼센트)
    Percent,
    /// 고정 값 (value는 pt)
    Fixed,
    /// 여백만 지정 (value는 pt)
    SpaceOnly,
    /// 최소 (value는 pt)
    Minimum,
}

#[derive(Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LineSpacing {
    /// 줄 간격 종류
    pub kind: LineSpacingKind,
    /// Percent면 퍼센트(예: 160), 그 외는 pt
    pub value: f64,
}

#[derive(Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub enum KoreanBreakUnit {
    /// 어절 단위 줄 나눔
    Word,
    /// 글자 단위 줄 나눔
    Char,
}

#[derive(Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub enum EnglishBreakUnit {
    /// 단어 단위
    Word,
    /// 하이픈
    Hyphen,
    /// 글자 단위
    Char,
}

/// 문단 서식. 적용할 때는 지정한 항목만 바뀐다. 길이는 모두 pt.
#[derive(Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ParaFormat {
    /// 정렬
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alignment: Option<Alignment>,
    /// 줄 간격
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_spacing: Option<LineSpacing>,
    /// 왼쪽 여백 (pt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_left_pt: Option<f64>,
    /// 오른쪽 여백 (pt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_right_pt: Option<f64>,
    /// 첫 줄 들여쓰기 (pt, 양수=들여쓰기, 음수=내어쓰기)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_pt: Option<f64>,
    /// 문단 위 간격 (pt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing_before_pt: Option<f64>,
    /// 문단 아래 간격 (pt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing_after_pt: Option<f64>,
    /// 다음 문단과 함께
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_with_next: Option<bool>,
    /// 문단 보호 (문단을 나누지 않음)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_lines: Option<bool>,
    /// 문단 앞에서 항상 쪽 나눔
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_break_before: Option<bool>,
    /// 외톨이줄 보호
    #[serde(skip_serializing_if = "Option::is_none")]
    pub widow_orphan: Option<bool>,
    /// 한글 줄 나눔 단위
    #[serde(skip_serializing_if = "Option::is_none")]
    pub korean_break_unit: Option<KoreanBreakUnit>,
    /// 영어 줄 나눔 단위
    #[serde(skip_serializing_if = "Option::is_none")]
    pub english_break_unit: Option<EnglishBreakUnit>,
}

/// 길이 상한. 쪽 크기를 넘는 값은 의미가 없고, 200배 해도 i32 안에 든다.
const MAX_LEN_PT: f64 = 2000.0;

impl ParaFormat {
    pub fn validate(&self) -> Result<(), ErrorData> {
        if let Some(ls) = self.line_spacing {
            match ls.kind {
                LineSpacingKind::Percent => {
                    check_range("line_spacing.value(%)", ls.value, 1.0, 500.0)?
                }
                _ => check_range("line_spacing.value(pt)", ls.value, 0.0, MAX_LEN_PT)?,
            }
        }
        for (key, v) in [
            ("margin_left_pt", self.margin_left_pt),
            ("margin_right_pt", self.margin_right_pt),
            ("spacing_before_pt", self.spacing_before_pt),
            ("spacing_after_pt", self.spacing_after_pt),
        ] {
            if let Some(v) = v {
                check_range(key, v, 0.0, MAX_LEN_PT)?;
            }
        }
        if let Some(v) = self.indent_pt {
            check_range("indent_pt", v, -MAX_LEN_PT, MAX_LEN_PT)?;
        }
        Ok(())
    }

    /// rhwp `apply_para_format*_native` 가 받는 속성 JSON.
    pub fn to_rhwp_props(&self) -> Result<String, ErrorData> {
        let units = |pt: f64| (pt * PARA_UNITS_PER_PT).round() as i32;
        let len = |pt: Option<f64>| pt.map(units);
        let mut p = Props::new();
        p.put(
            "alignment",
            self.alignment.map(|a| match a {
                Alignment::Justify => "justify",
                Alignment::Left => "left",
                Alignment::Right => "right",
                Alignment::Center => "center",
                Alignment::Distribute => "distribute",
                Alignment::Split => "split",
            }),
        );
        if let Some(ls) = self.line_spacing {
            let (kind, value) = match ls.kind {
                LineSpacingKind::Percent => ("Percent", ls.value.round() as i32),
                LineSpacingKind::Fixed => ("Fixed", units(ls.value)),
                LineSpacingKind::SpaceOnly => ("SpaceOnly", units(ls.value)),
                LineSpacingKind::Minimum => ("Minimum", units(ls.value)),
            };
            p.put("lineSpacingType", Some(kind));
            p.put("lineSpacing", Some(value));
        }
        p.put("marginLeft", len(self.margin_left_pt));
        p.put("marginRight", len(self.margin_right_pt));
        p.put("indent", len(self.indent_pt));
        p.put("spacingBefore", len(self.spacing_before_pt));
        p.put("spacingAfter", len(self.spacing_after_pt));
        p.put("keepWithNext", self.keep_with_next);
        p.put("keepLines", self.keep_lines);
        p.put("pageBreakBefore", self.page_break_before);
        p.put("widowOrphan", self.widow_orphan);
        p.put(
            "koreanBreakUnit",
            self.korean_break_unit.map(|u| match u {
                KoreanBreakUnit::Word => 0,
                KoreanBreakUnit::Char => 1,
            }),
        );
        p.put(
            "englishBreakUnit",
            self.english_break_unit.map(|u| match u {
                EnglishBreakUnit::Word => 0,
                EnglishBreakUnit::Hyphen => 1,
                EnglishBreakUnit::Char => 2,
            }),
        );
        p.finish("format")
    }

    /// IR 문단 모양을 같은 타입으로 읽는다. 비트 위치는 rhwp `ParaShapeMods::apply_to` 와 같다.
    pub fn from_shape(ps: &ParaShape) -> Self {
        let pt = |v: i32| v as f64 / PARA_UNITS_PER_PT;
        let bit = |n: u32| (ps.attr1 >> n) & 1 != 0;
        let line_spacing = match ps.line_spacing_type {
            RLineSpacingType::Percent => LineSpacing {
                kind: LineSpacingKind::Percent,
                value: ps.line_spacing as f64,
            },
            RLineSpacingType::Fixed => LineSpacing {
                kind: LineSpacingKind::Fixed,
                value: pt(ps.line_spacing),
            },
            RLineSpacingType::SpaceOnly => LineSpacing {
                kind: LineSpacingKind::SpaceOnly,
                value: pt(ps.line_spacing),
            },
            RLineSpacingType::Minimum => LineSpacing {
                kind: LineSpacingKind::Minimum,
                value: pt(ps.line_spacing),
            },
        };
        Self {
            alignment: Some(match ps.alignment {
                RAlignment::Justify => Alignment::Justify,
                RAlignment::Left => Alignment::Left,
                RAlignment::Right => Alignment::Right,
                RAlignment::Center => Alignment::Center,
                RAlignment::Distribute => Alignment::Distribute,
                RAlignment::Split => Alignment::Split,
            }),
            line_spacing: Some(line_spacing),
            margin_left_pt: Some(pt(ps.margin_left)),
            margin_right_pt: Some(pt(ps.margin_right)),
            indent_pt: Some(pt(ps.indent)),
            spacing_before_pt: Some(pt(ps.spacing_before)),
            spacing_after_pt: Some(pt(ps.spacing_after)),
            widow_orphan: Some(bit(16)),
            keep_with_next: Some(bit(17)),
            keep_lines: Some(bit(18)),
            page_break_before: Some(bit(19)),
            korean_break_unit: Some(if bit(7) {
                KoreanBreakUnit::Char
            } else {
                KoreanBreakUnit::Word
            }),
            english_break_unit: Some(match (ps.attr1 >> 5) & 0x03 {
                0 => EnglishBreakUnit::Word,
                1 => EnglishBreakUnit::Hyphen,
                _ => EnglishBreakUnit::Char,
            }),
        }
    }
}
