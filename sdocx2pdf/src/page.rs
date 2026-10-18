use std::{
    collections::{BTreeSet, HashMap, hash_map::Entry},
    io::Read,
    sync::{Arc, OnceLock},
};

use anyhow::Context;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use itertools::{Either, Itertools};
use krilla::{
    color::rgb,
    geom::{PathBuilder, Point, Rect, Transform},
    num::NormalizedF32,
    paint::Fill,
    surface::Surface,
    text::{Font, TextDirection},
};
use log::{error, info, warn};
use num::ToPrimitive;
use ordered_float::OrderedFloat;
use sdocx::{
    DocObject, MediaStorage, euclid,
    page::{
        PdfPage as EmbeddedPdfPage,
        object::{BulletKind, BulletStyle, InlineObject, TextStyle},
    },
};
use thiserror::Error;

use crate::{
    BasicSplitMode, pdf,
    shape::{NoStyleError, PathDrawingError, ShapeDrawingCtx},
    tool::{EventGroup, Tool},
};

/// Width of the pages that Samsung Notes produces when it exports a note to PDF.
///
/// Samsung uses 600x848 rather than exact ISO A4 (595.28x841.89). Matching its choice means our
/// output can be compared directly with Samsung's own exports, which is useful when checking
/// rendering fidelity.
const SAMSUNG_PAGE_WIDTH_PT: f64 = 600.0;

/// Height of the pages that Samsung Notes produces when it exports a note to PDF.
const SAMSUNG_PAGE_HEIGHT_PT: f64 = 848.0;

/// Height-to-width ratio of Samsung Notes' exported pages (848/600, very close to √2).
const SAMSUNG_PAGE_ASPECT: f64 = SAMSUNG_PAGE_HEIGHT_PT / SAMSUNG_PAGE_WIDTH_PT;

static NANUM_REGULAR_BYTES: &[u8] = include_bytes!("../fonts/NanumBarunGothic-Regular.ttf");
static NANUM_BOLD_BYTES: &[u8] = include_bytes!("../fonts/NanumBarunGothic-Bold.ttf");

pub struct LoadedFont {
    pub font: Font,
    pub bytes: Arc<Vec<u8>>,
}

static LOADED_REGULAR: OnceLock<LoadedFont> = OnceLock::new();
static LOADED_BOLD: OnceLock<LoadedFont> = OnceLock::new();
static METRICS_REGULAR: OnceLock<FontMetrics<'static>> = OnceLock::new();
static METRICS_BOLD: OnceLock<FontMetrics<'static>> = OnceLock::new();

/// Glyph metrics for one of the embedded fonts, parsed once and reused.
fn cached_metrics(is_bold: bool) -> &'static FontMetrics<'static> {
    let lock = if is_bold {
        &METRICS_BOLD
    } else {
        &METRICS_REGULAR
    };

    lock.get_or_init(|| {
        FontMetrics::new(if is_bold {
            NANUM_BOLD_BYTES
        } else {
            NANUM_REGULAR_BYTES
        })
    })
}

pub fn get_loaded_font(is_bold: bool) -> &'static LoadedFont {
    let font_lock = if is_bold { &LOADED_BOLD } else { &LOADED_REGULAR };
    font_lock.get_or_init(|| {
        let embedded_bytes = if is_bold {
            NANUM_BOLD_BYTES
        } else {
            NANUM_REGULAR_BYTES
        };
        let arc_bytes = Arc::new(embedded_bytes.to_vec());
        let font = Font::new(arc_bytes.clone().into(), 0).expect("embedded NanumBarunGothic must load");
        LoadedFont {
            font,
            bytes: arc_bytes,
        }
    })
}

pub fn get_font(is_bold: bool) -> Font {
    get_loaded_font(is_bold).font.clone()
}

/// Stores loaded PDFs for embeds.
pub struct EmbedMap<'e> {
    /// Maps file names to loaded PDFs.
    pdfs: HashMap<&'e str, krilla::pdf::Pdf>,
}

impl<'e> EmbedMap<'e> {
    pub fn new(document: &'e sdocx::Document, media: &mut MediaStorage) -> anyhow::Result<Self> {
        let mut pdfs = HashMap::new();

        for embed in document
            .pages()
            .iter()
            .flat_map(|page| page.embedded_pdf_pages())
        {
            let emb_pdf_name = embed.file().name();

            let pdf = &*match pdfs.entry(emb_pdf_name) {
                Entry::Occupied(occ) => occ.into_mut(),
                Entry::Vacant(vac) => {
                    let mut pdf_bytes = Vec::new();

                    media
                        .open_file(emb_pdf_name)
                        .with_context(|| format!("failed to open embedded PDF '{emb_pdf_name}'"))?
                        .read_to_end(&mut pdf_bytes)
                        .with_context(|| format!("failed to read embedded PDF '{emb_pdf_name}'"))?;

                    let pdf = krilla::pdf::Pdf::new(pdf_bytes).map_err(|e| {
                        anyhow::anyhow!("failed to parse embedded PDF '{emb_pdf_name}': {e:?}")
                    })?;

                    match pdf
                        .metadata()
                        .title
                        .as_ref()
                        .and_then(|v| str::from_utf8(v).ok())
                    {
                        Some(title) if !title.is_empty() => {
                            info!("Loaded embedded PDF '{emb_pdf_name}' ('{title}')")
                        }
                        _ => info!("Loaded embedded PDF '{emb_pdf_name}'"),
                    }

                    vac.insert(pdf)
                }
            };

            // Ensure that the PDF for this embed actually has the requisite page.
            anyhow::ensure!(
                (embed.page_index() as usize) < pdf.pages().len(),
                "PDF '{emb_pdf_name}' is loaded, but does not have page {}",
                embed.page_index()
            );
        }

        Ok(Self { pdfs })
    }

    fn get_pdf(&self, embed: &'e EmbeddedPdfPage) -> &krilla::pdf::Pdf {
        self.pdfs
            .get(embed.file().name())
            .expect("embed wasn't loaded")
    }

    fn get_crop_box(&self, embed: &'e EmbeddedPdfPage) -> pdf::Box2d {
        let rect = self
            .get_pdf(embed)
            .pages()
            .get(embed.page_index() as usize)
            // Unwrapping is fine here because `new` checks that every embed's page exists.
            .unwrap()
            .crop_box();

        pdf::Box2d::new((rect.x0, rect.y0).into(), (rect.x1, rect.y1).into())
    }

    pub fn into_documents(self) -> HashMap<&'e str, krilla::pdf::PdfDocument> {
        self.pdfs
            .into_iter()
            .map(|(name, pdf)| (name, krilla::pdf::PdfDocument::new(Arc::new(pdf))))
            .collect()
    }
}

trait HasBoundingBox {
    fn bounding_box(&self) -> sdocx::Box2d<f64>;
}

impl HasBoundingBox for DocObject {
    fn bounding_box(&self) -> sdocx::Box2d<f64> {
        self.object_base().rect
    }
}

impl HasBoundingBox for InlineObject {
    fn bounding_box(&self) -> sdocx::Box2d<f64> {
        self.object.bounding_box()
    }
}

impl HasBoundingBox for EmbeddedPdfPage {
    fn bounding_box(&self) -> sdocx::Box2d<f64> {
        self.rect()
    }
}

