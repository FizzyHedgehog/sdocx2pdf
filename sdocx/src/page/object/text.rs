use std::io::{self, Read, Seek};

use thiserror::Error;

use crate::{
    byte_stream::{BoundedStream, ByteStreamLe, UnfinishedParsingError},
    context::{DocumentContext, TryParseWithContext},
    page::object::{
        base::{HasObjectBase, ObjectBase},
        header::{FlagBlockError, ObjectHeaderError, try_parse_object_header},
        shape::{InvalidBorderTypeError, Shape, ShapeParseContext, ShapeParseError},
        text_core::{BulletStyle, InlineObject, ParagraphAlignment, TextStyle},
    },
    unpack_field_flags,
};

#[derive(Error, Debug)]
#[error(transparent)]
pub enum TextParseError {
    Io(#[from] io::Error),
    Shape(#[from] ShapeParseError),
    Header(#[from] ObjectHeaderError),
    FlagBlock(#[from] FlagBlockError),
    BadBorderType(#[from] InvalidBorderTypeError),
    Unfinished(#[from] UnfinishedParsingError),
}

#[derive(Debug)]
pub struct Text {
    shape: Shape,
}

impl Text {
    pub fn raw_string(&self) -> Option<&str> {
        self.shape.raw_text_string()
    }

    pub fn text_common(&self) -> Option<&crate::page::object::text_core::Common> {
        self.shape.text_data.text_common.as_ref()
    }

    pub fn inline_objects(&self) -> &[InlineObject] {
        match self.shape.text_data.text_common.as_ref() {
            Some(tc) => &tc.inline_objects[..],
            None => &[],
        }
    }

    pub fn section_data(&self) -> &[(u32, u32)] {
        match self.text_common() {
            Some(tc) => tc.section_data(),
            None => &[],
        }
    }

    pub fn left_margin(&self) -> f32 {
        self.text_common().map(|tc| tc.left_margin()).unwrap_or(0.0)
    }

    pub fn top_margin(&self) -> f32 {
        self.text_common().map(|tc| tc.top_margin()).unwrap_or(0.0)
    }

    pub fn right_margin(&self) -> f32 {
        self.text_common().map(|tc| tc.right_margin()).unwrap_or(0.0)
    }

    pub fn bottom_margin(&self) -> f32 {
        self.text_common().map(|tc| tc.bottom_margin()).unwrap_or(0.0)
    }

    pub fn font_size_at(&self, pos: u32) -> f32 {
        self.text_common().map(|tc| tc.font_size_at(pos)).unwrap_or(15.0)
    }

    pub fn text_color_at(&self, pos: u32) -> [u8; 4] {
        self.text_common().map(|tc| tc.text_color_at(pos)).unwrap_or([37, 37, 37, 255])
    }

    pub fn is_bold_at(&self, pos: u32) -> bool {
        self.text_common().map(|tc| tc.is_bold_at(pos)).unwrap_or(false)
    }

    pub fn is_italic_at(&self, pos: u32) -> bool {
        self.text_common()
            .map(|tc| tc.is_italic_at(pos))
            .unwrap_or(false)
    }

    pub fn is_underline_at(&self, pos: u32) -> bool {
        self.text_common()
            .map(|tc| tc.is_underline_at(pos))
            .unwrap_or(false)
    }

    pub fn is_strikethrough_at(&self, pos: u32) -> bool {
        self.text_common()
            .map(|tc| tc.is_strikethrough_at(pos))
            .unwrap_or(false)
    }

    /// The full character-level formatting that applies at `pos` (a UTF-16 position in the raw
    /// string).
    pub fn style_at(&self, pos: u32) -> Option<TextStyle> {
        self.text_common().map(|tc| tc.style_at(pos))
    }

    /// Splits `[start, end)` (UTF-16 positions) into contiguous runs that share a single style.
    pub fn style_runs(&self, start: u32, end: u32) -> Vec<(u32, TextStyle)> {
        match self.text_common() {
            Some(tc) => tc.style_runs(start, end),
            None => vec![(start, TextStyle::default())],
        }
    }

    /// The index of the paragraph containing the UTF-16 position `pos`.
    pub fn paragraph_index_at(&self, pos: u32) -> usize {
        self.text_common()
            .map(|tc| tc.paragraph_index_at(pos))
            .unwrap_or(0)
    }

    /// The UTF-16 position at which the paragraph with `paragraph_index` starts.
    pub fn paragraph_start_at(&self, paragraph_index: usize) -> u32 {
        self.text_common()
            .map(|tc| tc.paragraph_start_at(paragraph_index))
            .unwrap_or(0)
    }

    /// The indentation level of the paragraph at `paragraph_index`.
    pub fn indent_level_at_paragraph(&self, paragraph_index: usize) -> u32 {
        self.text_common()
            .map(|tc| tc.indent_level_at_paragraph(paragraph_index))
            .unwrap_or(0)
    }

    /// List-item information for the paragraph at `paragraph_index`, if it is a list item.
    pub fn bullet_at_paragraph(&self, paragraph_index: usize) -> Option<BulletStyle> {
        self.text_common()
            .and_then(|tc| tc.bullet_at_paragraph(paragraph_index))
    }

    /// The horizontal alignment of the paragraph at `paragraph_index`.
    pub fn alignment_at_paragraph(&self, paragraph_index: usize) -> ParagraphAlignment {
        self.text_common()
            .map(|tc| tc.alignment_at_paragraph(paragraph_index))
            .unwrap_or_default()
    }
}

impl<R: Read + Seek> TryParseWithContext<R, DocumentContext<'_, '_>> for Text {
    type ParseError = TextParseError;

    fn try_parse_with_ctx(
        stream: &mut R,
        &doc_ctx: &DocumentContext<'_, '_>,
    ) -> Result<Text, TextParseError> {
        let mut shape = Shape::try_parse_with_ctx(
            stream,
            &ShapeParseContext {
                is_shape_only: false,
                doc_ctx,
            },
        )?;

        let (mut flag_block, mut stream) = try_parse_object_header(stream, 2)?;

        let field_flags = flag_block.init_flex(&mut stream)?;

        unpack_field_flags!(field_flags, {
            1 => border_colour: stream.read_4_bytes()?;
            2 => border_width: stream.read_f32_le()?;
            3 => border_type: stream.read_u16_le()?.try_into()?;
        });

        shape.shape_data.border_colour = border_colour;
        shape.shape_data.border_width = border_width;
        shape.shape_data.border_type = border_type;

        flag_block.ensure_flags_used()?;
        stream.ensure_eof()?;

        Ok(Text { shape })
    }
}

impl HasObjectBase for Text {
    fn object_base(&self) -> &ObjectBase {
        self.shape.object_base()
    }
}
