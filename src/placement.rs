//! 개체 배치(글자처럼 취급·본문과의 배치·가로/세로 기준과 정렬·오프셋·바깥 여백)와 표 서식.
//!
//! 표와 그림은 rhwp 에서 같은 `CommonObjAttr` 배치 필드와 같은 속성 키(`treatAsChar`,
//! `textWrap`, `horzRelTo` …)를 쓰므로 한 `Placement` 타입이 둘 다 맡는다. 바깥 여백 키만
//! 개체마다 다르다(표 `outerLeft`, 그림 `outerMarginLeft`). rhwp 는 모르는 열거 문자열을 오류
//! 없이 첫 값으로 바꾸므로 열거값은 여기서 타입으로 막는다.

use rhwp::model::shape::{
    CommonObjAttr, HorzAlign as RHorzAlign, HorzRelTo as RHorzRelTo, TextWrap as RTextWrap,
    VertAlign as RVertAlign, VertRelTo as RVertRelTo,
};
use rhwp::model::table::Table;
use rhwp::model::Padding;
use rmcp::ErrorData;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::format::check_range;
use crate::page::{mm_to_hu, MAX_PAPER_MM};

/// 여백·간격 한 변의 상한 (mm). HWPUNIT16(i16, 약 115mm) 안.
const MAX_MARGIN_MM: f64 = 100.0;

fn hu_to_mm(hu: f64) -> f64 {
    (hu * 25.4 / 7200.0 * 100.0).round() / 100.0
}

fn mm_to_hu16(mm: f64) -> i16 {
    mm_to_hu(mm).round() as i16
}

