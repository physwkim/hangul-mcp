//! 표 셀 서식(배경·테두리·세로 정렬·안 여백) — MCP 입출력 타입과 rhwp 셀 속성 JSON·IR 사이의 변환.
//!
//! 조회(`get_cell_format`)는 IR(`Cell`·`BorderFill`)에서 읽고, 적용(`apply_cell_format`)은
//! `set_cell_properties_native` 의 속성 JSON 으로 보낸다. 입출력이 한 타입·한 단위(mm,
//! "#RRGGBB")를 써서 조회 결과를 그대로 다른 셀에 적용할 수 있다.
//!
//! rhwp 는 테두리 굵기를 16단계 index 로, 모르는 선 종류 번호를 선 없음으로 조용히 바꾸므로
//! 값 검증은 여기서 끝낸다. 한 테두리 면의 일부 항목만 주면 나머지는 그 면의 현재 값을 쓴다.

use rhwp::model::document::Document;
use rhwp::model::style::{BorderFill, BorderLine, BorderLineType, FillType, BORDER_WIDTHS};
use rhwp::model::table::{Cell, Table, VerticalAlign as RVerticalAlign};
use rmcp::ErrorData;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::format::{check_color, check_range, color_css};

/// 셀 안 여백 한 변의 상한 (mm). HWPUNIT16(i16) 이 담을 수 있는 범위 안.
const MAX_PADDING_MM: f64 = 100.0;

fn mm_to_hu16(mm: f64) -> i16 {
    (mm * 7200.0 / 25.4).round() as i16
}

fn hu_to_mm(hu: i16) -> f64 {
    (hu as f64 * 25.4 / 7200.0 * 100.0).round() / 100.0
}

/// 테두리 선 종류 (한글 선 모양 목록 순서).
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum BorderStyle {
    None,
    Solid,
    Dash,
    Dot,
    DashDot,
    DashDotDot,
    LongDash,
    Circle,
    Double,
    ThinThickDouble,
    ThickThinDouble,
    ThinThickThinTriple,
    Wave,
    DoubleWave,
    Thick3d,
    Thick3dReverse,
    Thin3d,
    Thin3dReverse,
}

impl BorderStyle {
    /// rhwp `u8_to_border_line_type` 의 번호.
    fn code(self) -> u8 {
        self as u8
    }

    fn from_rhwp(t: BorderLineType) -> Self {
        match t {
            BorderLineType::None => Self::None,
            BorderLineType::Solid => Self::Solid,
            BorderLineType::Dash => Self::Dash,
            BorderLineType::Dot => Self::Dot,
            BorderLineType::DashDot => Self::DashDot,
            BorderLineType::DashDotDot => Self::DashDotDot,
            BorderLineType::LongDash => Self::LongDash,
            BorderLineType::Circle => Self::Circle,
            BorderLineType::Double => Self::Double,
            BorderLineType::ThinThickDouble => Self::ThinThickDouble,
            BorderLineType::ThickThinDouble => Self::ThickThinDouble,
            BorderLineType::ThinThickThinTriple => Self::ThinThickThinTriple,
            BorderLineType::Wave => Self::Wave,
            BorderLineType::DoubleWave => Self::DoubleWave,
            BorderLineType::Thick3D => Self::Thick3d,
            BorderLineType::Thick3DReverse => Self::Thick3dReverse,
            BorderLineType::Thin3D => Self::Thin3d,
            BorderLineType::Thin3DReverse => Self::Thin3dReverse,
        }
    }
}

/// 테두리 한 면. 적용 시 생략한 항목은 그 면의 현재 값을 쓴다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct BorderSide {
    /// 선 종류
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<BorderStyle>,
    /// 굵기 (mm): 0.1, 0.12, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.6, 0.7, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0 중 하나
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_mm: Option<f64>,
    /// 색 "#RRGGBB"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl BorderSide {
    fn from_rhwp(b: &BorderLine) -> Self {
        Self {
            style: Some(BorderStyle::from_rhwp(b.line_type)),
            width_mm: Some(
                BORDER_WIDTHS
                    .get(b.width as usize)
                    .map(|w| w.0)
                    .unwrap_or(BORDER_WIDTHS[0].0),
            ),
            color: Some(color_css(b.color)),
        }
    }

    fn validate(&self, key: &str) -> Result<(), ErrorData> {
        if let Some(w) = self.width_mm {
            width_index(key, w)?;
        }
        if let Some(c) = &self.color {
            check_color(&format!("{key}.color"), c)?;
        }
        Ok(())
    }

    /// 현재 면 `cur` 에 덮어쓴 rhwp 테두리 JSON (`{type, width, color}`).
    fn merged_json(&self, key: &str, cur: &BorderLine) -> Result<Value, ErrorData> {
        let style = self
            .style
            .unwrap_or_else(|| BorderStyle::from_rhwp(cur.line_type));
        let width = match self.width_mm {
            Some(w) => width_index(key, w)?,
            None => cur.width,
        };
        let color = self.color.clone().unwrap_or_else(|| color_css(cur.color));
        Ok(json!({"type": style.code(), "width": width, "color": color}))
    }
}

