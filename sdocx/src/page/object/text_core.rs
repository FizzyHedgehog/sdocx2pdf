use std::io::{self, Read, Seek};

use num::FromPrimitive;
use num_derive::FromPrimitive;
use thiserror::Error;

use crate::{
    bits::{CheckedBitfield, UnhandledBitsError},
    byte_stream::{
        BlindWindow, BoundedStream, ByteStreamLe, ReadStringError, TryParse,
        UnfinishedParsingError, Window,
    },
    context::{DocumentContext, TryParseWithContext},
    if_any_left, impl_try_from_for_optional_from,
    page::object::{DocObject, DocObjectParseContext, DocObjectParseError},
    read_u16_sized_vec, read_u32_sized_vec, unpack_bool_flags,
};

#[derive(Debug, FromPrimitive)]
enum Gravity {
    /// `GRAVITY_TOP`
    Top = 0,
    /// `GRAVITY_CENTER`
    Centre = 1,
    /// `GRAVITY_BOTTOM`
    Bottom = 2,
}

impl_try_from_for_optional_from!(Gravity, u8, from_u8, pub InvalidGravityError);

#[derive(Debug, FromPrimitive, Clone, Copy, PartialEq, Eq)]
pub enum SpanType {
    /// `TYPE_NONE`
    None = 0,
    /// `TYPE_FOREGROUND_COLOR`
    ForegroundColour = 1,
    /// `TYPE_FONT_SIZE`
    FontSize = 3,
    /// `TYPE_FONT_NAME`
    FontName = 4,
    /// `TYPE_BOLD`
    Bold = 5,
    /// `TYPE_ITALIC`
    Italic = 6,
    /// `TYPE_UNDERLINE`
    Underline = 7,
    /// `TYPE_HYPER_TEXT`
    Hypertext = 9,
    /// `TYPE_COMPOSING_BACKGROUND_COLOR`
    ComposingBackgroundColour = 15,
    /// `TYPE_COMPOSING`
    Composing = 16,
    /// `TYPE_BACKGROUND_COLOR`
    BackgroundColour = 17,
    /// `TYPE_COMPOSING_TAG`
    ComposingTag = 18,
    /// `TYPE_TIME_STAMP`
    TimeStamp = 19,
    /// `TYPE_STRIKETHROUGH`
    Strikethrough = 20,
    /// `TYPE_SUGGESTION`
    Suggestion = 21,
    /// `TYPE_SPELL_CORRECTION`
    SpellCorrection = 22,
    /// `TYPE_FORMULA`
    Formula = 23,
}

impl_try_from_for_optional_from!(SpanType, u32, from_u32, pub InvalidSpanTypeError);

#[derive(Debug, FromPrimitive)]
pub enum IntervalType {
    //// `SPAN_INCLUSIVE_EXCLUSIVE`
    InclusiveExclusive = 0,
    /// `SPAN_INCLUSIVE_INCLUSIVE`
    InclusiveInclusive = 1,
    /// `SPAN_EXCLUSIVE_EXCLUSIVE`
    ExclusiveExclusive = 2,
    /// `SPAN_EXCLUSIVE_INCLUSIVE`
    ExclusiveInclusive = 3,
}

impl_try_from_for_optional_from!(IntervalType, u32, from_u32, pub InvalidIntervalTypeError);

#[derive(Debug)]
pub struct SpanBase {
    pub span_type: SpanType,
    pub start_pos: u32,
    pub end_pos: u32,
    pub interval_type: IntervalType,
}