/// 본문과의 배치.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TextWrap {
    /// 어울림
    Square,
    /// 빈틈없이 (그림 전용)
    Tight,
    /// 투과 (그림 전용)
    Through,
    /// 자리 차지
    TopAndBottom,
    /// 글 뒤로
    BehindText,
    /// 글 앞으로
    InFrontOfText,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum HorzRelTo {
    Paper,
    Page,
    Column,
    Para,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum VertRelTo {
    Paper,
    Page,
    Para,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum HorzAlign {
    Left,
    Center,
    Right,
    Inside,
    Outside,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum VertAlign {
    Top,
    Center,
    Bottom,
    Inside,
    Outside,
}

/// 개체 배치. 적용 시 지정한 항목만 바뀐다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct Placement {
    /// 글자처럼 취급
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
    /// 본문과의 배치 (글자처럼 취급이 아닐 때)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_wrap: Option<TextWrap>,
    /// 가로 위치 기준
    #[serde(skip_serializing_if = "Option::is_none")]
    pub horz_rel_to: Option<HorzRelTo>,
    /// 가로 정렬 (기준 영역 안)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub horz_align: Option<HorzAlign>,
    /// 가로 오프셋 (mm, 정렬 위치에서)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub horz_offset_mm: Option<f64>,
    /// 세로 위치 기준
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vert_rel_to: Option<VertRelTo>,
    /// 세로 정렬 (기준 영역 안)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vert_align: Option<VertAlign>,
    /// 세로 오프셋 (mm, 정렬 위치에서)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vert_offset_mm: Option<f64>,
    /// 바깥 여백 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_left_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_right_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_top_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_bottom_mm: Option<f64>,
}

impl Placement {
    pub fn from_common(c: &CommonObjAttr, outer: &Padding) -> Self {
        Self {
            inline: Some(c.treat_as_char),
            text_wrap: Some(match c.text_wrap {
                RTextWrap::Square => TextWrap::Square,
                RTextWrap::Tight => TextWrap::Tight,
                RTextWrap::Through => TextWrap::Through,
                RTextWrap::TopAndBottom => TextWrap::TopAndBottom,
                RTextWrap::BehindText => TextWrap::BehindText,
                RTextWrap::InFrontOfText => TextWrap::InFrontOfText,
            }),
            horz_rel_to: Some(match c.horz_rel_to {
                RHorzRelTo::Paper => HorzRelTo::Paper,
                RHorzRelTo::Page => HorzRelTo::Page,
                RHorzRelTo::Column => HorzRelTo::Column,
                RHorzRelTo::Para => HorzRelTo::Para,
            }),
            horz_align: Some(match c.horz_align {
                RHorzAlign::Left => HorzAlign::Left,
                RHorzAlign::Center => HorzAlign::Center,
                RHorzAlign::Right => HorzAlign::Right,
                RHorzAlign::Inside => HorzAlign::Inside,
                RHorzAlign::Outside => HorzAlign::Outside,
            }),
            horz_offset_mm: Some(hu_to_mm(c.horizontal_offset as i32 as f64)),
            vert_rel_to: Some(match c.vert_rel_to {
                RVertRelTo::Paper => VertRelTo::Paper,
                RVertRelTo::Page => VertRelTo::Page,
                RVertRelTo::Para => VertRelTo::Para,
            }),
            vert_align: Some(match c.vert_align {
                RVertAlign::Top => VertAlign::Top,
                RVertAlign::Center => VertAlign::Center,
                RVertAlign::Bottom => VertAlign::Bottom,
                RVertAlign::Inside => VertAlign::Inside,
                RVertAlign::Outside => VertAlign::Outside,
            }),
            vert_offset_mm: Some(hu_to_mm(c.vertical_offset as i32 as f64)),
            margin_left_mm: Some(hu_to_mm(outer.left as f64)),
            margin_right_mm: Some(hu_to_mm(outer.right as f64)),
            margin_top_mm: Some(hu_to_mm(outer.top as f64)),
            margin_bottom_mm: Some(hu_to_mm(outer.bottom as f64)),
        }
    }

    fn margins(&self) -> [(&'static str, Option<f64>); 4] {
        [
            ("margin_left_mm", self.margin_left_mm),
            ("margin_right_mm", self.margin_right_mm),
            ("margin_top_mm", self.margin_top_mm),
            ("margin_bottom_mm", self.margin_bottom_mm),
        ]
    }

    /// rhwp 속성 키로 옮긴다. `margin_keys` 는 개체의 바깥 여백 키 (왼, 오른, 위, 아래).
    /// `allow_tight_through=false` 면 rhwp 가 받지 않는(어울림으로 바꾸는) 두 배치를 거부한다.
    fn put(
        &self,
        props: &mut Map<String, Value>,
        margin_keys: [&str; 4],
        allow_tight_through: bool,
    ) -> Result<(), ErrorData> {
        if let Some(v) = self.inline {
            props.insert("treatAsChar".into(), json!(v));
        }
        if let Some(w) = self.text_wrap {
            let name = match w {
                TextWrap::Square => "Square",
                TextWrap::Tight | TextWrap::Through if !allow_tight_through => {
                    return Err(ErrorData::invalid_params(
                        "이 개체의 text_wrap 은 square/top_and_bottom/behind_text/in_front_of_text 만 된다",
                        None,
                    ))
                }
                TextWrap::Tight => "Tight",
                TextWrap::Through => "Through",
                TextWrap::TopAndBottom => "TopAndBottom",
                TextWrap::BehindText => "BehindText",
                TextWrap::InFrontOfText => "InFrontOfText",
            };
            props.insert("textWrap".into(), json!(name));
        }
        if let Some(v) = self.horz_rel_to {
            let name = match v {
                HorzRelTo::Paper => "Paper",
                HorzRelTo::Page => "Page",
                HorzRelTo::Column => "Column",
                HorzRelTo::Para => "Para",
            };
            props.insert("horzRelTo".into(), json!(name));
        }
        if let Some(v) = self.horz_align {
            let name = match v {
                HorzAlign::Left => "Left",
                HorzAlign::Center => "Center",
                HorzAlign::Right => "Right",
                HorzAlign::Inside => "Inside",
                HorzAlign::Outside => "Outside",
            };
            props.insert("horzAlign".into(), json!(name));
        }
        if let Some(v) = self.vert_rel_to {
            let name = match v {
                VertRelTo::Paper => "Paper",
                VertRelTo::Page => "Page",
                VertRelTo::Para => "Para",
            };
            props.insert("vertRelTo".into(), json!(name));
        }
        if let Some(v) = self.vert_align {
            let name = match v {
                VertAlign::Top => "Top",
                VertAlign::Center => "Center",
                VertAlign::Bottom => "Bottom",
                VertAlign::Inside => "Inside",
                VertAlign::Outside => "Outside",
            };
            props.insert("vertAlign".into(), json!(name));
        }
        for (name, key, v) in [
            ("horz_offset_mm", "horzOffset", self.horz_offset_mm),
            ("vert_offset_mm", "vertOffset", self.vert_offset_mm),
        ] {
            if let Some(v) = v {
                check_range(name, v, -MAX_PAPER_MM, MAX_PAPER_MM)?;
                props.insert(key.into(), json!(mm_to_hu(v).round() as i32));
            }
        }
        for ((name, v), key) in self.margins().into_iter().zip(margin_keys) {
            if let Some(v) = v {
                check_range(name, v, 0.0, MAX_MARGIN_MM)?;
                props.insert(key.into(), json!(mm_to_hu16(v)));
            }
        }
        Ok(())
    }
}

/// 표 서식: 배치 + 표 기본 셀 안 여백·셀 간격·제목 줄 반복. 적용 시 지정한 항목만 바뀐다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct TableFormat {
    #[serde(flatten)]
    pub placement: Placement,
    /// 셀 기본 안 여백 (mm). 자기 안 여백을 켠 셀(apply_cell_format padding_*)은 따르지 않는다
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_padding_left_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_padding_right_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_padding_top_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_padding_bottom_mm: Option<f64>,
    /// 셀 간격 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_spacing_mm: Option<f64>,
    /// 표가 쪽을 넘을 때 제목 셀 줄을 반복
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_header: Option<bool>,
}

impl TableFormat {
    pub fn from_table(t: &Table) -> Self {
        let outer = Padding {
            left: t.outer_margin_left,
            right: t.outer_margin_right,
            top: t.outer_margin_top,
            bottom: t.outer_margin_bottom,
        };
        Self {
            placement: Placement::from_common(&t.common, &outer),
            cell_padding_left_mm: Some(hu_to_mm(t.padding.left as f64)),
            cell_padding_right_mm: Some(hu_to_mm(t.padding.right as f64)),
            cell_padding_top_mm: Some(hu_to_mm(t.padding.top as f64)),
            cell_padding_bottom_mm: Some(hu_to_mm(t.padding.bottom as f64)),
            cell_spacing_mm: Some(hu_to_mm(t.cell_spacing as f64)),
            repeat_header: Some(t.repeat_header),
        }
    }

    /// rhwp `set_table_properties_native` 속성 JSON. 값을 모두 검증한 뒤에 만든다.
    pub fn to_rhwp_json(&self) -> Result<String, ErrorData> {
        let mut props = Map::new();
        self.placement.put(
            &mut props,
            ["outerLeft", "outerRight", "outerTop", "outerBottom"],
            false,
        )?;
        for (name, key, v) in [
            (
                "cell_padding_left_mm",
                "paddingLeft",
                self.cell_padding_left_mm,
            ),
            (
                "cell_padding_right_mm",
                "paddingRight",
                self.cell_padding_right_mm,
            ),
            (
                "cell_padding_top_mm",
                "paddingTop",
                self.cell_padding_top_mm,
            ),
            (
                "cell_padding_bottom_mm",
                "paddingBottom",
                self.cell_padding_bottom_mm,
            ),
            ("cell_spacing_mm", "cellSpacing", self.cell_spacing_mm),
        ] {
            if let Some(v) = v {
                check_range(name, v, 0.0, MAX_MARGIN_MM)?;
                props.insert(key.into(), json!(mm_to_hu16(v)));
            }
        }
        if let Some(v) = self.repeat_header {
            props.insert("repeatHeader".into(), json!(v));
        }
        if props.is_empty() {
            return Err(ErrorData::invalid_params(
                "format 에 바꿀 항목이 하나도 없다",
                None,
            ));
        }
        Ok(Value::Object(props).to_string())
    }
}
