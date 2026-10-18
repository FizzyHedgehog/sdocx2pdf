use std::path::Path;
use indicatif::MultiProgress;
use itertools::Itertools;
use krilla::{
    geom::{Size, Transform},
    metadata::Metadata,
    page::PageSettings,
};
use sdocx::Document;

pub mod page;
pub mod pdf;
pub mod shape;
pub mod stroke;
pub mod tool;

use page::{EmbedMap, PageConversionCtx};

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BasicSplitMode {
    #[value(
        help = "Split the document into portrait pages with a 1:√2 aspect ratio, \
    like ISO 216 paper sizes (A1, A2, A3, A4, etc.)"
    )]
    IsoPortrait,
    #[value(
        help = "Split the document into landscape pages with a √2:1 aspect ratio, \
    like ISO 216 paper sizes (A1, A2, A3, A4, etc.)"
    )]
    IsoLandscape,
}

pub fn convert_sdocx_to_pdf(
    in_path: &Path,
    out_path: &Path,
    pen_multiplier: f32,
    marker_multiplier: f32,
    auto_split: bool,
    basic_split: Option<BasicSplitMode>,
) -> anyhow::Result<()> {
    let (document, mut media_storage) = match Document::from_zip(in_path) {
        Ok(v) => v,
        Err(_) => Document::from_dir(in_path)?,
    };

    let document_name = document
        .title_text()
        .raw_string()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("Samsung Notes");

    let mut pdf = krilla::Document::new();
    let embed_map = EmbedMap::new(&document, &mut media_storage)?;
    let pages = page::split_document_into_pages(&document, &embed_map, auto_split, basic_split).collect_vec();
    let embedded_documents = embed_map.into_documents();

    let multi_progress = MultiProgress::new();

    for (page_index, page) in pages.into_iter().enumerate() {
        let output_size = page.output_size();
        let mut pdf_page = pdf.start_page_with(PageSettings::new(
            Size::from_wh(output_size.width as f32, output_size.height as f32)
                .expect("page size calculation failed"),
        ));

        let mut page_ctx = PageConversionCtx::new(&page, pdf_page.surface());

        if let Some([b, g, r, a]) = page.background_colour() {
            page_ctx.fill_background(r, g, b, a);
        }

        if let Some(bg_id) = page.background_image_id() {
            if let Some(bound_file) = document.file_registry().get(bg_id as u32) {
                page_ctx.draw_background_image(bound_file.name(), &mut media_storage);
            }
        }

        for embed in page.embeds() {
            let src_page_index = embed.page_index() as usize;
            let dest_rect = embed.rect();
            let dest_width = dest_rect.width();
            let dest_height = dest_rect.height();

            if let Some(dest_size) = Size::from_wh(dest_width as f32, dest_height as f32) {
                page_ctx.surface.push_transform(&Transform::from_row(
                    1.0, 0.0, 0.0, 1.0,
                    dest_rect.min.x as f32,
                    dest_rect.min.y as f32,
                ));

                if let Some(doc) = embedded_documents.get(embed.file().name()) {
                    page_ctx.surface.draw_pdf_page(doc, dest_size, src_page_index);
                }

                page_ctx.surface.pop();
            }
        }

        page_ctx.draw_body_text_and_inline_objects(
            Some(document.body_text()),
            page_index,
            page.inline_objects(),
            pen_multiplier,
            marker_multiplier,
            &mut media_storage,
        );

        page_ctx.draw_objects(
            page.objects(),
            pen_multiplier,
            marker_multiplier,
            &multi_progress,
            &mut media_storage,
        );
    }

    pdf.set_metadata(
        Metadata::new()
            .title(document_name.to_owned())
            .producer("sdocx2pdf".to_owned())
            .creator("Samsung Notes".to_owned()),
    );

    let pdf_bytes = pdf.finish()?;
    std::fs::write(out_path, &pdf_bytes)?;

    Ok(())
}