#[derive(Error, Debug)]
#[error(transparent)]
pub enum SpanParseError {
    Io(#[from] io::Error),
    SpanType(#[from] InvalidSpanTypeError),
    IntervalTypes(#[from] InvalidIntervalTypeError),

    #[error("data size {0} is too small")]
    BadSize(u16),
}

#[derive(Debug)]
pub struct Span {
    pub span_base: SpanBase,
    pub bytes: Vec<u8>,
}

impl<R: Read> TryParse<R> for Span {
    type ParseError = SpanParseError;

    fn try_parse(stream: &mut R) -> Result<Span, SpanParseError> {
        // First `u16` is the size of the span data that follows.
        let extra_data_size: usize = match stream.read_u16_le()? {
            // The `SpanBase` is 16 bytes.
            data_size @ 16.. => (data_size - 16).into(),
            data_size => return Err(SpanParseError::BadSize(data_size)),
        };

        Ok(Span {
            span_base: SpanBase {
                span_type: stream.read_u32_le()?.try_into()?,
                start_pos: stream.read_u32_le()?,
                end_pos: stream.read_u32_le()?,
                interval_type: stream.read_u32_le()?.try_into()?,
            },
            bytes: stream.read_u8s(extra_data_size)?,
        })
    }
}

/// A snapshot of the character-level formatting that applies at a given position in a text
/// object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    /// Font size in sp (i.e. the raw value stored in the document, before any page scaling).
    pub font_size: f32,

    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,

    /// Raw colour bytes as stored in the document (blue, green, red, alpha).
    pub colour: [u8; 4],
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_size: 15.0,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            colour: [37, 37, 37, 255],
        }
    }
}

/// The kind of marker that precedes a list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulletKind {
    /// A numbered item, e.g. `1.`, `2.`, ...
    Numbered,

    /// A symbolic item, e.g. `•`.
    Symbol,
}

/// List-item information for a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulletStyle {
    pub kind: BulletKind,

    /// The item number stored in the document, if any. Zero means "not recorded" (the renderer
    /// should continue its own running count).
    pub number: u32,

    /// Nesting level of the item (0 for top-level items).
    pub level: u32,
}

/// Reads the little-endian `u32` at `offset`, if the payload is long enough.
fn read_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let chunk = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
}

/// Whether a span's payload marks its feature as switched on. Span payloads start with a boolean
/// flag byte; the remaining bytes carry feature-specific data (for example, strikethrough payloads
/// are four bytes long and use the last byte for something else, so only the first byte can be
/// treated as the flag).
fn span_is_enabled(span: &Span) -> bool {
    span.bytes.first().is_some_and(|&b| b != 0)
}

impl BulletStyle {
    /// Parses the payload of a `TYPE_BULLET` paragraph.
    ///
    /// The payload consists of four little-endian `u32`s: the bullet type, the item number (only
    /// meaningful for numbered lists), a nesting level and an enabled flag. Bullet type `4` is a
    /// numbered list; everything else that we know of is a symbol bullet.
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let bullet_type = read_u32_at(bytes, 0)?;
        let number = read_u32_at(bytes, 4).unwrap_or(0);
        let level = read_u32_at(bytes, 8).unwrap_or(0);

        // Newer documents carry an explicit enabled flag; older ones just rely on the type.
        let enabled = match read_u32_at(bytes, 12) {
            Some(flag) => flag != 0,
            None => bullet_type != 0,
        };

        if bullet_type == 0 || !enabled {
            return None;
        }

        let kind = if bullet_type == 4 || number > 0 {
            BulletKind::Numbered
        } else {
            BulletKind::Symbol
        };

        Some(Self {
            kind,
            number,
            level,
        })
    }
}

#[derive(Debug, FromPrimitive, Clone, Copy, PartialEq, Eq)]
pub enum ParagraphType {
    /// `TYPE_INDENTLEVEL`
    IndentLevel = 2,
    /// `TYPE_ALIGN`
    Alignment = 3,
    /// `TYPE_LINE_SPACING`
    LineSpacing = 4,
    /// `TYPE_BULLET`
    Bullet = 5,
    /// `TYPE_PARSING_STATE`
    ParsingState = 6,
    // (Missing 7)
    /// `TYPE_SPACING_BEFORE`
    SpacingBefore = 8,
    /// `TYPE_SPACING_AFTER`
    SpacingAfter = 9,
    /// `TYPE_PREDEFINED_STYLE`
    PredefinedStyle,
}

impl_try_from_for_optional_from!(ParagraphType, u32, from_u32, pub InvalidParagraphTypeError);