/// Takes an iterable of input pages (page box + iterable of embedded PDF page crop boxes and
/// destination boxes) and returns a scale to use across all the pages to scale the SDOCX page
/// sizes to PDF page sizes. Returns `None` iff `pages` is empty.
fn compute_uniform_scale<Pages, Embeds>(
    pages: Pages,
) -> Option<euclid::Scale<f64, sdocx::SdocxSpace, pdf::PdfSpace>>
where
    Pages: IntoIterator<Item = (sdocx::Box2d<f64>, Embeds)>,
    Embeds: IntoIterator<Item = (pdf::Box2d, sdocx::Box2d<f64>)>,
{
    // Terminology:
    //  - Input page: A page from an SDOCX document
    //  - Host page: An input page that embeds one or more pages from a PDF document
    //  - Embed: An SDOCX object that renders a page from a PDF in a rectangle on a host page
    //  - Source page: The PDF page in an embed
    //  - Destination box: The rectangle on the host page inside which the source page is rendered
    //  - Output page: A page in the PDF document we are creating
    //
    // The goal here is to find a single scale factor to use to derive the output page sizes from
    // the input page sizes. The output page sizes are physical (PDF) sizes; the input page sizes
    // are in SDOCX units, so they are not physical. We have to try to bridge that gap by assigning
    // a physical meaning to the SDOCX units in the context of this document.
    //
    // If there are embeds in the document, their source pages' crop boxes are transformed to fit
    // in their destination boxes. This is a mapping from PDF-space to SDOCX-space. If we obtain
    // the size of the output page by applying the inverse of this transformation's scale to the
    // size of the input page, the source page will appear on the output page at the size it has in
    // its source document. This is a desirable property because it means that if the destination
    // boxes are exactly the same sizes as the host pages (which is what happens if the input
    // document was created by importing a PDF to annotate), each page of the output PDF containing
    // a source page will be exactly the same size as that source page. In other words, an SDOCX
    // file created by importing an {A1, A2, A3, A4, A5, A6, US Letter, ...} PDF and annotating it
    // will be converted into another {A1, A2, A3, A4, A5, A6, US Letter, ...} PDF.
    //
    // There are some issues we have to consider when implementing this approach:
    //  1. There may be no embeds in the whole document, in which case we have no direct link
    //     between PDF units and SDOCX units.
    //  2. There may be many embeds in the whole document, in which case applying the logic
    //     described above to all embeds could give many conflicting opinions on what the scale
    //     should be.
    //  3. It might not be sensible to scale everything up so that source pages end up at their
    //     real size in the output. For example, if an embed is used as a graphic, the size of the
    //     destination box will likely have little to do with the size of the source page, so we
    //     shouldn't treat them as related.
    //  4. The input page size -> output page size mapping must preserve aspect ratio, so an embed
    //     with a difference between the aspect ratios of the source page and the destination box
    //     is not helpful. (I haven't actually seen this happen, but it's technically possible
    //     within the format.)
    //
    // We implement the following solutions to these problems:
    //  1. If there are no useful embeds (see (3) and (4) below) in the whole document, find the
    //     modal input page size (breaking ties by earliest first appearance) and apply a scale
    //     that takes the smaller dimension of this modal size to the width of an A4 page. Two very
    //     simple examples: If every input page is the same size and portrait, then every output
    //     page will be A4 portrait. If every input page is the same size and landscape, then every
    //     output page will be A4 landscape. OTOH, if the sizes/orientations are mixed, we do a
    //     best-effort attempt to get the most common page size as A4-ish as we can while retaining
    //     aspect ratio.
    //  2. Consider all useful embeds (see below) individually. Break ties by earliest appearance,
    //     first by page index and then by index in the array of embeds if comparing two embeds
    //     from the same host page. Of the useful embeds, choose the best per the criteria
    //     described in (3). If we have a clear winner, then we have no problem; if we have more
    //     than one winner, we achieve a stable result by way of the tiebreak, regardless of
    //     whether the candidates come from the same or different host pages.
    //  3. Ignore any embed whose destination box is not comparable in size and shape to its host
    //     page. That is, if the destination box and host page have very different aspect ratios or
    //     areas, ignore the embed. When looking for winners amongst embeds that pass this test and
    //     the one in (4), sort first by smallest difference in area between the destination box
    //     and the host page (for a better size match) and then by most similar aspect ratio
    //     (looking for near-exact matches).
    //  4. Consider only embeds whose source pages and destination boxes have the same aspect ratio
    //     (up to a very small tolerance). This will normally include all embeds, because the
    //     destination box usually has the same aspect ratio as the source page, but it means we
    //     can get a uniform scale by taking either source page width / destination box width or
    //     source page height / destination box height (equivalently, since we know them to be
    //     almost or exactly equal).

    // If there are no useful embeds, we'll try to match the modal page size to A4; for that, we
    // need to count the number of times we see each page size. We also store the index at which we
    // first found each page size so we can choose the earlier size in the case of a tie.
    let mut page_size_occurrences: HashMap<(OrderedFloat<f64>, OrderedFloat<f64>), (u32, usize)> =
        HashMap::new();

    fn aspect_ratio_factor(a: f64, b: f64) -> f64 {
        // This is the factor by which one box is wider (relative to its height) than the other. No
        // need for `abs` because we must have `a >= b` or `a <= b`, so the max here will be at
        // least 1.
        (a / b).max(b / a) - 1.0
    }

    let scale_candidates = pages
        .into_iter()
        .enumerate()
        .flat_map(|(page_i, (ipb, embeds))| {
            // Save an indentation level:
            let input_page_box = ipb;

            // Update the occurrence count for this size. If it's the first occurrence, record the
            // page index.
            page_size_occurrences
                .entry((
                    input_page_box.width().into(),
                    input_page_box.height().into(),
                ))
                .and_modify(|(count, _)| *count += 1)
                .or_insert((1, page_i));

            let host_aspect_ratio = input_page_box.width() / input_page_box.height();
            let host_area = input_page_box.area();

            let extract_candidates = move |(src_box, dest_box): (pdf::Box2d, sdocx::Box2d<f64>)| {
                let dest_aspect_ratio = dest_box.width() / dest_box.height();
                let dest_area = dest_box.area();

                let dest_host_ar_factor = aspect_ratio_factor(dest_aspect_ratio, host_aspect_ratio);

                if dest_host_ar_factor > 0.1 {
                    // Rule 3: One box is more than 10% wider relative to its height than the
                    // other, so the destination box and host page disagree on shape.
                    return None;
                }

                let dest_host_area_abs_rel_diff = (dest_area - host_area).abs() / host_area;

                if dest_host_area_abs_rel_diff > 0.35 {
                    // Rule 3: The magnitude of the relative difference between the areas of the
                    // destination box and host page is greater than 35%, so the destination box
                    // does not fill the host page well (it is too big or too small).
                    return None;
                }

                let src_aspect_ratio = src_box.width() / src_box.height();
                let dest_src_ar_factor = aspect_ratio_factor(dest_aspect_ratio, src_aspect_ratio);

                if dest_src_ar_factor > 0.01 {
                    // Rule 4: Dest/source box is more than 1% wider relative to its height than
                    // the other.
                    return None;
                }

                // Per (4), we can take width/width or height/height and it makes little
                // difference.
                let proposed_scale =
                    pdf::Length::new(src_box.height()) / sdocx::Length::new(dest_box.height());

                Some((
                    proposed_scale,
                    // Sort key, per (3):
                    (
                        OrderedFloat(dest_host_area_abs_rel_diff),
                        OrderedFloat(dest_host_ar_factor),
                    ),
                ))
            };

            embeds.into_iter().filter_map(extract_candidates)
        });

    // Find the best scale candidate according to (3). `min_by_key` retains an earlier minimum if
    // an equal element is found later, so this respects the tiebreak rules in (2).
    match scale_candidates.min_by_key(|(_ps, key)| *key) {
        Some((scale, _sort_key)) => Some(scale),
        None => {
            // We didn't find a sensible scale by inspecting embeds, so use the modal page size (if
            // there were any pages at all).
            let ((w, h), _) = page_size_occurrences
                .into_iter()
                // Maximum count with minimum first-seen index.
                .max_by_key(|(_, (count, first_seen_index))| (*count, !*first_seen_index))?;

            // Use a scale that maps the smaller dimension of the modal page size to the width of
            // a Samsung Notes export page.
            Some(pdf::Length::new(SAMSUNG_PAGE_WIDTH_PT) / sdocx::Length::new(w.min(h).0))
        }
    }
}

/// Returns an iterator yielding the vertical positions of the page breaks that should be used to
/// split `page` according to the given splitting options. Each pair of adjacent break positions in
/// the returned iterator forms a single page slice. The returned iterator is guaranteed to yield
/// at least two elements.
fn compute_page_breaks(
    page: &sdocx::page::Page,
    auto_split: bool,
    basic_split_mode: Option<BasicSplitMode>,
) -> impl Iterator<Item = f64> {
    let page_height = page.height as f64;

    if !auto_split || page.embedded_pdf_pages().is_empty() {
        // Auto-splitting wasn't requested, or is impossible because there are no embeds.
        return Either::Left(match basic_split_mode {
            // No basic split mode set, so use the whole page.
            None => Either::Left([0.0, page_height].into_iter()),

            Some(bsm) => {
                let output_page_inv_aspect_ratio = match bsm {
                    // Portrait pages, as tall as they are wide times the Samsung page aspect.
                    BasicSplitMode::IsoPortrait => SAMSUNG_PAGE_ASPECT,
                    // Landscape pages: the inverse.
                    BasicSplitMode::IsoLandscape => 1.0 / SAMSUNG_PAGE_ASPECT,
                };

                // Pageless documents are vertical, so the height is arbitrary. Use the width and
                // the page aspect ratio to determine how tall the page slices should be. With the
                // scale that maps the document width to `SAMSUNG_PAGE_WIDTH_PT`, each slice comes
                // out at exactly 600x848 (or 848x600 for landscape).
                let slice_height = (page.width as f64) * output_page_inv_aspect_ratio;

                // Round the slice count up so that if the slice height doesn't perfectly divide
                // the height of the page, we extend the page to add a bit of blank space in the
                // final slice rather than dropping the final slice and potentially some page
                // content.
                let slice_count: u32 = (page_height / slice_height).ceil().to_u32().unwrap();

                // Inclusive range so that we get the page breaks at the top and bottom of the
                // document.
                Either::Right((0..=slice_count).map(move |i| slice_height * (i as f64)))
            }
        });
    }

    // Use a set so we can ignore exact duplicates; use a B-tree set so we can keep the order of
    // the breaks.
    let mut proposed_page_breaks = BTreeSet::from([
        // Always start a new page at the beginning of the document, and finish a page at the end
        // of the document.
        OrderedFloat(0.0),
        OrderedFloat(page_height),
    ]);

    for embed in page.embedded_pdf_pages() {
        let min_y = embed.rect().min.y;
        let max_y = embed.rect().max.y;

        // Add page breaks before and after this embed.
        proposed_page_breaks.extend([OrderedFloat(min_y), OrderedFloat(max_y)]);
    }

    let minimum_page_height = {
        // Find the median height of the pages that would be created by our breaks if we used all
        // of them, and use that median to obtain a minimum allowed page height. We have to do this
        // rather than setting some constant threshold because we don't know the scale of the page
        // height units.
        let mut proposed_page_heights = proposed_page_breaks
            .iter()
            .tuple_windows()
            .map(|(a, b)| b - a)
            // Collect and sort. This is not the most efficient way to find the median, but it's
            // fine for this.
            .collect_vec();
        proposed_page_heights.sort_unstable();

        // Indexing directly is fine here because the vector can't be empty, so the floored result
        // of halving the length is guaranteed to be a valid index.
        let median_page_height = proposed_page_heights[proposed_page_heights.len() / 2].0;

        // Only permit pages that are no shorter than 5% of the median page height.
        median_page_height * 0.05
    };

    Either::Right(
        proposed_page_breaks
            .into_iter()
            .map(OrderedFloat::into_inner)
            .with_position()
            // Manipulate adjacent page breaks to avoid having any pages below the minimum height.
            // We have total freedom with how we place page breaks within the page, but we can't
            // move or ignore the breaks at the beginning and end of the page.
            .coalesce(
                move |earlier @ (earlier_pos, earlier_break), later @ (later_pos, later_break)| {
                    if later_break - earlier_break >= minimum_page_height {
                        // Page height is fine. Keep both breaks.
                        return Err((earlier, later));
                    }

                    use itertools::Position::{First, Last, Middle};

                    // We can't get here if there is only one page, because that page's height
                    // would be the median height, so can't be below the threshold. Therefore,
                    // neither position is Only. The pair can't be (First, First) or (Last, Last),
                    // because we're looking at distinct breaks. It can't be (First, Last) because
                    // that would mean we'd dropped all breaks between the start and end of the
                    // document and then decided (per the above condition) that the document height
                    // is much less than the median page height, which is impossible.
                    match (earlier_pos, later_pos) {
                        (First, Middle) => {
                            // We can't drop the earlier break because that's the break at the
                            // start of the document. The later break isn't the one at the end of
                            // the document, so we can drop it.
                            Ok(earlier)
                        }

                        (Middle, Middle) => {
                            // Take the midpoint of the two breaks, replacing both.
                            Ok((Middle, earlier_break.midpoint(later_break)))
                        }

                        (Middle, Last) => {
                            // We can't drop the later break because it's the one at the end of the
                            // document, so drop the earlier one.
                            Ok(later)
                        }

                        // As described above, we shouldn't get here, but just in case:
                        _wtf => Err((earlier, later)),
                    }
                },
            )
            .map(|(_, page_break)| page_break),
    )
}

