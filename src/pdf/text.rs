use std::collections::HashMap;
use std::sync::Arc;

use failure::Fallible;

use rpdf_lopdf_extra::DocumentExt;

use crate::pdf::data::Name;
use crate::pdf::font::{FontMap, LoadedFont};

const OP_BEGIN_TEXT_OBJECT: &str = "BT";
const OP_END_TEXT_OBJECT: &str = "ET";

struct TextState {
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scaling: f32,
    text_font: Vec<u8>,
    text_font_size: f32,
    text_leading: f32,
}

pub struct TextIter<'a> {
    document: Arc<lopdf::Document>,
    font_map: &'a FontMap,
    loaded_fonts: HashMap<Vec<u8>, LoadedFont>,
    text_state: TextState,
    text_matrix: euclid::Transform2D<f32>,
    text_line_matrix: euclid::Transform2D<f32>,
    operations: std::vec::IntoIter<lopdf::content::Operation>,
    fragments: Vec<TextFragment>,
}

impl<'a> TextIter<'a> {
    pub fn decode(
        document: Arc<lopdf::Document>,
        font_map: &'a FontMap,
        data: &[u8],
    ) -> Fallible<Self> {
        let content = lopdf::content::Content::decode(data)?;

        let text_state = TextState {
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scaling: 1.0,
            text_font: Vec::new(),
            text_font_size: 0.0,
            text_leading: 0.0,
        };

        Ok(TextIter {
            document,
            font_map,
            loaded_fonts: HashMap::new(),
            text_state,
            text_matrix: euclid::Transform2D::identity(),
            text_line_matrix: euclid::Transform2D::identity(),
            operations: content.operations.into_iter(),
            fragments: Vec::new(),
        })
    }

    fn flush_segment(&mut self, chars: &[u8]) {
        let font = self.font_map.get(&self.text_state.text_font).unwrap();
        let loaded_font = self
            .loaded_fonts
            .entry(self.text_state.text_font.clone())
            .or_insert_with(|| font.load().unwrap());

        let mut fragment = TextFragment {
            transform: self.text_matrix,
            font_name: self.text_state.text_font.clone(),
            font_size: self.text_state.text_font_size,
            line_height: self.text_state.text_leading,
            glyphs: Vec::with_capacity(chars.len()),
        };

        for c in chars {
            let index;
            if let Some(glyph_name) = font.decode_char(*c) {
                index = loaded_font.glyph_index_for_name(glyph_name.as_bytes());
            } else {
                index = loaded_font.glyph_index_for_char(*c as char);
            }

            let origin = euclid::Point2D::zero();
            let w0 = font.width_for_char(*c) as f32;
            let tx = (w0 * self.text_state.text_font_size
                + self.text_state.char_spacing
                + self.text_state.word_spacing)
                * self.text_state.horizontal_scaling;

            fragment.glyphs.push(TextGlyph {
                index,
                origin: self.text_matrix.transform_point(&origin),
                advance: tx,
            });

            let translation = euclid::Transform2D::create_translation(tx, 0.0);
            self.text_matrix = self.text_matrix.pre_mul(&translation);
        }

        self.fragments.push(fragment);
    }