fn width_index(key: &str, mm: f64) -> Result<u8, ErrorData> {
    BORDER_WIDTHS
        .iter()
        .position(|w| (w.0 - mm).abs() < 1e-9)
        .map(|i| i as u8)
        .ok_or_else(|| {
            let allowed: Vec<_> = BORDER_WIDTHS.iter().map(|w| w.1).collect();
            ErrorData::invalid_params(
                format!(
                    "{key}.width_mm 는 {} 중 하나여야 한다: {mm}",
                    allowed.join(", ")
                ),
                None,
            )
        })
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum VerticalAlign {
    Top,
    Center,
    Bottom,
}

/// 셀 서식. 적용 시 지정한 항목만 바뀐다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct CellFormat {
    /// 배경색 "#RRGGBB", 또는 "none" (채우기 없음)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// 네 면 공통 테두리 (적용 전용. border_left 등 면별 지정이 이보다 우선)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<BorderSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_left: Option<BorderSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_right: Option<BorderSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_top: Option<BorderSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_bottom: Option<BorderSide>,
    /// 세로 정렬
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical_align: Option<VerticalAlign>,
    /// 안 여백 (mm). 지정하면 이 셀은 표 기본 안 여백 대신 자기 여백을 쓴다
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding_left_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding_right_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding_top_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding_bottom_mm: Option<f64>,
    /// 제목 셀 (표가 쪽을 넘을 때 반복되는 머리 행)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
}

/// 셀에 실제로 쓰이는 안 여백: 셀 자기 여백을 켰으면 그것, 아니면 표 기본 안 여백.
fn effective_padding<'a>(table: &'a Table, cell: &'a Cell) -> &'a rhwp::model::Padding {
    if cell.apply_inner_margin {
        &cell.padding
    } else {
        &table.padding
    }
}

fn border_fill(doc: &Document, id: u16) -> Option<&BorderFill> {
    usize::from(id)
        .checked_sub(1)
        .and_then(|i| doc.doc_info.border_fills.get(i))
}

impl CellFormat {
    /// 셀의 현재 서식. 안 여백은 실제로 쓰이는 값(셀 자체 또는 표 기본)이다.
    pub fn from_cell(doc: &Document, table: &Table, cell: &Cell) -> Self {
        let bf = border_fill(doc, cell.border_fill_id);
        let side = |i: usize| {
            Some(BorderSide::from_rhwp(
                &bf.map(|b| b.borders[i]).unwrap_or_default(),
            ))
        };
        let background = Some(match bf.map(|b| (&b.fill.fill_type, &b.fill.solid)) {
            Some((FillType::Solid, Some(solid))) => color_css(solid.background_color),
            _ => "none".to_string(),
        });
        let padding = effective_padding(table, cell);
        Self {
            background,
            border: None,
            border_left: side(0),
            border_right: side(1),
            border_top: side(2),
            border_bottom: side(3),
            vertical_align: Some(match cell.vertical_align {
                RVerticalAlign::Top => VerticalAlign::Top,
                RVerticalAlign::Center => VerticalAlign::Center,
                RVerticalAlign::Bottom => VerticalAlign::Bottom,
            }),
            padding_left_mm: Some(hu_to_mm(padding.left)),
            padding_right_mm: Some(hu_to_mm(padding.right)),
            padding_top_mm: Some(hu_to_mm(padding.top)),
            padding_bottom_mm: Some(hu_to_mm(padding.bottom)),
            header: Some(cell.is_header),
        }
    }