pub struct PageCapture<'p> {
    /// The rectangle that was used to capture the parent page's content.
    capture_box: sdocx::Box2d<f64>,

    /// The background colour of the parent page, if there is one.
    background_colour: Option<[u8; 4]>,

    /// The background image ID of the parent page, if there is one.
    background_image_id: Option<i32>,

    /// The embedded PDF pages captured.
    embeds: Vec<&'p EmbeddedPdfPage>,

    /// The normal objects captured.
    objects: Vec<&'p DocObject>,

    /// The inline objects captured.
    inline_objects: Vec<&'p InlineObject>,
}

pub enum PageContent<'p> {
    Slice(
        PageCapture<'p>,
        euclid::Scale<f64, sdocx::SdocxSpace, pdf::PdfSpace>,
    ),
    WholePage {
        page: &'p sdocx::page::Page,
        inline_objects: Vec<&'p InlineObject>,
        scale: euclid::Scale<f64, sdocx::SdocxSpace, pdf::PdfSpace>,
    },
}

impl<'p> PageContent<'p> {
    fn input_box(&self) -> sdocx::Box2d<f64> {
        match self {
            PageContent::Slice(page_capture, ..) => page_capture.capture_box,
            PageContent::WholePage { page, .. } => {
                sdocx::Box2d::from_size(sdocx::Size2d::new(page.width as f64, page.height as f64))
            }
        }
    }

    fn input_to_output_scale(&self) -> euclid::Scale<f64, sdocx::SdocxSpace, pdf::PdfSpace> {
        match self {
            PageContent::Slice(.., scale) => *scale,
            PageContent::WholePage { scale, .. } => *scale,
        }
    }

    pub fn output_size(&self) -> pdf::Size2d {
        self.input_box().size() * self.input_to_output_scale()
    }

    pub fn background_colour(&self) -> Option<[u8; 4]> {
        match self {
            PageContent::Slice(page_capture, ..) => page_capture.background_colour,
            PageContent::WholePage { page, .. } => page.background_colour(),
        }
    }

    pub fn background_image_id(&self) -> Option<i32> {
        match self {
            PageContent::Slice(page_capture, ..) => page_capture.background_image_id,
            PageContent::WholePage { page, .. } => page.background_image_id(),
        }
    }

    pub fn embeds(&self) -> impl Iterator<Item = &'p EmbeddedPdfPage> {
        match self {
            PageContent::Slice(page_capture, ..) => {
                Either::Left(page_capture.embeds.iter().copied())
            }

            PageContent::WholePage { page, .. } => Either::Right(page.embedded_pdf_pages().iter()),
        }
    }

    pub fn objects(&self) -> impl ExactSizeIterator<Item = &'p DocObject> {
        match self {
            PageContent::Slice(page_capture, ..) => {
                Either::Left(page_capture.objects.iter().copied())
            }

            PageContent::WholePage { page, .. } => Either::Right({
                // Wrapper type so we can return an `ExactSizeIterator`.
                struct It<'p, I: Iterator<Item = &'p DocObject>>(I, usize);

                impl<'p, I: Iterator<Item = &'p DocObject>> Iterator for It<'p, I> {
                    type Item = &'p DocObject;

                    fn next(&mut self) -> Option<Self::Item> {
                        self.0.next()
                    }

                    fn size_hint(&self) -> (usize, Option<usize>) {
                        (self.1, Some(self.1))
                    }
                }

                impl<'p, I: Iterator<Item = &'p DocObject>> ExactSizeIterator for It<'p, I> {}

                let objects = page
                    .layers()
                    .iter()
                    .flat_map(|layer| layer.objects().iter());

                let n = page.layers().iter().map(|l| l.objects().len()).sum();

                It(objects, n)
            }),
        }
    }

    pub fn inline_objects(&self) -> impl Iterator<Item = &'p InlineObject> {
        match self {
            PageContent::Slice(page_capture, ..) => page_capture.inline_objects.iter().copied(),
            PageContent::WholePage { inline_objects, .. } => inline_objects.iter().copied(),
        }
    }
}

fn group_inline_objects_by_page(document: &sdocx::Document) -> Vec<Vec<&InlineObject>> {
    let mut by_page = vec![Vec::new(); document.pages().len()];

    let pageless = matches!(document.page_model(), sdocx::PageModel::Pageless);

    for inline_obj in document.body_text().inline_objects() {
        match inline_obj.object.page_index() {
            Some(i) => by_page[i as usize].push(inline_obj),

            // If no page is specified and this is a pageless document, put the inline object on
            // the first (only) page.
            None if pageless => by_page[0].push(inline_obj),

            None => {
                warn!(
                    "Ignoring inline {} because it does not specify a page, \
                    and this is not a pageless document",
                    <&str>::from(&inline_obj.object),
                );
            }
        }
    }

    for page_objs in &mut by_page {
        page_objs.sort_by_key(|iobj| iobj.index_in_text);
    }

    by_page
}

pub fn split_document_into_pages<'d>(
    doc: &'d sdocx::Document,
    embed_cache: &EmbedMap<'d>,
    user_auto_split: bool,
    user_basic_split: Option<BasicSplitMode>,
    user_no_split: bool,
) -> impl Iterator<Item = PageContent<'d>> {
    // Samsung Notes always paginates pageless notes when it exports them, so we do the same by
    // default. `--no-split` opts back in to a single long page.
    let (auto_split, basic_split) = if user_no_split {
        (false, None)
    } else {
        (
            user_auto_split,
            Some(user_basic_split.unwrap_or(BasicSplitMode::IsoPortrait)),
        )
    };

    let is_marked_pageless = matches!(doc.page_model(), sdocx::PageModel::Pageless);

    let page = match doc.pages().iter().exactly_one().ok() {
        // Pageless documents get special treatment, because it's up to us to determine whether to
        // insert page breaks, and if so, where.
        Some(only_page) if is_marked_pageless => only_page,

        // Paged documents (and documents with a page count other than 1 which are incorrectly
        // marked as pageless - shouldn't happen, but just in case...) are broken up for us
        // already, so we use the existing pages.
        _ => {
            let uniform_scale = compute_uniform_scale(doc.pages().iter().map(|page| {
                let page_box = sdocx::Box2d::from_size(sdocx::Size2d::new(
                    page.width as f64,
                    page.height as f64,
                ));

                (
                    page_box,
                    page.embedded_pdf_pages()
                        .iter()
                        .map(|embed| (embed_cache.get_crop_box(embed), embed.rect())),
                )
            }));

            return Either::Left(uniform_scale.into_iter().flat_map(move |uniform_scale| {
                let inline_objects = group_inline_objects_by_page(doc);

                doc.pages()
                    .iter()
                    .zip(inline_objects)
                    .with_position()
                    .filter_map(move |(pos, (page, inline_objects))| {
                        // For paged documents, there is a ghost page in the SDOCX that is not
                        // represented in the raster PDF. We ignore it too.
                        if !is_marked_pageless
                            && pos == itertools::Position::Last
                            && page.is_empty()
                            && inline_objects.is_empty()
                        {
                            return None;
                        }

                        Some(PageContent::WholePage {
                            page,
                            inline_objects,
                            scale: uniform_scale,
                        })
                    })
            }));
        }
    };

    // Compute the lowest point the bounding box of any content object reaches so we can ignore any
    // pages that begin after this point.
    let content_max_y = page
        .embedded_pdf_pages()
        .iter()
        .map(|embed| embed.bounding_box().max.y)
        .chain(
            page.layers()
                .iter()
                .flat_map(|layer| layer.objects())
                .map(|object| object.bounding_box().max.y),
        )
        .chain(
            doc.body_text()
                .inline_objects()
                .iter()
                .map(|iobj| iobj.bounding_box().max.y),
        )
        .max_by(f64::total_cmp)
        // If there's no content, just use the whole page height because it's probably sane.
        .unwrap_or(page.height as f64);

    let page_breaks = compute_page_breaks(page, auto_split, basic_split);

    let content_slices = Vec::from_iter({
        let page_width = page.width as f64;
        let background_colour = page.background_colour();

        page_breaks
            .tuple_windows()
            .map_while(move |(min_y, max_y)| {
                if min_y >= content_max_y {
                    // There is no content in this slice or any of the ones that follow.
                    return None;
                }

                let capture_box =
                    sdocx::Box2d::new((0.0, min_y).into(), (page_width, max_y).into());

                // todo: Check whether we include embeds/objects that are barely touching the
                // capture box here. If the object won't be visible on the output page, we
                // shouldn't be including it here. It might throw off the scale calculation and
                // could increase the size of the output PDF and make it more expensive to render
                // by including unnecessary duplicate objects on each page.

                // Find all the content that intersects the capture box.
                Some(PageCapture {
                    capture_box,
                    background_colour,
                    background_image_id: page.background_image_id(),
                    embeds: page
                        .embedded_pdf_pages()
                        .iter()
                        .filter(|embed| capture_box.intersects(&embed.bounding_box()))
                        .collect(),
                    objects: page
                        .layers()
                        .iter()
                        .flat_map(|layer| layer.objects())
                        .filter(|object| capture_box.intersects(&object.bounding_box()))
                        .collect(),
                    // There's only one input page, so all the inline objects in the document must
                    // be on it.
                    inline_objects: {
                        let mut objs: Vec<_> = doc
                            .body_text()
                            .inline_objects()
                            .iter()
                            .filter(|iobj| capture_box.intersects(&iobj.bounding_box()))
                            .collect();
                        objs.sort_by_key(|iobj| iobj.index_in_text);
                        objs
                    },
                })
            })
    });

    let uniform_scale = compute_uniform_scale(content_slices.iter().map(|slice| {
        (
            slice.capture_box,
            slice
                .embeds
                .iter()
                .map(|embed| (embed_cache.get_crop_box(embed), embed.rect())),
        )
    }));

    // Unwrapping is fine because we'd only get `None` if there were no pages, but we have at least
    // one content slice because we were guaranteed at least one pair of page breaks.
    let uniform_scale = uniform_scale.unwrap();

    Either::Right(
        content_slices
            .into_iter()
            .map(move |c| PageContent::Slice(c, uniform_scale)),
    )
}

