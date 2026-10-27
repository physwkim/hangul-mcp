//! 개체 배치(글자처럼 취급·본문과의 배치·가로/세로 기준과 정렬·오프셋·바깥 여백)와
//! 표·그림·도형 서식.
//!
//! 표·그림·도형은 rhwp 에서 같은 `CommonObjAttr` 배치 필드와 같은 속성 키(`treatAsChar`,
//! `textWrap`, `horzRelTo` …)를 쓰므로 한 `Placement` 타입이 모두 맡는다. 개체마다 다른 것
//! (바깥 여백 키, rhwp 가 받는 열거값, 오프셋 표기)은 `PlacementTarget` 하나에 모은다. rhwp 는 받지 않는
//! 열거 문자열을 오류 없이 무시하거나 다른 값으로 바꾸므로 여기서 먼저 거부한다.

use rhwp::model::image::{ImageEffect as RImageEffect, Picture};
use rhwp::model::shape::{
    CommonObjAttr, HorzAlign as RHorzAlign, HorzRelTo as RHorzRelTo, ShapeObject,
    TextWrap as RTextWrap, VertAlign as RVertAlign, VertRelTo as RVertRelTo,
};
use rhwp::model::style::FillType;
use rhwp::model::table::Table;
use rhwp::model::Padding;
use rmcp::ErrorData;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::format::{check_color, check_range, color_bgr, color_css};
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

/// 배치를 받는 개체. rhwp 의 개체별 속성 setter 가 받는 키와 값이 다르다:
/// 표(`set_table_properties_native`)는 tight/through 배치를 모르고, 그림
/// (`set_picture_properties_native`)과 도형(`set_shape_properties_native`)은 inside/outside
/// 정렬을 모른다. 도형은 오프셋을 부호 없는 정수로만 읽어 음수를 버린다.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlacementTarget {
    Table,
    Picture,
    Shape,
}