    pub fn validate(&self) -> Result<(), ErrorData> {
        if let Some(bg) = &self.background {
            if bg != "none" {
                check_color("background", bg)?;
            }
        }
        for (key, side) in self.sides() {
            if let Some(side) = side {
                side.validate(key)?;
            }
        }
        if let Some(b) = &self.border {
            b.validate("border")?;
        }
        for (key, v) in self.paddings() {
            if let Some(v) = v {
                check_range(key, v, 0.0, MAX_PADDING_MM)?;
            }
        }
        if self == &Self::default() {
            return Err(ErrorData::invalid_params(
                "format 에 바꿀 항목이 하나도 없다",
                None,
            ));
        }
        Ok(())
    }

    fn sides(&self) -> [(&'static str, Option<&BorderSide>); 4] {
        [
            ("border_left", self.border_left.as_ref()),
            ("border_right", self.border_right.as_ref()),
            ("border_top", self.border_top.as_ref()),
            ("border_bottom", self.border_bottom.as_ref()),
        ]
    }

    fn paddings(&self) -> [(&'static str, Option<f64>); 4] {
        [
            ("padding_left_mm", self.padding_left_mm),
            ("padding_right_mm", self.padding_right_mm),
            ("padding_top_mm", self.padding_top_mm),
            ("padding_bottom_mm", self.padding_bottom_mm),
        ]
    }

    /// 한 셀에 보낼 rhwp 속성 JSON 두 묶음: (배치·여백, 테두리·배경).
    ///
    /// rhwp 는 테두리 교체와 크기·안 여백 변경을 한 호출에 받지 않으므로 나눠 보낸다.
    /// 빈 묶음은 None.
    pub fn to_rhwp_props(
        &self,
        doc: &Document,
        table: &Table,
        cell: &Cell,
    ) -> Result<(Option<String>, Option<String>), ErrorData> {
        let mut layout = Map::new();
        if let Some(v) = self.vertical_align {
            let code = match v {
                VerticalAlign::Top => 0,
                VerticalAlign::Center => 1,
                VerticalAlign::Bottom => 2,
            };
            layout.insert("verticalAlign".into(), json!(code));
        }
        let pad_keys = ["paddingLeft", "paddingRight", "paddingTop", "paddingBottom"];
        if self.paddings().iter().any(|(_, v)| v.is_some()) {
            // 셀 자기 여백을 켜는 순간 지정하지 않은 변도 자기 값이 쓰이므로, 그 변은 지금
            // 쓰이는 값(표 기본일 수 있다)으로 채워 보이는 여백이 바뀌지 않게 한다.
            let pad = effective_padding(table, cell);
            let cur_pad = [pad.left, pad.right, pad.top, pad.bottom];
            for ((key, (_, v)), cur) in pad_keys.iter().zip(self.paddings()).zip(cur_pad) {
                layout.insert((*key).into(), json!(v.map(mm_to_hu16).unwrap_or(cur)));
            }
            layout.insert("applyInnerMargin".into(), json!(true));
        }
        if let Some(h) = self.header {
            layout.insert("isHeader".into(), json!(h));
        }

        let mut paint = Map::new();
        let bf = border_fill(doc, cell.border_fill_id);
        let rhwp_keys = ["borderLeft", "borderRight", "borderTop", "borderBottom"];
        for (i, (key, side)) in self.sides().into_iter().enumerate() {
            let Some(side) = side.or(self.border.as_ref()) else {
                continue;
            };
            let cur = bf.map(|b| b.borders[i]).unwrap_or_default();
            paint.insert(rhwp_keys[i].into(), side.merged_json(key, &cur)?);
        }
        if let Some(bg) = &self.background {
            if bg == "none" {
                paint.insert("fillType".into(), json!("none"));
            } else {
                let (pattern_color, pattern_type) = match bf.and_then(|b| b.fill.solid.as_ref()) {
                    Some(s) => (color_css(s.pattern_color), s.pattern_type),
                    None => ("#000000".to_string(), 0),
                };
                paint.insert("fillType".into(), json!("solid"));
                paint.insert("fillColor".into(), json!(bg));
                paint.insert("patternColor".into(), json!(pattern_color));
                paint.insert("patternType".into(), json!(pattern_type));
            }
        }
        let to_json = |m: Map<String, Value>| (!m.is_empty()).then(|| Value::Object(m).to_string());
        Ok((to_json(layout), to_json(paint)))
    }
}