#[derive(Debug, Error)]
pub enum DrawObjectError {
    #[error("failed to draw stroke object")]
    Stroke,

    #[error("failed to draw path for shape")]
    ShapePath(PathDrawingError),

    #[error("no style information for line")]
    NoLineStyle(NoStyleError),

    #[error("objects of type '{0}' are not yet supported")]
    Unsupported(&'static str),
}

impl DrawObjectError {
    pub fn log(&self) {
        match self {
            DrawObjectError::Stroke => error!("Failed to draw stroke object"),

            DrawObjectError::ShapePath(e @ PathDrawingError::NothingDrawn(_)) => {
                warn!("Shape will be invisible in output: {e}");
            }

            DrawObjectError::ShapePath(err) => {
                error!("Failed to draw shape: {err}")
            }

            DrawObjectError::NoLineStyle(_) => {
                error!("Line has no style information")
            }

            DrawObjectError::Unsupported(obj_type_name) => {
                warn!("Ignoring '{obj_type_name}' object (not yet supported)")
            }
        }
    }
}

/// SDOCX page -> PDF page conversion context.
pub struct PageConversionCtx<'s> {
    input_box: sdocx::Box2d<f64>,

    /// PDF drawing surface.
    ///
    /// When the page conversion context is constructed, a transformation matrix is pushed to this
    /// drawing surface that scales the content such that drawing operations can be done in
    /// document space rather than PDF space.
    pub surface: Surface<'s>,
}

pub struct FontMetrics<'a> {
    face: Option<ttf_parser::Face<'a>>,
    units_per_em: f32,
}

impl<'a> FontMetrics<'a> {
    pub fn new(font_bytes: &'a [u8]) -> Self {
        if let Ok(face) = ttf_parser::Face::parse(font_bytes, 0) {
            let units_per_em = face.units_per_em() as f32;
            Self {
                face: Some(face),
                units_per_em,
            }
        } else {
            Self {
                face: None,
                units_per_em: 1000.0,
            }
        }
    }

    pub fn char_width(&self, c: char, font_size: f32) -> f32 {
        if let Some(ref face) = self.face {
            if let Some(gid) = face.glyph_index(c) {
                if let Some(adv) = face.glyph_hor_advance(gid) {
                    return (adv as f32 / self.units_per_em) * font_size;
                }
            }
        }
        if c == ' ' {
            font_size * 0.28
        } else if c.is_ascii() {
            font_size * 0.48
        } else {
            font_size * 0.88
        }
    }

    pub fn str_width(&self, text: &str, font_size: f32) -> f32 {
        text.chars().map(|c| self.char_width(c, font_size)).sum()
    }
}

/// Gap between the bottom edge of a floating image and the top of the first line of text pushed
/// below it, in document units (about 2.3pt for the usual 1752-unit-wide page).
///
/// Samsung Notes leaves only a couple of points here, and pushes as many text lines as will fit
/// into the gap between two stacked images, so a large clearance would visibly move text that
/// Samsung keeps in place.
const FLOATING_IMAGE_CLEARANCE: f32 = 6.7;

/// Slack used when checking whether a line of text overlaps a floating image, so that a line that
/// merely grazes an image's edge is not pushed below it.
const COLLISION_TOLERANCE: f32 = 2.5;

/// How far below the baseline an underline sits, as a multiple of the font size.
///
/// Calibrated against Samsung Notes' own PDF export.
const UNDERLINE_BELOW_BASELINE_EM: f32 = 0.19;

/// How far above the baseline a strikethrough sits, as a multiple of the font size.
///
/// Calibrated against Samsung Notes' own PDF export.
const STRIKETHROUGH_ABOVE_BASELINE_EM: f32 = 0.21;

/// Distance from the top of a line box to the text baseline, as a multiple of the font size.
///
/// Samsung Notes places the baseline one full em below the top of the line, which we confirmed by
/// comparing against its own PDF export.
const BASELINE_EM: f32 = 1.0;

/// Distance from the baseline to the top of the glyphs, as a multiple of the font size.
const ASCENT_EM: f32 = 0.85;

/// Distance from the baseline to the bottom of the glyphs, as a multiple of the font size.
const DESCENT_EM: f32 = 0.18;

/// Horizontal shear applied to italic text: the tangent of the slant angle (roughly 12°).
const ITALIC_SKEW: f32 = 0.21;

/// Width of one indentation level, as a multiple of the current font size.
const INDENT_STEP_EM: f32 = 2.0;

/// Line height as a multiple of the font size.
const LINE_HEIGHT_FACTOR: f32 = 1.35;

/// Builds a transform that rotates by `angle` (radians) about the centre of `rect`.
fn rotation_about_centre(rect: &sdocx::Box2d<f64>, angle: f32) -> Transform {
    let (sin, cos) = angle.sin_cos();
    let cx = ((rect.min.x + rect.max.x) / 2.0) as f32;
    let cy = ((rect.min.y + rect.max.y) / 2.0) as f32;

    Transform::from_row(cos, sin, -sin, cos, cx - cx * cos + cy * sin, cy - cx * sin - cy * cos)
}

/// Wraps `text` into lines that fit within `max_width`, returning character-index ranges into
/// `text` for each line. Words are kept intact unless a single word is wider than a whole line, in
/// which case it is broken character by character.
fn wrap_text_ranges(
    text: &str,
    max_width: f32,
    font_size: f32,
    metrics: &FontMetrics,
) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();

    if max_width <= 0.0 || chars.is_empty() {
        return vec![(0, chars.len())];
    }

    // A little slack, so that we don't wrap a word just because our metrics are marginally wider
    // than the ones the original app used.
    let wrap_limit = max_width + 25.0;

    let mut ranges = Vec::new();
    let mut line_start = 0usize;
    let mut current_width = 0.0f32;
    let mut i = 0usize;

    while i < chars.len() {
        // A word is a run of characters ending with a space, and the space is kept.
        let mut word_end = i;
        while word_end < chars.len() && chars[word_end] != ' ' {
            word_end += 1;
        }
        if word_end < chars.len() {
            word_end += 1;
        }

        let word: String = chars[i..word_end].iter().collect();
        let word_width = metrics.str_width(&word, font_size);
        let line_is_empty = i == line_start;

        if word_width > wrap_limit {
            // The word can't fit on a line of its own, so break it up character by character. This
            // also applies to an empty line, which would otherwise be allowed to overflow.
            for k in i..word_end {
                let char_width = metrics.char_width(chars[k], font_size);

                if current_width + char_width > wrap_limit && k > line_start {
                    ranges.push((line_start, k));
                    line_start = k;
                    current_width = 0.0;
                }

                current_width += char_width;
            }

            i = word_end;
        } else if current_width + word_width <= wrap_limit || line_is_empty {
            current_width += word_width;
            i = word_end;
        } else {
            // Move the whole word onto the next line. Don't advance `i`, so that the word is
            // measured again against the new (empty) line.
            ranges.push((line_start, i));
            line_start = i;
            current_width = 0.0;
        }
    }

    if line_start < chars.len() {
        ranges.push((line_start, chars.len()));
    }

    if ranges.is_empty() {
        ranges.push((0, chars.len()));
    }

    ranges
}