/// Horizontal alignment of a paragraph, as stored in a `TYPE_ALIGN` paragraph attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParagraphAlignment {
    /// `ALIGN_LEFT`
    #[default]
    Left,

    /// `ALIGN_RIGHT`
    Right,

    /// `ALIGN_CENTER`
    Centre,
}

impl ParagraphAlignment {
    /// Maps the raw value stored in the document to an alignment. `0` is left, `1` is right and
    /// `2` is centre; anything else (including values we have never seen) is treated as left.
    const fn from_raw(value: u32) -> Self {
        match value {
            1 => Self::Right,
            2 => Self::Centre,
            _ => Self::Left,
        }
    }
}

#[derive(Debug)]
pub struct ParagraphBase {
    pub paragraph_type: ParagraphType,
    pub start_pos: u32,
    pub end_pos: u32,
}

#[derive(Error, Debug)]
#[error(transparent)]
pub enum ParagraphParseError {
    Io(#[from] io::Error),
    ParagraphType(#[from] InvalidParagraphTypeError),

    #[error("data size {0} is too small")]
    BadSize(u16),
}

#[derive(Debug)]
pub struct Paragraph {
    pub paragraph_base: ParagraphBase,
    pub bytes: Vec<u8>,
}

impl<R: Read> TryParse<R> for Paragraph {
    type ParseError = ParagraphParseError;

    fn try_parse(stream: &mut R) -> Result<Paragraph, ParagraphParseError> {
        // First `u16` is the size of the paragraph data that follows.
        let extra_data_size: usize = match stream.read_u16_le()? {
            // The `ParagraphBase` is 12 bytes.
            data_size @ 12.. => (data_size - 12).into(),
            data_size => return Err(ParagraphParseError::BadSize(data_size)),
        };

        Ok(Paragraph {
            paragraph_base: ParagraphBase {
                paragraph_type: stream.read_u32_le()?.try_into()?,
                start_pos: stream.read_u32_le()?,
                end_pos: stream.read_u32_le()?,
            },
            bytes: stream.read_u8s(extra_data_size)?,
        })
    }
}

#[derive(Debug, FromPrimitive, Clone, Copy, PartialEq, Eq)]
pub enum InTextLayoutOption {
    /// `LAYOUT_OPTION_BLOCK`
    Block = 0,
    /// `LAYOUT_OPTION_INLINE`
    Inline = 1,
    /// `LAYOUT_OPTION_BLOCK_WITH_MARGIN_SMALL`
    BlockSmallMargin = 2,
    /// `LAYOUT_OPTION_BLOCK_WITH_MARGIN_MEDIUM`
    BlockMediumMargin = 3,
}

impl_try_from_for_optional_from!(
    InTextLayoutOption,
    u32,
    from_u32,
    pub InvalidInTextLayoutOptionError,
);

#[derive(Debug, FromPrimitive, Clone, Copy, PartialEq, Eq)]
pub enum InTextLayoutConstraint {
    /// `LAYOUT_CONSTRAINT_NORMAL`
    Normal = 0,
    /// `LAYOUT_CONSTRAINT_OVER_PAGES_OVERLAP_PADDING`
    OverPagesOverlapPadding = 1,
    /// `LAYOUT_CONSTRAINT_OVER_PAGES`
    OverPages = 2,
}

impl_try_from_for_optional_from!(
    InTextLayoutConstraint,
    u32,
    from_u32,
    pub InvalidInTextLayoutConstraintError,
);

#[derive(Debug)]
#[allow(dead_code)]
pub struct InlineObject {
    pub index_in_text: u32,
    pub layout_option: Option<InTextLayoutOption>,
    pub layout_constraint: Option<InTextLayoutConstraint>,
    pub object: DocObject,
}

#[derive(Error, Debug)]
#[error(transparent)]
pub enum CommonParseError {
    Io(#[from] io::Error),

    #[error("failed to read main text string")]
    MainText(#[source] ReadStringError),

    #[error("element count {0} is too large")]
    TooManyElements(u32),

    Span(#[from] SpanParseError),
    Paragraph(#[from] ParagraphParseError),
    BadGravityType(#[from] InvalidGravityError),

    #[error("{0} is too big to be a valid object type")]
    ObjectTypeTooBig(u32),

    #[error("one or more inline object list flags were set but not checked")]
    UnhandledInlineObjectListFlags(#[source] UnhandledBitsError),

    // The object we were trying to parse could include text, in which case the error may contain a
    // `CommonParseError`. `Box` here is to avoid recursion.
    InlineObject(#[from] Box<DocObjectParseError>),

    BadLayoutOption(#[from] InvalidInTextLayoutOptionError),
    BadLayoutConstraint(#[from] InvalidInTextLayoutConstraintError),

    #[error("object span was unfinished")]
    ObjectSpanUnfinished(#[source] UnfinishedParsingError),

    #[error("object in span was unfinished")]
    ObjectInObjectSpanUnfinished(#[source] UnfinishedParsingError),

    #[error(transparent)]
    Unfinished(#[from] UnfinishedParsingError),
}

pub struct CommonParseContext<'fr, 'sr> {
    pub format_version: u32,
    pub doc_ctx: DocumentContext<'fr, 'sr>,
}

#[derive(Debug)]
#[expect(dead_code)]
pub struct Common {
    text: String,
    left_margin: f32,
    top_margin: f32,
    right_margin: f32,
    bottom_margin: f32,
    gravity: Gravity,

    spans: Vec<Span>,
    paragraphs: Vec<Paragraph>,
    section_data: Vec<(u32, u32)>,

    // a.k.a. "object spans", but they're not really spans.
    pub(crate) inline_objects: Vec<InlineObject>,
}

impl Common {
    pub fn raw_string(&self) -> &str {
        &self.text
    }

    pub fn section_data(&self) -> &[(u32, u32)] {
        &self.section_data
    }

    pub fn left_margin(&self) -> f32 {
        self.left_margin
    }

    pub fn top_margin(&self) -> f32 {
        self.top_margin
    }

    pub fn right_margin(&self) -> f32 {
        self.right_margin
    }

    pub fn bottom_margin(&self) -> f32 {
        self.bottom_margin
    }

    pub fn default_font_size(&self) -> Option<f32> {
        for span in &self.spans {
            if span.span_base.span_type == SpanType::FontSize && span.bytes.len() >= 4 {
                let val = f32::from_le_bytes([
                    span.bytes[0],
                    span.bytes[1],
                    span.bytes[2],
                    span.bytes[3],
                ]);
                if val > 0.0 {
                    return Some(val);
                }
            }
        }
        None
    }

    pub fn default_text_color(&self) -> Option<[u8; 4]> {
        for span in &self.spans {
            if span.span_base.span_type == SpanType::ForegroundColour && span.bytes.len() >= 4 {
                return Some([span.bytes[0], span.bytes[1], span.bytes[2], span.bytes[3]]);
            }
        }
        None
    }

    pub fn is_bold(&self) -> bool {
        self.spans
            .iter()
            .any(|s| s.span_base.span_type == SpanType::Bold)
    }

    pub fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn font_size_at(&self, pos: u32) -> f32 {
        let default_sz = self.default_font_size().unwrap_or(15.0);
        for span in &self.spans {
            if span.span_base.span_type == SpanType::FontSize {
                let start = span.span_base.start_pos;
                let end = span.span_base.end_pos;
                if pos >= start && pos < end && span.bytes.len() >= 4 {
                    let sz = f32::from_le_bytes([
                        span.bytes[0],
                        span.bytes[1],
                        span.bytes[2],
                        span.bytes[3],
                    ]);
                    if sz > 0.0 {
                        return sz;
                    }
                }
            }
        }
        default_sz
    }

    pub fn text_color_at(&self, pos: u32) -> [u8; 4] {
        let default_color = self.default_text_color().unwrap_or([37, 37, 37, 255]);
        for span in &self.spans {
            if span.span_base.span_type == SpanType::ForegroundColour {
                let start = span.span_base.start_pos;
                let end = span.span_base.end_pos;
                if pos >= start && pos < end && span.bytes.len() >= 4 {
                    return [span.bytes[0], span.bytes[1], span.bytes[2], span.bytes[3]];
                }
            }
        }
        default_color
    }

    pub fn is_bold_at(&self, pos: u32) -> bool {
        self.has_enabled_span_at(SpanType::Bold, pos)
    }

    pub fn is_italic_at(&self, pos: u32) -> bool {
        self.has_enabled_span_at(SpanType::Italic, pos)
    }

    pub fn is_underline_at(&self, pos: u32) -> bool {
        self.has_enabled_span_at(SpanType::Underline, pos)
    }

    pub fn is_strikethrough_at(&self, pos: u32) -> bool {
        self.has_enabled_span_at(SpanType::Strikethrough, pos)
    }

    /// Whether an enabled span of `span_type` covers `pos`. Spans are half-open (`[start, end)`),
    /// matching how Android-style span ranges behave.
    fn has_enabled_span_at(&self, span_type: SpanType, pos: u32) -> bool {
        self.spans.iter().any(|span| {
            span.span_base.span_type == span_type
                && pos >= span.span_base.start_pos
                && pos < span.span_base.end_pos
                && span_is_enabled(span)
        })
    }

    /// The full character-level formatting that applies at `pos`.
    pub fn style_at(&self, pos: u32) -> TextStyle {
        TextStyle {
            font_size: self.font_size_at(pos),
            bold: self.is_bold_at(pos),
            italic: self.is_italic_at(pos),
            underline: self.is_underline_at(pos),
            strikethrough: self.is_strikethrough_at(pos),
            colour: self.text_color_at(pos),
        }
    }

    /// Splits `[start, end)` into contiguous runs that share a single style. The returned offsets
    /// are UTF-16 positions, and each run extends up to the start of the next one (the final run
    /// ends at `end`).
    pub fn style_runs(&self, start: u32, end: u32) -> Vec<(u32, TextStyle)> {
        if end <= start {
            return vec![(start, self.style_at(start))];
        }

        let mut cuts = vec![start];

        for span in &self.spans {
            // Only these span types affect how a run is drawn.
            if !matches!(
                span.span_base.span_type,
                SpanType::Bold
                    | SpanType::Italic
                    | SpanType::Underline
                    | SpanType::Strikethrough
                    | SpanType::FontSize
                    | SpanType::ForegroundColour
            ) {
                continue;
            }

            let span_start = span.span_base.start_pos;
            let span_end = span.span_base.end_pos;

            if span_start > start && span_start < end {
                cuts.push(span_start);
            }

            if span_end > start && span_end < end {
                cuts.push(span_end);
            }
        }

        cuts.sort_unstable();
        cuts.dedup();

        cuts.iter().map(|&cut| (cut, self.style_at(cut))).collect()
    }

    /// The index of the paragraph (counting from zero) that contains the UTF-16 position `pos`.
    pub fn paragraph_index_at(&self, pos: u32) -> usize {
        let mut index = 0;
        let mut units = 0u32;

        for ch in self.text.chars() {
            if units >= pos {
                break;
            }

            units += ch.len_utf16() as u32;

            if ch == '\n' {
                index += 1;
            }
        }

        index
    }

    /// The UTF-16 position at which the paragraph with `paragraph_index` starts.
    pub fn paragraph_start_at(&self, paragraph_index: usize) -> u32 {
        let mut index = 0;
        let mut units = 0u32;

        for ch in self.text.chars() {
            if index == paragraph_index {
                return units;
            }
            units += ch.len_utf16() as u32;
            if ch == '\n' {
                index += 1;
            }
        }
        units
    }

    /// The indentation level of the paragraph at `paragraph_index`.
    ///
    /// Note that paragraph attributes are keyed by *paragraph* index (the number of newlines
    /// before the paragraph), not by character position: a `TYPE_INDENTLEVEL` record spans a run
    /// of paragraphs.
    pub fn indent_level_at_paragraph(&self, paragraph_index: usize) -> u32 {
        self.paragraphs
            .iter()
            .find(|paragraph| {
                paragraph.paragraph_base.paragraph_type == ParagraphType::IndentLevel
                    && paragraph_covers(&paragraph.paragraph_base, paragraph_index)
            })
            .and_then(|paragraph| {
                // The level is the first `u32` of the payload; some documents store the level in
                // the second slot instead, so fall back to that if the first is zero.
                match read_u32_at(&paragraph.bytes, 0) {
                    Some(level) if level != 0 => Some(level),
                    _ => read_u32_at(&paragraph.bytes, 4),
                }
            })
            .unwrap_or(0)
    }

    /// List-item information for the paragraph at `paragraph_index`, if it is a list item.
    pub fn bullet_at_paragraph(&self, paragraph_index: usize) -> Option<BulletStyle> {
        self.paragraphs
            .iter()
            .filter(|paragraph| {
                paragraph.paragraph_base.paragraph_type == ParagraphType::Bullet
                    && paragraph_covers(&paragraph.paragraph_base, paragraph_index)
            })
            .find_map(|paragraph| BulletStyle::from_bytes(&paragraph.bytes))
    }

    /// The horizontal alignment of the paragraph at `paragraph_index`.
    ///
    /// Like the other paragraph attributes, a `TYPE_ALIGN` record covers a *run of paragraphs*
    /// (`start_pos..end_pos` are paragraph indices), so the record has to be matched against the
    /// paragraph index rather than against a character position. Records are written only where
    /// the alignment differs from the default, and the last record that covers a paragraph wins.
    pub fn alignment_at_paragraph(&self, paragraph_index: usize) -> ParagraphAlignment {
        self.paragraphs
            .iter()
            .filter(|paragraph| {
                paragraph.paragraph_base.paragraph_type == ParagraphType::Alignment
                    && paragraph_covers(&paragraph.paragraph_base, paragraph_index)
            })
            .max_by_key(|paragraph| paragraph.paragraph_base.start_pos)
            .and_then(|paragraph| read_u32_at(&paragraph.bytes, 0))
            .map(ParagraphAlignment::from_raw)
            .unwrap_or_default()
    }
}

/// Whether a paragraph attribute applies to the paragraph at `paragraph_index`.
fn paragraph_covers(base: &ParagraphBase, paragraph_index: usize) -> bool {
    let index = paragraph_index as u32;
    index >= base.start_pos && index < base.end_pos.max(base.start_pos + 1)
}

impl<'a, R: Read + Seek> TryParseWithContext<R, CommonParseContext<'a, 'a>> for Common {
    type ParseError = CommonParseError;

    fn try_parse_with_ctx(
        stream: &mut R,
        &CommonParseContext {
            format_version,
            doc_ctx,
        }: &CommonParseContext<'a, 'a>,
    ) -> Result<Common, CommonParseError> {
        let mut stream = stream.take_exclusive_length_prefixed()?;

        let text = stream
            .read_long_u16_string()
            .map_err(CommonParseError::MainText)?;

        let spans = read_u32_sized_vec!(
            stream,
            CommonParseError::TooManyElements,
            Span::try_parse(&mut stream)?
        );

        let paragraphs = read_u32_sized_vec!(
            stream,
            CommonParseError::TooManyElements,
            Paragraph::try_parse(&mut stream)?
        );

        let (left_margin, top_margin, right_margin, bottom_margin) = (
            stream.read_f32_le()?,
            stream.read_f32_le()?,
            stream.read_f32_le()?,
            stream.read_f32_le()?,
        );

        let gravity: Gravity = stream.read_u8()?.try_into()?;

        let section_data =
            read_u16_sized_vec!(stream, (stream.read_u32_le()?, stream.read_u32_le()?));

        let inline_objects = if format_version >= 2035 && {
            let mut flags = CheckedBitfield::from(stream.read_u32_le()?);

            unpack_bool_flags!(flags, {
                0 => any_present;
            });

            flags
                .ensure_none_set_unchecked()
                .map_err(CommonParseError::UnhandledInlineObjectListFlags)?;

            // It's 64 bits really, so read a second 32-bit bitfield. None of the bits should be
            // set.
            CheckedBitfield::from(stream.read_u32_le()?)
                .ensure_none_set_unchecked()
                .map_err(CommonParseError::UnhandledInlineObjectListFlags)?;

            any_present
        } {
            read_u32_sized_vec!(stream, CommonParseError::TooManyElements, {
                let mut obj_stream = (&mut stream).take_exclusive_length_prefixed()?;

                let obj_size = obj_stream.read_u32_le()?;

                let object_type: u8 = {
                    // This could be a single byte...
                    let object_type = obj_stream.read_u32_le()?;
                    object_type
                        .try_into()
                        .map_err(|_| CommonParseError::ObjectTypeTooBig(object_type))?
                };

                let object = {
                    let mut inner_stream: BlindWindow<_> =
                        Window::new(&mut obj_stream, obj_size.into()).into();

                    let obj = DocObject::try_parse_with_ctx(
                        &mut inner_stream,
                        &DocObjectParseContext {
                            object_type,
                            doc_ctx,
                        },
                    )
                    .map_err(Box::new)?;

                    inner_stream
                        .ensure_eof()
                        .map_err(CommonParseError::ObjectSpanUnfinished)?;

                    obj
                };

                let span = InlineObject {
                    index_in_text: obj_stream.read_u32_le()?,
                    layout_option: if_any_left!(obj_stream, obj_stream.read_u32_le()?.try_into()?),
                    layout_constraint: if_any_left!(
                        obj_stream,
                        obj_stream.read_u32_le()?.try_into()?
                    ),
                    object,
                };

                obj_stream
                    .ensure_eof()
                    .map_err(CommonParseError::ObjectSpanUnfinished)?;

                span
            })
        } else {
            vec![]
        };

        stream.ensure_eof()?;

        Ok(Common {
            text,
            left_margin,
            top_margin,
            right_margin,
            bottom_margin,
            gravity,
            spans,
            paragraphs,
            section_data,
            inline_objects,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(span_type: SpanType, start_pos: u32, end_pos: u32, bytes: Vec<u8>) -> Span {
        Span {
            span_base: SpanBase {
                span_type,
                start_pos,
                end_pos,
                interval_type: IntervalType::InclusiveInclusive,
            },
            bytes,
        }
    }

    #[test]
    fn span_flag_is_read_from_first_word() {
        assert!(span_is_enabled(&span(SpanType::Bold, 0, 1, vec![1, 0, 0, 0])));
        assert!(!span_is_enabled(&span(SpanType::Bold, 0, 1, vec![0, 0, 0, 0])));

        // Strikethrough payloads are four bytes long, with an extra trailing byte.
        assert!(span_is_enabled(&span(
            SpanType::Strikethrough,
            0,
            1,
            vec![1, 0, 0, 80]
        )));
        assert!(!span_is_enabled(&span(
            SpanType::Strikethrough,
            0,
            1,
            vec![0, 0, 0, 80]
        )));
    }

    #[test]
    fn numbered_bullet_is_parsed() {
        let bytes = [4, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
        let bullet = BulletStyle::from_bytes(&bytes).expect("bullet should parse");

        assert_eq!(bullet.kind, BulletKind::Numbered);
        assert_eq!(bullet.number, 3);
        assert_eq!(bullet.level, 0);
    }

    #[test]
    fn symbol_bullet_is_parsed() {
        let bytes = [8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
        let bullet = BulletStyle::from_bytes(&bytes).expect("bullet should parse");

        assert_eq!(bullet.kind, BulletKind::Symbol);
        assert_eq!(bullet.number, 0);
    }

    #[test]
    fn disabled_or_missing_bullet_is_ignored() {
        // Explicitly disabled.
        let disabled = [4, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(BulletStyle::from_bytes(&disabled), None);

        // Type zero means "no bullet".
        assert_eq!(BulletStyle::from_bytes(&[0; 16]), None);

        // Too short to contain a type.
        assert_eq!(BulletStyle::from_bytes(&[]), None);
    }

    #[test]
    fn indent_level_uses_second_word_as_fallback() {
        // First word is zero, so the second word carries the level.
        assert_eq!(read_u32_at(&[0, 0, 0, 0, 2, 0, 0, 0], 0), Some(0));
        assert_eq!(read_u32_at(&[0, 0, 0, 0, 2, 0, 0, 0], 4), Some(2));
    }

    fn paragraph(paragraph_type: ParagraphType, start_pos: u32, end_pos: u32) -> Paragraph {
        Paragraph {
            paragraph_base: ParagraphBase {
                paragraph_type,
                start_pos,
                end_pos,
            },
            bytes: vec![],
        }
    }

    fn alignment(start_pos: u32, end_pos: u32, value: u32) -> Paragraph {
        Paragraph {
            paragraph_base: ParagraphBase {
                paragraph_type: ParagraphType::Alignment,
                start_pos,
                end_pos,
            },
            bytes: value.to_le_bytes().to_vec(),
        }
    }

    fn common_with(paragraphs: Vec<Paragraph>) -> Common {
        Common {
            text: String::new(),
            left_margin: 0.0,
            top_margin: 0.0,
            right_margin: 0.0,
            bottom_margin: 0.0,
            gravity: Gravity::Top,
            spans: vec![],
            paragraphs,
            section_data: vec![],
            inline_objects: vec![],
        }
    }

    #[test]
    fn alignment_values_map_to_the_documented_meanings() {
        // `1` is right and `2` is centre. Unknown values fall back to left.
        assert_eq!(ParagraphAlignment::from_raw(0), ParagraphAlignment::Left);
        assert_eq!(ParagraphAlignment::from_raw(1), ParagraphAlignment::Right);
        assert_eq!(ParagraphAlignment::from_raw(2), ParagraphAlignment::Centre);
        assert_eq!(ParagraphAlignment::from_raw(9), ParagraphAlignment::Left);
    }

    #[test]
    fn alignment_is_looked_up_by_paragraph_index() {
        // Mirrors the shape of a real note: the centre/right runs start deep into the note
        // (paragraph 170), far beyond the character position of the paragraph they belong to.
        let common = common_with(vec![
            paragraph(ParagraphType::ParsingState, 0, 1),
            alignment(0, 1, 0),
            alignment(170, 176, 2),
            alignment(176, 183, 1),
            alignment(187, 188, 0),
        ]);

        assert_eq!(common.alignment_at_paragraph(0), ParagraphAlignment::Left);
        assert_eq!(common.alignment_at_paragraph(169), ParagraphAlignment::Left);

        // The centre run covers paragraphs 170..176 (half-open).
        assert_eq!(common.alignment_at_paragraph(170), ParagraphAlignment::Centre);
        assert_eq!(common.alignment_at_paragraph(175), ParagraphAlignment::Centre);

        // The right run covers 176..183.
        assert_eq!(common.alignment_at_paragraph(176), ParagraphAlignment::Right);
        assert_eq!(common.alignment_at_paragraph(182), ParagraphAlignment::Right);

        // Past the last record the alignment is back to the default.
        assert_eq!(common.alignment_at_paragraph(183), ParagraphAlignment::Left);
        assert_eq!(common.alignment_at_paragraph(187), ParagraphAlignment::Left);
        assert_eq!(common.alignment_at_paragraph(500), ParagraphAlignment::Left);
    }

    #[test]
    fn later_alignment_record_wins_when_runs_overlap() {
        let common = common_with(vec![alignment(5, 20, 2), alignment(8, 12, 1)]);

        assert_eq!(common.alignment_at_paragraph(5), ParagraphAlignment::Centre);
        assert_eq!(common.alignment_at_paragraph(8), ParagraphAlignment::Right);
        assert_eq!(common.alignment_at_paragraph(12), ParagraphAlignment::Centre);
    }

    #[test]
    fn alignment_record_without_payload_is_ignored() {
        let common = common_with(vec![paragraph(ParagraphType::Alignment, 3, 4)]);

        assert_eq!(common.alignment_at_paragraph(3), ParagraphAlignment::Left);
    }
}