    fn handle_text_state_operation(&mut self, op: &lopdf::content::Operation) -> Fallible<()> {
        match op.operator.as_str() {
            "Tc" => {
                let char_spacing = self.document.deserialize_object(&op.operands[0])?;
                self.text_state.char_spacing = char_spacing;
            }
            "Tw" => {
                let word_spacing = self.document.deserialize_object(&op.operands[0])?;
                self.text_state.word_spacing = word_spacing;
            }
            "Tz" => {
                let scale: f32 = self.document.deserialize_object(&op.operands[0])?;
                self.text_state.horizontal_scaling = scale / 100.0;
            }
            "TL" => match op.operands[0] {
                lopdf::Object::Real(leading) => {
                    self.text_state.text_leading = leading as f32;
                }
                lopdf::Object::Integer(leading) => {
                    self.text_state.text_leading -= leading as f32;
                }
                _ => failure::bail!("unexpected operand {:?}", op),
            },
            "Tf" => {
                let Name(font_name) = self.document.deserialize_object(&op.operands[0])?;
                let font_size = self.document.deserialize_object(&op.operands[1])?;
                self.text_state.text_font = font_name;
                self.text_state.text_font_size = font_size;
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_translation(&mut self, x: f32, y: f32) {
        let translation = euclid::Transform2D::create_translation(x as f32, y as f32);
        let transform = self.text_line_matrix.pre_mul(&translation);
        self.text_matrix = transform;
        self.text_line_matrix = transform;
    }

    fn apply_adjustment(&mut self, adjustment: f32) {
        let tx = (adjustment / 1000.0)
            * self.text_state.text_font_size
            * self.text_state.horizontal_scaling;
        let translation = euclid::Transform2D::create_translation(-tx, 0.0);
        self.text_matrix = self.text_matrix.pre_mul(&translation);
    }

    fn handle_text_position_operation(&mut self, op: &lopdf::content::Operation) -> Fallible<()> {
        match op.operator.as_str() {
            "Td" => {
                let x = self.document.deserialize_object(&op.operands[0])?;
                let y = self.document.deserialize_object(&op.operands[1])?;
                self.apply_translation(x, y);
            }
            "TD" => {
                let x = self.document.deserialize_object(&op.operands[0])?;
                let y = self.document.deserialize_object(&op.operands[1])?;
                self.apply_translation(x, y);
                self.text_state.text_leading = -y;
            }
            "Tm" => {
                let a = self.document.deserialize_object(&op.operands[0])?;
                let b = self.document.deserialize_object(&op.operands[1])?;
                let c = self.document.deserialize_object(&op.operands[2])?;
                let d = self.document.deserialize_object(&op.operands[3])?;
                let e = self.document.deserialize_object(&op.operands[4])?;
                let f = self.document.deserialize_object(&op.operands[5])?;
                let transform = euclid::Transform2D::row_major(a, b, c, d, e, f);
                self.text_matrix = transform;
                self.text_line_matrix = transform;
            }
            "T*" => {
                self.apply_translation(0.0, self.text_state.text_leading);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_text_show_operation(&mut self, op: &lopdf::content::Operation) -> Fallible<()> {
        match op.operator.as_str() {
            "Tj" => match op.operands[0] {
                lopdf::Object::String(ref s, _) => {
                    self.flush_segment(s);
                }
                _ => failure::bail!("unexpected operand {:?}", op),
            },
            "'" => match op.operands[0] {
                lopdf::Object::String(ref s, _) => {
                    self.apply_translation(0.0, self.text_state.text_leading);
                    self.flush_segment(s);
                }
                _ => failure::bail!("unexpected operand {:?}", op),
            },
            "\"" => {
                let word_spacing = self.document.deserialize_object(&op.operands[0])?;
                let char_spacing = self.document.deserialize_object(&op.operands[1])?;
                match op.operands[2] {
                    lopdf::Object::String(ref s, _) => {
                        self.text_state.word_spacing = word_spacing;
                        self.text_state.char_spacing = char_spacing;
                        self.apply_translation(0.0, self.text_state.text_leading);
                        self.flush_segment(s);
                    }
                    _ => failure::bail!("unexpected operand {:?}", op),
                }
            }
            "TJ" => match op.operands[0] {
                lopdf::Object::Array(ref parts) => {
                    for part in parts {
                        match part {
                            lopdf::Object::String(ref s, _) => {
                                self.flush_segment(s);
                            }
                            lopdf::Object::Real(amount) => {
                                self.apply_adjustment(*amount as f32);
                            }
                            lopdf::Object::Integer(amount) => {
                                self.apply_adjustment(*amount as f32);
                            }
                            _ => {}
                        }
                    }
                }
                _ => failure::bail!("unexpected operand {:?}", op),
            },
            _ => {}
        }
        Ok(())
    }
}

impl<'a> Iterator for TextIter<'a> {
    type Item = TextObject;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(operation) = self.operations.next() {
                match operation.operator.as_str() {
                    OP_BEGIN_TEXT_OBJECT => {
                        self.text_matrix = euclid::Transform2D::identity();
                        self.text_line_matrix = euclid::Transform2D::identity();
                    }
                    OP_END_TEXT_OBJECT => {
                        return Some(TextObject {
                            fragments: self.fragments.split_off(0),
                        });
                    }
                    "Tc" | "Tw" | "Tz" | "TL" | "Tf" => {
                        self.handle_text_state_operation(&operation).unwrap();
                    }
                    "Td" | "TD" | "Tm" | "T*" => {
                        self.handle_text_position_operation(&operation).unwrap();
                    }
                    "Tj" | "'" | "\"" | "TJ" => {
                        self.handle_text_show_operation(&operation).unwrap();
                    }
                    _ => {}
                }
            } else {
                return None;
            }
        }
    }
}

pub struct TextObject {
    pub fragments: Vec<TextFragment>,
}

pub struct TextFragment {
    pub transform: euclid::Transform2D<f32>,
    pub font_name: Vec<u8>,
    pub font_size: f32,
    pub line_height: f32,
    pub glyphs: Vec<TextGlyph>,
}

pub struct TextGlyph {
    pub index: u32,
    pub origin: euclid::Point2D<f32>,
    pub advance: f32,
}