/// Converts the character-index range `start..end` into a byte range in `text`.
fn char_range(text: &str, start: usize, end: usize) -> (usize, usize) {
    let byte_start = text
        .char_indices()
        .nth(start)
        .map(|(index, _)| index)
        .unwrap_or(text.len());

    let byte_end = text
        .char_indices()
        .nth(end)
        .map(|(index, _)| index)
        .unwrap_or(text.len());

    (byte_start, byte_end.max(byte_start))
}

/// Builds a mapping from UTF-16 positions to byte offsets in `text`, where `base` is the UTF-16
/// position of the first character.
fn utf16_to_byte_offsets(text: &str, base: u32) -> Vec<(u32, usize)> {
    let mut offsets = Vec::with_capacity(text.len() + 1);
    offsets.push((base, 0));

    let mut units = 0u32;
    for (byte_index, ch) in text.char_indices() {
        units += ch.len_utf16() as u32;
        offsets.push((base + units, byte_index + ch.len_utf8()));
    }

    offsets
}

/// Looks up the byte offset for a UTF-16 position, clamping to the closest known offset.
fn byte_offset_for_utf16(offsets: &[(u32, usize)], pos: u32) -> usize {
    match offsets.binary_search_by_key(&pos, |&(unit, _)| unit) {
        Ok(index) => offsets[index].1,
        Err(index) => offsets
            .get(index)
            .or_else(|| offsets.last())
            .map(|&(_, byte)| byte)
            .unwrap_or(0),
    }
}

/// A run of text drawn with a single style. Font sizes are in document units (already scaled).
#[derive(Clone, Copy)]
struct RunStyle {
    font_size: f32,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    /// Raw document colour bytes (blue, green, red, alpha).
    colour: [u8; 4],
}

impl RunStyle {
    fn from_text_style(style: TextStyle, dp_scale: f32) -> Self {
        Self {
            font_size: style.font_size * dp_scale,
            bold: style.bold,
            italic: style.italic,
            underline: style.underline,
            strikethrough: style.strikethrough,
            colour: style.colour,
        }
    }

    fn fallback(dp_scale: f32) -> Self {
        Self::from_text_style(TextStyle::default(), dp_scale)
    }
}

/// Reads the paragraph alignment at UTF-16 position `pos` from `body_text`, if any.
///
/// Returns one of:
/// * `0` for left
/// * `1` for centre
/// * `2` for right
/// * any other value is treated as left.
fn paragraph_alignment(body_text: Option<&sdocx::page::object::text::Text>, pos: u32) -> u32 {
    let Some(bt) = body_text else { return 0; };
    let Some(common) = bt.text_common() else { return 0; };
    let paragraph_index = bt.paragraph_index_at(pos);
    let paragraph_start = bt.paragraph_start_at(paragraph_index);
    let mut best = None;
    for p in common.paragraphs() {
        if format!("{:?}", p.paragraph_base.paragraph_type) != "Alignment" {
            continue;
        }
        let start = p.paragraph_base.start_pos;
        if start > paragraph_start {
            continue;
        }
        if let Some(b4) = p.bytes.get(0..4) {
            let v = u32::from_le_bytes([b4[0], b4[1], b4[2], b4[3]]);
            // Pick the alignment whose start_pos is closest to paragraph_start.
            best = match best {
                Some((best_start, _)) if start > best_start => Some((start, v)),
                None => Some((start, v)),
                other => other,
            };
        }
    }
    best.map(|(_, v)| v).unwrap_or(0)
}

/// Vertical flow state for the body text on one page.
struct TextFlowState {
    start_x: f32,
    max_x: f32,
    dp_scale: f32,
    cur_y: f32,
    flow_bottom: Option<f32>,
    /// Bottom of the last line of text that was actually drawn on this page. Empty lines advance
    /// `cur_y` but don't set this, so it can be used to tell whether there is real text above a
    /// given point.
    last_text_bottom: Option<f32>,
    /// Bounding boxes of floating images on the page, which text must not be drawn over.
    image_boxes: Vec<sdocx::Box2d<f64>>,
}

impl TextFlowState {
    /// How far an inline object at `obj_y` needs to be moved down so that it doesn't cover text
    /// that has already been drawn.
    ///
    /// Empty lines don't count as text: on a pageless note made only of handwriting, the object
    /// positions recorded in the document are already correct and must not be adjusted.
    fn inline_object_shift(&self, obj_y: f32) -> f32 {
        match self.last_text_bottom {
            Some(prev) if prev > obj_y => prev - obj_y,
            _ => 0.0,
        }
    }

    /// Cursor position at which a line of text of `font_size`, pushed below an image whose bottom
    /// edge is at `image_bottom`, has its glyph tops `FLOATING_IMAGE_CLEARANCE` below the image.
    ///
    /// The baseline sits `BASELINE_EM` below the cursor and the glyphs `ASCENT_EM` above the
    /// baseline, so the gap between the cursor and the glyph tops is `BASELINE_EM - ASCENT_EM`.
    fn position_below_image(&self, image_bottom: f32, font_size: f32) -> f32 {
        image_bottom + FLOATING_IMAGE_CLEARANCE - (BASELINE_EM - ASCENT_EM) * font_size
    }

    /// Moves `cur_y` down until a line of text starting there, and spanning `[x, x + width]`, no
    /// longer intersects any floating image. A line that would intrude into an image is moved
    /// below the bottom of that image.
    ///
    /// The vertical extent of the line is taken from the glyphs themselves (ascent and descent
    /// around the baseline) rather than from the full line box, because the extra leading in the
    /// line box is empty space and would otherwise push text below images that it doesn't actually
    /// touch.
    fn resolve_collision(&mut self, font_size: f32, x: f32, width: f32) {
        let line_right = x + width.max(1.0);
        let baseline = self.cur_y + font_size * BASELINE_EM;
        let mut ink_top = baseline - font_size * ASCENT_EM;
        let mut ink_bottom = baseline + font_size * DESCENT_EM;

        // Images can be stacked, so keep going until nothing collides.
        for _ in 0..32 {
            let mut collided = false;

            for bbox in &self.image_boxes {
                let image_top = bbox.min.y as f32;
                let image_bottom = bbox.max.y as f32;
                let image_left = bbox.min.x as f32;
                let image_right = bbox.max.x as f32;

                if image_left >= line_right || image_right <= x {
                    // No horizontal overlap, so the line can pass beside the image.
                    continue;
                }

                let overlaps_vertically = ink_top < image_bottom - COLLISION_TOLERANCE
                    && ink_bottom > image_top + COLLISION_TOLERANCE;

                if overlaps_vertically {
                    // Skip past the image, then recompute the ink band for the new position.
                    self.cur_y = self.position_below_image(image_bottom, font_size);
                    collided = true;
                    break;
                }
            }

            if !collided {
                break;
            }

            let baseline = self.cur_y + font_size * BASELINE_EM;
            ink_top = baseline - font_size * ASCENT_EM;
            ink_bottom = baseline + font_size * DESCENT_EM;
        }
    }
}