impl PlacementTarget {
    fn name(self) -> &'static str {
        match self {
            Self::Table => "표",
            Self::Picture => "그림",
            Self::Shape => "도형",
        }
    }

    /// 바깥 여백 키 (왼, 오른, 위, 아래).
    fn margin_keys(self) -> [&'static str; 4] {
        match self {
            Self::Table => ["outerLeft", "outerRight", "outerTop", "outerBottom"],
            Self::Picture | Self::Shape => [
                "outerMarginLeft",
                "outerMarginRight",
                "outerMarginTop",
                "outerMarginBottom",
            ],
        }
    }

    /// 오프셋 값. HWP 의 오프셋은 부호 있는 HWPUNIT 를 u32 칸에 담는다(한컴 HWPX 도 음수를
    /// 4294965296 처럼 쓴다). 도형 setter 는 u32 로만 읽으므로 같은 비트 패턴으로 넘긴다.
    fn offset_value(self, hu: i32) -> Value {
        match self {
            Self::Table | Self::Picture => json!(hu),
            Self::Shape => json!(hu as u32),
        }
    }
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

    /// `target` 의 rhwp 속성 키로 옮긴다. 그 개체에 rhwp 가 받지 않는 값은 거부한다.
    fn put(
        &self,
        props: &mut Map<String, Value>,
        target: PlacementTarget,
    ) -> Result<(), ErrorData> {
        if let Some(v) = self.inline {
            props.insert("treatAsChar".into(), json!(v));
        }
        if let Some(w) = self.text_wrap {
            let name = match w {
                TextWrap::Square => "Square",
                TextWrap::Tight | TextWrap::Through if target == PlacementTarget::Table => {
                    return Err(ErrorData::invalid_params(
                        "표의 text_wrap 은 square/top_and_bottom/behind_text/in_front_of_text 만 된다",
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
        let inside_outside = |name: &str| {
            Err(ErrorData::invalid_params(
                format!("{}의 {name} 은 inside/outside 를 쓸 수 없다", target.name()),
                None,
            ))
        };
        if let Some(v) = self.horz_align {
            let name = match v {
                HorzAlign::Inside | HorzAlign::Outside if target != PlacementTarget::Table => {
                    return inside_outside("horz_align")
                }
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
                VertAlign::Inside | VertAlign::Outside if target != PlacementTarget::Table => {
                    return inside_outside("vert_align")
                }
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
                props.insert(key.into(), target.offset_value(mm_to_hu(v).round() as i32));
            }
        }
        for ((name, v), key) in self.margins().into_iter().zip(target.margin_keys()) {
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
        self.placement.put(&mut props, PlacementTarget::Table)?;
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

/// 그림 효과.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ImageEffect {
    /// 원래 그림
    Original,
    /// 회색조
    GrayScale,
    /// 흑백
    BlackWhite,
    /// 무늬
    Pattern8x8,
}

/// 그림 서식: 배치 + 크기·설명·밝기·대비·투명도·효과·회전·대칭. 적용 시 지정한 항목만 바뀐다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct PictureFormat {
    #[serde(flatten)]
    pub placement: Placement,
    /// 그림 폭 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_mm: Option<f64>,
    /// 그림 높이 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_mm: Option<f64>,
    /// 그림 설명(대체 텍스트). 제어 문자 불가
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 밝기 (-100..100)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<i8>,
    /// 대비 (-100..100)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast: Option<i8>,
    /// 투명도 (%, 0 = 불투명 .. 100 = 완전 투명)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparency: Option<u8>,
    /// 그림 효과
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<ImageEffect>,
    /// 회전 각 (도, -360..360)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation_deg: Option<i16>,
    /// 좌우 대칭
    #[serde(skip_serializing_if = "Option::is_none")]
    pub horz_flip: Option<bool>,
    /// 상하 대칭
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vert_flip: Option<bool>,
}

impl PictureFormat {
    pub fn from_picture(pic: &Picture) -> Self {
        Self {
            placement: Placement::from_common(&pic.common, &pic.common.margin),
            width_mm: Some(hu_to_mm(pic.common.width as f64)),
            height_mm: Some(hu_to_mm(pic.common.height as f64)),
            description: Some(pic.common.description.clone()),
            brightness: Some(pic.image_attr.brightness),
            contrast: Some(pic.image_attr.contrast),
            transparency: Some(pic.image_attr.transparency),
            effect: Some(match pic.image_attr.effect {
                RImageEffect::RealPic => ImageEffect::Original,
                RImageEffect::GrayScale => ImageEffect::GrayScale,
                RImageEffect::BlackWhite => ImageEffect::BlackWhite,
                RImageEffect::Pattern8x8 => ImageEffect::Pattern8x8,
            }),
            rotation_deg: Some(pic.shape_attr.rotation_angle),
            horz_flip: Some(pic.shape_attr.horz_flip),
            vert_flip: Some(pic.shape_attr.vert_flip),
        }
    }

    /// rhwp `set_picture_properties_native` 속성 JSON. 값을 모두 검증한 뒤에 만든다.
    pub fn to_rhwp_json(&self) -> Result<String, ErrorData> {
        let mut props = Map::new();
        self.placement.put(&mut props, PlacementTarget::Picture)?;
        for (name, key, v) in [
            ("width_mm", "width", self.width_mm),
            ("height_mm", "height", self.height_mm),
        ] {
            if let Some(v) = v {
                check_range(name, v, 0.1, MAX_PAPER_MM)?;
                props.insert(key.into(), json!(mm_to_hu(v).round().max(1.0) as u32));
            }
        }
        put_description(&mut props, self.description.as_deref())?;
        for (name, key, v) in [
            ("brightness", "brightness", self.brightness),
            ("contrast", "contrast", self.contrast),
        ] {
            if let Some(v) = v {
                check_range(name, f64::from(v), -100.0, 100.0)?;
                props.insert(key.into(), json!(v));
            }
        }
        if let Some(v) = self.transparency {
            check_range("transparency", f64::from(v), 0.0, 100.0)?;
            props.insert("transparency".into(), json!(v));
        }
        if let Some(e) = self.effect {
            let name = match e {
                ImageEffect::Original => "RealPic",
                ImageEffect::GrayScale => "GrayScale",
                ImageEffect::BlackWhite => "BlackWhite",
                ImageEffect::Pattern8x8 => "Pattern8x8",
            };
            props.insert("effect".into(), json!(name));
        }
        put_transform(
            &mut props,
            self.rotation_deg,
            self.horz_flip,
            self.vert_flip,
        )?;
        if props.is_empty() {
            return Err(ErrorData::invalid_params(
                "format 에 바꿀 항목이 하나도 없다",
                None,
            ));
        }
        Ok(Value::Object(props).to_string())
    }
}

/// 그림·도형 설명(대체 텍스트).
fn put_description(props: &mut Map<String, Value>, d: Option<&str>) -> Result<(), ErrorData> {
    if let Some(d) = d {
        // rhwp 의 JSON 문자열 해석은 \uXXXX 이스케이프를 풀지 않는다.
        if d.chars().any(char::is_control) {
            return Err(ErrorData::invalid_params(
                "description 에 제어 문자(줄바꿈 포함)를 쓸 수 없다",
                None,
            ));
        }
        props.insert("description".into(), json!(d));
    }
    Ok(())
}

/// 그림·도형 회전 각과 대칭.
fn put_transform(
    props: &mut Map<String, Value>,
    rotation_deg: Option<i16>,
    horz_flip: Option<bool>,
    vert_flip: Option<bool>,
) -> Result<(), ErrorData> {
    if let Some(v) = rotation_deg {
        check_range("rotation_deg", f64::from(v), -360.0, 360.0)?;
        props.insert("rotationAngle".into(), json!(v));
    }
    for (key, v) in [("horzFlip", horz_flip), ("vertFlip", vert_flip)] {
        if let Some(v) = v {
            props.insert(key.into(), json!(v));
        }
    }
    Ok(())
}

/// 선 굵기 상한 (mm).
const MAX_LINE_WIDTH_MM: f64 = 10.0;

/// 색 없음 (HWPX "none").
const NO_COLOR: u32 = 0xFFFF_FFFF;

/// 도형 서식: 배치 + 크기·선·채우기·설명·회전·대칭. 적용 시 지정한 항목만 바뀐다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct ShapeFormat {
    #[serde(flatten)]
    pub placement: Placement,
    /// 도형 폭 (mm). 직선은 0 일 수 있다(세로선)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_mm: Option<f64>,
    /// 도형 높이 (mm). 직선은 0 일 수 있다(가로선)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_mm: Option<f64>,
    /// 선 색 ("#RRGGBB")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_color: Option<String>,
    /// 선 굵기 (mm, 0..10)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_width_mm: Option<f64>,
    /// 채우기 색 ("#RRGGBB") 또는 "none"(채우기 없음). 직선에는 없다.
    /// 그러데이션/그림 채우기는 조회에서 생략된다
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_color: Option<String>,
    /// 도형 설명(대체 텍스트). 제어 문자 불가
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 회전 각 (도, -360..360)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation_deg: Option<i16>,
    /// 좌우 대칭
    #[serde(skip_serializing_if = "Option::is_none")]
    pub horz_flip: Option<bool>,
    /// 상하 대칭
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vert_flip: Option<bool>,
}

impl ShapeFormat {
    /// `shape` 는 그리기 개체(직선·사각형·타원·호·다각형·곡선)여야 한다.
    pub fn from_shape(shape: &ShapeObject) -> Self {
        let c = shape.common();
        let d = shape.drawing().expect("그리기 개체");
        let fill_color = match (&shape, d.fill.fill_type, &d.fill.solid) {
            (ShapeObject::Line(_), _, _) => None,
            (_, FillType::None, _) => Some("none".to_string()),
            (_, FillType::Solid, Some(solid)) if solid.background_color == NO_COLOR => {
                Some("none".to_string())
            }
            (_, FillType::Solid, Some(solid)) => Some(color_css(solid.background_color)),
            _ => None,
        };
        Self {
            placement: Placement::from_common(c, &c.margin),
            width_mm: Some(hu_to_mm(c.width as f64)),
            height_mm: Some(hu_to_mm(c.height as f64)),
            line_color: Some(color_css(d.border_line.color)),
            line_width_mm: Some(hu_to_mm(d.border_line.width as f64)),
            fill_color,
            description: Some(c.description.clone()),
            rotation_deg: Some(d.shape_attr.rotation_angle),
            horz_flip: Some(d.shape_attr.horz_flip),
            vert_flip: Some(d.shape_attr.vert_flip),
        }
    }

    /// rhwp `set_shape_properties_native` 속성 JSON. 값을 모두 검증한 뒤에 만든다.
    /// rhwp 는 0 크기를 오류 없이 최소 크기로 바꾸므로, 0 은 이미 0 인 변(직선)에만 받는다.
    pub fn to_rhwp_json(&self, shape: &ShapeObject) -> Result<String, ErrorData> {
        let mut props = Map::new();
        self.placement.put(&mut props, PlacementTarget::Shape)?;
        let c = shape.common();
        for (name, key, v, current) in [
            ("width_mm", "width", self.width_mm, c.width),
            ("height_mm", "height", self.height_mm, c.height),
        ] {
            if let Some(v) = v {
                check_range(name, v, 0.0, MAX_PAPER_MM)?;
                let hu = mm_to_hu(v).round() as u32;
                if hu == 0 && current != 0 {
                    return Err(ErrorData::invalid_params(
                        format!("{name} 을 0 으로 줄일 수 없다 (0 은 직선을 넣을 때만)"),
                        None,
                    ));
                }
                props.insert(key.into(), json!(hu));
            }
        }
        if let Some(v) = &self.line_color {
            check_color("line_color", v)?;
            props.insert("borderColor".into(), json!(color_bgr(v)));
        }
        if let Some(v) = self.line_width_mm {
            check_range("line_width_mm", v, 0.0, MAX_LINE_WIDTH_MM)?;
            props.insert("borderWidth".into(), json!(mm_to_hu(v).round() as i32));
        }
        if let Some(v) = &self.fill_color {
            if matches!(shape, ShapeObject::Line(_)) {
                return Err(ErrorData::invalid_params(
                    "직선에는 fill_color 를 쓸 수 없다",
                    None,
                ));
            }
            if v == "none" {
                // HWPX 는 채우기 없음을 winBrush faceColor="none" 으로 쓰고 읽으면 단색 NO_COLOR
                // 가 된다. rhwp 는 채우기 종류 none 에도 남은 단색 값을 그대로 써 넣으므로 색도 지운다.
                props.insert("fillType".into(), json!("none"));
                props.insert("fillBgColor".into(), json!(NO_COLOR as i32));
            } else {
                check_color("fill_color", v)?;
                props.insert("fillType".into(), json!("solid"));
                props.insert("fillBgColor".into(), json!(color_bgr(v)));
            }
        }
        put_description(&mut props, self.description.as_deref())?;
        put_transform(
            &mut props,
            self.rotation_deg,
            self.horz_flip,
            self.vert_flip,
        )?;
        if props.is_empty() {
            return Err(ErrorData::invalid_params(
                "format 에 바꿀 항목이 하나도 없다",
                None,
            ));
        }
        Ok(Value::Object(props).to_string())
    }
}