impl<'s> PageConversionCtx<'s> {
    pub fn new<'p>(
        page: &PageContent<'p>,
        // page_size: (f32, f32),
        // pt_per_unit: f32,
        mut surface: Surface<'s>,
    ) -> PageConversionCtx<'s> {
        // Apply a transformation here so that drawing operations to the surface can use document
        // units rather than locally scaling things whenever we need to draw them.
        let scale = page.input_to_output_scale().get() as f32;
        let input_origin = page.input_box().min.cast::<f32>();

        surface.push_transform(&Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            // If the page is actually a slice of a larger page, the input box origin won't be at
            // zero. Translate the content back to the origin.
            -input_origin.x * scale,
            -input_origin.y * scale,
        ));

        PageConversionCtx {
            input_box: page.input_box(),
            surface,
        }
    }

    pub fn fill_background(&mut self, r: u8, g: u8, b: u8, a: u8) {
        self.surface.set_stroke(None);
        self.surface.set_fill(Some(Fill {
            paint: rgb::Color::new(r, g, b).into(),
            opacity: NormalizedF32::new(a as f32 / 255.0).unwrap(),
            ..Fill::default()
        }));

        self.surface.draw_path(&self.boundary_path());
    }

    pub fn draw_background_image(
        &mut self,
        image_name: &str,
        media: &mut MediaStorage,
    ) {
        if let Ok(mut reader) = media.open_file(image_name) {
            let mut image_bytes = Vec::new();
            if reader.read_to_end(&mut image_bytes).is_ok() {
                let img = krilla::image::Image::from_jpeg(image_bytes.clone().into(), true)
                    .or_else(|_| krilla::image::Image::from_png(image_bytes.clone().into(), true))
                    .or_else(|_| krilla::image::Image::from_webp(image_bytes.clone().into(), true))
                    .or_else(|_| krilla::image::Image::from_gif(image_bytes.into(), true))
                    .ok();
                if let Some(img) = img {
                    let input_box = self.input_box.cast::<f32>();
                    let size = input_box.size();
                    if let Some(sz) = krilla::geom::Size::from_wh(size.width, size.height) {
                        self.surface.push_transform(&krilla::geom::Transform::from_translate(input_box.min.x, input_box.min.y));
                        self.surface.draw_image(img, sz);
                        self.surface.pop();
                    }
                }
            }
        }
    }

    fn boundary_path(&self) -> krilla::geom::Path {
        let mut rect_pb = krilla::geom::PathBuilder::new();

        let input_box = self.input_box.cast::<f32>();
        let origin = input_box.min;
        let size = input_box.size();

        rect_pb.push_rect(
            krilla::geom::Rect::from_xywh(origin.x, origin.y, size.width, size.height).unwrap(),
        );

        rect_pb.finish().unwrap()
    }

    fn draw_stroke_chunk_events<'e>(
        &mut self,
        stroke_events: impl IntoIterator<Item = EventGroup<'e>>,
        fill_boundary: &krilla::geom::Path,
        tool: Tool,
    ) -> Result<(), ()> {
        tool.draw_events(stroke_events, fill_boundary, &mut self.surface)
    }

    /// Fills an axis-aligned rectangle using the current fill colour.
    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        if !(width > 0.0 && height > 0.0) {
            return;
        }

        let Some(rect) = Rect::from_xywh(x, y, width, height) else {
            return;
        };

        let mut builder = PathBuilder::new();
        builder.push_rect(rect);

        if let Some(path) = builder.finish() {
            self.surface.draw_path(&path);
        }
    }

    /// Draws a single styled run of text with its baseline at `baseline_y`, returning the advance
    /// width of the run.
    fn draw_text_run(
        &mut self,
        x: f32,
        baseline_y: f32,
        text: &str,
        style: &RunStyle,
        metrics: &FontMetrics,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }

        let width = metrics.str_width(text, style.font_size);
        let [b, g, r, a] = style.colour;

        self.surface.set_stroke(None);
        self.surface.set_fill(Some(Fill {
            paint: rgb::Color::new(r, g, b).into(),
            opacity: NormalizedF32::new(a as f32 / 255.0).unwrap_or(NormalizedF32::ONE),
            ..Fill::default()
        }));

        // Italic text is synthesised by shearing around the baseline, so that the text keeps its
        // position on the line instead of sliding sideways with the shear.
        if style.italic {
            self.surface.push_transform(&Transform::from_row(
                1.0,
                0.0,
                ITALIC_SKEW,
                1.0,
                -ITALIC_SKEW * baseline_y,
                0.0,
            ));
        }

        self.surface.draw_text(
            Point::from_xy(x, baseline_y),
            get_font(style.bold),
            style.font_size,
            text,
            false,
            TextDirection::Auto,
        );

        if style.underline {
            let thickness = (style.font_size * 0.055).max(0.4);
            let y = baseline_y + style.font_size * UNDERLINE_BELOW_BASELINE_EM;
            self.fill_rect(x, y, width, thickness);
        }

        if style.strikethrough {
            let thickness = (style.font_size * 0.05).max(0.4);
            let y = baseline_y - style.font_size * STRIKETHROUGH_ABOVE_BASELINE_EM;
            self.fill_rect(x, y, width, thickness);
        }

        if style.italic {
            self.surface.pop();
        }

        width
    }

    /// Draws one logical line (i.e. one document paragraph) of body text, including any list
    /// marker and indentation, advancing `flow`.
    #[allow(clippy::too_many_arguments)]
    fn draw_text_line(
        &mut self,
        flow: &mut TextFlowState,
        body_text: Option<&sdocx::page::object::text::Text>,
        line: &str,
        line_start_u16: u32,
        indent_level: u32,
        marker: Option<&str>,
        metrics_regular: &FontMetrics,
        metrics_bold: &FontMetrics,
    ) {
        let trimmed = line.trim_end();

        let base_style = body_text
            .and_then(|bt| bt.style_at(line_start_u16))
            .map(|style| RunStyle::from_text_style(style, flow.dp_scale))
            .unwrap_or_else(|| RunStyle::fallback(flow.dp_scale));

        let font_size = base_style.font_size.max(1.0);
        let line_height = font_size * LINE_HEIGHT_FACTOR;
        let line_x = flow.start_x + indent_level as f32 * font_size * INDENT_STEP_EM;
        let base_metrics = if base_style.bold { metrics_bold } else { metrics_regular };

        if trimmed.is_empty() {
            // An empty line still takes up vertical space (the user pressed Enter).
            flow.resolve_collision(font_size, line_x, font_size * 0.5);
            flow.cur_y += line_height;
            flow.flow_bottom = Some(flow.cur_y);
            return;
        }

        // List markers push the text of the item to the right, and continuation lines of a wrapped
        // item stay aligned with the item's text rather than with its marker.
        let mut body_x = line_x;
        if let Some(marker) = marker {
            body_x += base_metrics.str_width(marker, font_size);
        }

        let available_width = (flow.max_x - body_x).max(50.0);
        let ranges = wrap_text_ranges(trimmed, available_width, font_size, base_metrics);

        for (range_index, &(char_start, char_end)) in ranges.iter().enumerate() {
            let (byte_start, byte_end) = char_range(trimmed, char_start, char_end);
            let text = trimmed[byte_start..byte_end].trim_end();

            if text.is_empty() {
                continue;
            }

            let abs_start = line_start_u16 + trimmed[..byte_start].encode_utf16().count() as u32;
            let abs_end = abs_start + text.encode_utf16().count() as u32;

            let runs = match body_text {
                Some(bt) => bt.style_runs(abs_start, abs_end),
                None => vec![(abs_start, TextStyle::default())],
            };

            let offsets = utf16_to_byte_offsets(text, abs_start);
            let mut pieces: Vec<(String, RunStyle)> = Vec::new();

            for (run_index, &(run_start, style)) in runs.iter().enumerate() {
                let run_end = runs
                    .get(run_index + 1)
                    .map(|&(next_start, _)| next_start)
                    .unwrap_or(abs_end);

                let run_byte_start = byte_offset_for_utf16(&offsets, run_start);
                let run_byte_end = byte_offset_for_utf16(&offsets, run_end);

                if run_byte_end <= run_byte_start {
                    continue;
                }

                pieces.push((
                    text[run_byte_start..run_byte_end].to_string(),
                    RunStyle::from_text_style(style, flow.dp_scale),
                ));
            }

            if pieces.is_empty() {
                continue;
            }

            let line_width: f32 = pieces
                .iter()
                .map(|(piece, style)| {
                    let metrics = if style.bold { metrics_bold } else { metrics_regular };
                    metrics.str_width(piece, style.font_size)
                })
                .sum();

            // All runs share a baseline, which is placed as if the tallest run were the line's
            // font.
            let largest_font_size = pieces
                .iter()
                .map(|(_, style)| style.font_size)
                .fold(font_size, f32::max);

            // The first wrapped line honours the paragraph's alignment; continuation lines start
            // at body_x. Samsung's reference exports align only the first line; we follow the same.
            let mut body_x = body_x;
            if range_index == 0 {
                let alignment = paragraph_alignment(body_text, line_start_u16);
                let available = (flow.max_x - body_x).max(0.0);
                let shift = match alignment {
                    1 => (available - line_width) * 0.5,
                    2 => (available - line_width).max(0.0),
                    _ => 0.0,
                };
                body_x += shift;
            }

            flow.resolve_collision(largest_font_size, body_x, line_width);

            let baseline = flow.cur_y + largest_font_size * BASELINE_EM;

            if range_index == 0 {
                if let Some(marker) = marker {
                    self.draw_text_run(line_x, baseline, marker, &base_style, base_metrics);
                }
            }

            let mut x = body_x;
            for (piece, style) in &pieces {
                let metrics = if style.bold { metrics_bold } else { metrics_regular };
                x += self.draw_text_run(x, baseline, piece, style, metrics);
            }

            flow.cur_y += line_height;
            flow.flow_bottom = Some(flow.cur_y);
            flow.last_text_bottom = Some(flow.cur_y);
        }
    }

    pub fn draw_single_object(
        &mut self,
        object: &sdocx::DocObject,
        pen_width_mul: f32,
        marker_width_mul: f32,
        media: &mut MediaStorage,
    ) -> Result<(), DrawObjectError> {
        match object {
            sdocx::DocObject::Stroke(stroke) => self
                .draw_stroke_chunk_events(
                    [EventGroup::from_stroke(stroke)],
                    &self.boundary_path(),
                    Tool::for_stroke(stroke).with_scaled_width(pen_width_mul, marker_width_mul),
                )
                .map_err(|()| DrawObjectError::Stroke),

            sdocx::DocObject::Line(line) => {
                if line.has_control_points() {
                    warn!("Ignoring line control points");
                }

                self.draw_line(line.start(), line.end(), line.colour_effect(), line.style())
                    .map_err(DrawObjectError::NoLineStyle)
            }

            sdocx::DocObject::Shape(_) | sdocx::DocObject::Image(_) => {
                let shape = match object {
                    sdocx::DocObject::Shape(shape) => shape,
                    sdocx::DocObject::Image(image) => &image.shape,
                    _ => unreachable!(),
                };

                self.draw_shape(shape, media)
                    .map_err(DrawObjectError::ShapePath)
            }

            sdocx::DocObject::Text(text) => {
                self.draw_text_box(text, object.object_base().rect, object.object_base().angle());
                Ok(())
            }

            other => Err(DrawObjectError::Unsupported(<&str>::from(other))),
        }
    }

    /// Draws a text box (`DocObject::Text`) inside the rectangle recorded in the document.
    ///
    /// Text boxes hold their own copy of the text stream, so they go through the same layout code
    /// as the note body, just constrained to the box instead of the page margins.
    fn draw_text_box(
        &mut self,
        text: &sdocx::page::object::text::Text,
        rect: sdocx::Box2d<f64>,
        angle: Option<f32>,
    ) {
        let dp_scale = self.dp_scale();

        let start_x = rect.min.x as f32 + text.left_margin() * dp_scale;
        let max_x = (rect.max.x as f32 - text.right_margin() * dp_scale).max(start_x + 1.0);
        let start_y = rect.min.y as f32 + text.top_margin() * dp_scale;

        // A text box can be rotated. Lay the text out unrotated and then rotate the result about
        // the centre of the box, so that wrapping is unaffected by the angle.
        let rotated = angle.filter(|a| a.abs() > 0.01);
        if let Some(angle) = rotated {
            self.surface
                .push_transform(&rotation_about_centre(&rect, angle.to_radians()));
        }

        let mut flow = TextFlowState {
            start_x,
            max_x,
            dp_scale,
            cur_y: start_y,
            flow_bottom: None,
            last_text_bottom: None,
            image_boxes: Vec::new(),
        };

        let metrics_regular = cached_metrics(false);
        let metrics_bold = cached_metrics(true);

        let raw = text.raw_string().unwrap_or("");
        let units: Vec<u16> = raw.encode_utf16().collect();
        let mut line_start = 0usize;
        let mut next_number = 1u32;

        while line_start <= units.len() {
            let line_end = units[line_start..]
                .iter()
                .position(|&c| c == 0x000A)
                .map(|pos| line_start + pos)
                .unwrap_or(units.len());

            let line = String::from_utf16_lossy(&units[line_start..line_end]);
            let paragraph_index = text.paragraph_index_at(line_start as u32);
            let indent_level = text.indent_level_at_paragraph(paragraph_index);

            let marker = match text.bullet_at_paragraph(paragraph_index) {
                Some(BulletStyle {
                    kind: BulletKind::Numbered,
                    number,
                    ..
                }) => {
                    let number = if number > 0 { number } else { next_number };
                    next_number = number + 1;
                    Some(format!("{number}. "))
                }
                Some(_) => {
                    next_number = 1;
                    Some("\u{2022} ".to_string())
                }
                None => {
                    next_number = 1;
                    None
                }
            };

            self.draw_text_line(
                &mut flow,
                Some(text),
                &line,
                line_start as u32,
                indent_level,
                marker.as_deref(),
                metrics_regular,
                metrics_bold,
            );

            if line_end < units.len() {
                line_start = line_end + 1;
            } else {
                break;
            }
        }

        if rotated.is_some() {
            self.surface.pop();
        }
    }

    /// Scale that maps the document's 360-unit-wide page to the output page width.
    fn dp_scale(&self) -> f32 {
        let page_width = self.input_box.width() as f32;

        if page_width > 0.0 {
            page_width / 360.0
        } else {
            1752.0 / 360.0
        }
    }

    pub fn draw_inline_objects<'a>(
        &mut self,
        inline_objects: impl IntoIterator<Item = &'a InlineObject>,
        pen_width_mul: f32,
        marker_width_mul: f32,
        media: &mut MediaStorage,
    ) {
        self.draw_body_text_and_inline_objects(
            None,
            0,
            inline_objects,
            std::iter::empty(),
            pen_width_mul,
            marker_width_mul,
            media,
        );
    }

    #[allow(unused_assignments)]
    pub fn draw_body_text_and_inline_objects<'a>(
        &mut self,
        opt_body_text: Option<&sdocx::page::object::text::Text>,
        page_index: usize,
        inline_objects: impl IntoIterator<Item = &'a InlineObject>,
        floating_objects: impl IntoIterator<Item = &'a DocObject>,
        pen_width_mul: f32,
        marker_width_mul: f32,
        media: &mut MediaStorage,
    ) {
        let page_width = self.input_box.width() as f32;
        let dp_scale = if page_width > 0.0 {
            page_width / 360.0
        } else {
            1752.0 / 360.0
        };

        let (left_m, top_m, right_m) = match opt_body_text {
            Some(bt) => (
                bt.left_margin() * dp_scale,
                bt.top_margin() * dp_scale,
                bt.right_margin() * dp_scale,
            ),
            None => (0.0, 0.0, 0.0),
        };
        let start_x = if left_m > 1.0 { left_m } else { 77.8667 };
        let max_x = if right_m > 1.0 {
            page_width - right_m
        } else {
            page_width - start_x
        };

        // Floating images are obstacles that the text flow has to avoid.
        let mut image_boxes: Vec<sdocx::Box2d<f64>> = floating_objects
            .into_iter()
            .filter_map(|obj| match obj {
                DocObject::Image(_) => Some(obj.object_base().rect),
                _ => None,
            })
            .collect();
        image_boxes.sort_by(|a, b| {
            a.min.y
                .partial_cmp(&b.min.y)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut flow = TextFlowState {
            start_x,
            max_x,
            dp_scale,
            cur_y: if top_m > 1.0 { top_m } else { 48.6667 },
            flow_bottom: None,
            last_text_bottom: None,
            image_boxes,
        };

        let loaded_regular = get_loaded_font(false);
        let loaded_bold = get_loaded_font(true);
        let metrics_regular = FontMetrics::new(&loaded_regular.bytes);
        let metrics_bold = FontMetrics::new(&loaded_bold.bytes);

        let full_text = opt_body_text.and_then(|bt| bt.raw_string()).unwrap_or("");
        let utf16_units: Vec<u16> = full_text.encode_utf16().collect();
        let section_data = opt_body_text.map(|bt| bt.section_data()).unwrap_or(&[]);

        let (page_u16_slice, page_start_u16) = if !section_data.is_empty() {
            if page_index < section_data.len() {
                let (sec_start, sec_count) = section_data[page_index];
                let start_u16 = (sec_start as usize).min(utf16_units.len());
                let end_u16 = (start_u16 + sec_count as usize).min(utf16_units.len());
                let slice = &utf16_units[start_u16..end_u16];
                if page_index > 0 && !slice.is_empty() && slice[0] == 0x000A {
                    (&slice[1..], start_u16 + 1)
                } else {
                    (slice, start_u16)
                }
            } else {
                (&[][..], 0)
            }
        } else if page_index == 0 && !utf16_units.is_empty() {
            (&utf16_units[..], 0)
        } else {
            (&[][..], 0)
        };

        let mut page_inline_objs: Vec<&'a InlineObject> = inline_objects.into_iter().collect();
        page_inline_objs.sort_by_key(|io| io.index_in_text);
        let mut inline_idx = 0;

        // Running counter for numbered list items that don't carry an explicit number.
        let mut next_number = 1u32;

        if !page_u16_slice.is_empty() {
            let mut line_start_in_slice = 0;
            while line_start_in_slice <= page_u16_slice.len() {
                let line_end_in_slice = page_u16_slice[line_start_in_slice..]
                    .iter()
                    .position(|&c| c == 0x000A)
                    .map(|pos| line_start_in_slice + pos)
                    .unwrap_or(page_u16_slice.len());

                let line_u16 = &page_u16_slice[line_start_in_slice..line_end_in_slice];
                let line_doc_u16_pos = (page_start_u16 + line_start_in_slice) as u32;

                // Paragraph-level formatting: indentation and list markers.
                let (indent_level, bullet) = match opt_body_text {
                    Some(bt) => {
                        let paragraph_index = bt.paragraph_index_at(line_doc_u16_pos);
                        (
                            bt.indent_level_at_paragraph(paragraph_index),
                            bt.bullet_at_paragraph(paragraph_index),
                        )
                    }
                    None => (0, None),
                };

                let marker = match bullet {
                    Some(BulletStyle {
                        kind: BulletKind::Numbered,
                        number,
                        ..
                    }) => {
                        let number = if number > 0 { number } else { next_number };
                        next_number = number + 1;
                        Some(format!("{number}. "))
                    }
                    Some(_) => {
                        next_number = 1;
                        Some("\u{2022} ".to_string())
                    }
                    None => {
                        next_number = 1;
                        None
                    }
                };

                if line_u16.iter().any(|&c| c == 0xFFFC) {
                    let mut sub_start = 0;
                    while sub_start <= line_u16.len() {
                        let sub_end = line_u16[sub_start..]
                            .iter()
                            .position(|&c| c == 0xFFFC)
                            .map(|pos| sub_start + pos)
                            .unwrap_or(line_u16.len());

                        let sub_u16 = &line_u16[sub_start..sub_end];
                        let sub_doc_u16_pos = line_doc_u16_pos + sub_start as u32;
                        let sub_text = String::from_utf16_lossy(sub_u16);

                        // The list marker belongs to the start of the line, not to each segment.
                        let sub_marker = if sub_start == 0 { marker.as_deref() } else { None };

                        // Segments on either side of an object placeholder are often empty. Those
                        // must not consume a line of vertical space, or every object on the page
                        // ends up pushed further down than the last.
                        if !sub_text.trim().is_empty() {
                            self.draw_text_line(
                                &mut flow,
                                opt_body_text,
                                &sub_text,
                                sub_doc_u16_pos,
                                indent_level,
                                sub_marker,
                                &metrics_regular,
                                &metrics_bold,
                            );
                        }

                        if sub_end < line_u16.len() {
                            // Consuming inline object placeholder (0xFFFC)
                            if inline_idx < page_inline_objs.len() {
                                let io = page_inline_objs[inline_idx];
                                inline_idx += 1;

                                let bbox = io.object.object_base().rect;
                                let obj_y = bbox.min.y as f32;
                                let obj_h = bbox.height() as f32;

                                // Objects carry absolute positions, and for a note with no text
                                // (handwriting on a pageless canvas) that position is already
                                // exactly right. When there is text immediately above, though, our
                                // own text layout may have ended up lower than the document's, so
                                // move the object below the text instead of letting it cover what
                                // we just drew.
                                let shift = flow.inline_object_shift(obj_y);

                                if shift > 0.01 {
                                    self.surface
                                        .push_transform(&Transform::from_translate(0.0, shift));
                                    self.draw_inline_object(io, pen_width_mul, marker_width_mul, media);
                                    self.surface.pop();
                                } else {
                                    self.draw_inline_object(io, pen_width_mul, marker_width_mul, media);
                                }

                                // Continue the text flow below the object so that any following
                                // text doesn't land on top of it.
                                flow.cur_y = flow.cur_y.max(obj_y + shift + obj_h + 10.0);
                                flow.flow_bottom = Some(flow.cur_y);
                            }
                            sub_start = sub_end + 1;
                        } else {
                            break;
                        }
                    }
                } else {
                    let line_text = String::from_utf16_lossy(line_u16);
                    self.draw_text_line(
                        &mut flow,
                        opt_body_text,
                        &line_text,
                        line_doc_u16_pos,
                        indent_level,
                        marker.as_deref(),
                        &metrics_regular,
                        &metrics_bold,
                    );
                }

                if line_end_in_slice < page_u16_slice.len() {
                    line_start_in_slice = line_end_in_slice + 1;
                } else {
                    break;
                }
            }
        }

        // Process any remaining inline objects (or all of them, if no text was present on this
        // page). These are positioned absolutely, like the ones handled in the text loop above.
        while inline_idx < page_inline_objs.len() {
            let io = page_inline_objs[inline_idx];
            inline_idx += 1;

            let bbox = io.object.object_base().rect;
            let obj_y = bbox.min.y as f32;
            let obj_h = bbox.height() as f32;
            let shift = flow.inline_object_shift(obj_y);

            if shift > 0.01 {
                self.surface
                    .push_transform(&Transform::from_translate(0.0, shift));
                self.draw_inline_object(io, pen_width_mul, marker_width_mul, media);
                self.surface.pop();
            } else {
                self.draw_inline_object(io, pen_width_mul, marker_width_mul, media);
            }

            flow.cur_y = flow.cur_y.max(obj_y + shift + obj_h + 10.0);
            flow.flow_bottom = Some(flow.cur_y);
        }
    }

    /// Draws an inline object at the position recorded in the document.
    fn draw_inline_object(
        &mut self,
        inline_object: &InlineObject,
        pen_width_mul: f32,
        marker_width_mul: f32,
        media: &mut MediaStorage,
    ) {
        if let Err(err) =
            self.draw_single_object(&inline_object.object, pen_width_mul, marker_width_mul, media)
        {
            err.log();
        }
    }

    pub fn draw_objects<'d>(
        &mut self,
        objects: impl ExactSizeIterator<Item = &'d DocObject>,
        pen_width_mul: f32,
        marker_width_mul: f32,
        multi_progress: &MultiProgress,
        media: &mut MediaStorage,
    ) {
        let objects_bar = multi_progress
            .add(ProgressBar::new(objects.len() as _))
            .with_style(
                ProgressStyle::with_template(
                    "Processing objects [{bar:40}] {percent}% [{pos}/{len}]",
                )
                .unwrap()
                .progress_chars("# "),
            );

        // Consecutive strokes very often use the same tool. To reduce the size of the output PDF,
        // we can process chains of such strokes in one go, loading the necessary graphics state
        // only once and then using it for all the strokes rather than loading the same graphics
        // state for every stroke.
        let chunked_objects = objects
            .inspect(|_| objects_bar.inc(1))
            .chunk_by(|&obj| match obj {
                sdocx::DocObject::Stroke(stroke) => Some(
                    Tool::for_stroke(stroke).with_scaled_width(pen_width_mul, marker_width_mul),
                ),
                _non_stroke => None,
            });

        // Compute the filling boundary once for this layer so we don't have to recompute it for
        // every chunk of events we draw.
        let fill_boundary = self.boundary_path();

        for (opt_stroke_tool, objects) in &chunked_objects {
            if let Some(tool) = opt_stroke_tool {
                // This is a chunk of strokes that all use `tool`.

                // Get the event slice for each stroke.
                let stroke_events = objects.map(|o| {
                    let sdocx::DocObject::Stroke(s) = o else {
                        unreachable!()
                    };
                    EventGroup::from_stroke(s)
                });

                if let Err(()) = self.draw_stroke_chunk_events(stroke_events, &fill_boundary, tool)
                {
                    DrawObjectError::Stroke.log();
                }

                continue;
            }

            // This is a chunk of non-strokes.
            for obj in objects {
                if let Err(err) =
                    self.draw_single_object(obj, pen_width_mul, marker_width_mul, media)
                {
                    err.log();
                }
            }
        }
    }
}

impl Drop for PageConversionCtx<'_> {
    fn drop(&mut self) {
        // Pop the coordinate transformation.
        self.surface.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_metrics() -> FontMetrics<'static> {
        FontMetrics::new(NANUM_REGULAR_BYTES)
    }

    fn flow_with_image(min_y: f64, max_y: f64, min_x: f64, max_x: f64) -> TextFlowState {
        TextFlowState {
            start_x: 0.0,
            max_x: 1000.0,
            dp_scale: 1.0,
            cur_y: 0.0,
            flow_bottom: None,
            last_text_bottom: None,
            image_boxes: vec![sdocx::Box2d::new((min_x, min_y).into(), (max_x, max_y).into())],
        }
    }

    #[test]
    fn short_text_is_not_wrapped() {
        let metrics = test_metrics();
        let ranges = wrap_text_ranges("hello world", 10_000.0, 10.0, &metrics);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], (0, 11));
    }

    #[test]
    fn long_text_is_wrapped_at_word_boundaries() {
        let metrics = test_metrics();
        let text = "aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll";
        let ranges = wrap_text_ranges(text, 60.0, 10.0, &metrics);

        assert!(ranges.len() > 1, "expected wrapping, got {ranges:?}");

        // Ranges must be contiguous, in order, and cover the whole string.
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, text.chars().count());
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }

        // Each wrapped line must fit (allowing the slack used by the wrapper).
        for &(start, end) in &ranges {
            let line: String = text.chars().skip(start).take(end - start).collect();
            assert!(
                metrics.str_width(&line, 10.0) <= 60.0 + 25.0,
                "line {line:?} is too wide"
            );
        }
    }

    #[test]
    fn overlong_word_is_broken_between_characters() {
        let metrics = test_metrics();
        let ranges = wrap_text_ranges(&"a".repeat(100), 50.0, 10.0, &metrics);

        assert!(ranges.len() > 1);
        assert_eq!(ranges.last().unwrap().1, 100);
    }

    #[test]
    fn utf16_offsets_handle_non_bmp_characters() {
        // The emoji is a surrogate pair, so it occupies two UTF-16 units but one char.
        let text = "a\u{1F600}b";
        let offsets = utf16_to_byte_offsets(text, 10);

        assert_eq!(byte_offset_for_utf16(&offsets, 10), 0);
        assert_eq!(byte_offset_for_utf16(&offsets, 11), 1);
        assert_eq!(byte_offset_for_utf16(&offsets, 13), 5);
        assert_eq!(byte_offset_for_utf16(&offsets, 14), 6);

        // Positions outside the range clamp to the nearest known offset.
        assert_eq!(byte_offset_for_utf16(&offsets, 0), 0);
        assert_eq!(byte_offset_for_utf16(&offsets, 99), 6);
    }

    #[test]
    fn line_overlapping_an_image_is_moved_below_it() {
        // The image starts where the first line would be drawn. The line's glyph tops land
        // `FLOATING_IMAGE_CLEARANCE` below the image bottom, which means `cur_y` is shifted by the
        // glyph-height-in-em difference as well.
        let mut flow = flow_with_image(20.0, 500.0, 0.0, 400.0);
        flow.resolve_collision(50.0, 0.0, 200.0);

        let expected = 500.0 + FLOATING_IMAGE_CLEARANCE - (BASELINE_EM - ASCENT_EM) * 50.0;
        assert_eq!(flow.cur_y, expected);
    }

    #[test]
    fn line_above_an_image_is_left_alone() {
        let mut flow = flow_with_image(300.0, 500.0, 0.0, 400.0);
        flow.resolve_collision(50.0, 0.0, 200.0);

        assert_eq!(flow.cur_y, 0.0);
    }

    #[test]
    fn line_beside_an_image_is_left_alone() {
        // The image is entirely to the right of the line.
        let mut flow = flow_with_image(0.0, 500.0, 300.0, 400.0);
        flow.resolve_collision(50.0, 0.0, 200.0);

        assert_eq!(flow.cur_y, 0.0);
    }

    #[test]
    fn collision_resolution_skips_stacked_images() {
        let mut flow = TextFlowState {
            start_x: 0.0,
            max_x: 1000.0,
            dp_scale: 1.0,
            cur_y: 0.0,
            flow_bottom: None,
            last_text_bottom: None,
            image_boxes: vec![
                sdocx::Box2d::new((0.0, 10.0).into(), (400.0, 200.0).into()),
                sdocx::Box2d::new((0.0, 210.0).into(), (400.0, 400.0).into()),
            ],
        };

        flow.resolve_collision(50.0, 0.0, 200.0);

        let expected = 400.0 + FLOATING_IMAGE_CLEARANCE - (BASELINE_EM - ASCENT_EM) * 50.0;
        assert_eq!(flow.cur_y, expected);
    }
}
