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
    no_split: bool,
) -> anyhow::Result<()> {
    convert_sdocx_to_pdf_with_options(
        in_path,
        out_path,
        pen_multiplier,
        marker_multiplier,
        1.0,
        auto_split,
        basic_split,
        no_split,
    )
}

pub fn convert_sdocx_to_pdf_with_options(
    in_path: &Path,
    out_path: &Path,
    pen_multiplier: f32,
    marker_multiplier: f32,
    marker_opacity: f32,
    auto_split: bool,
    basic_split: Option<BasicSplitMode>,
    no_split: bool,
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
    let pages = page::split_document_into_pages(&document, &embed_map, auto_split, basic_split, no_split).collect_vec();
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
            page.objects(),
            pen_multiplier,
            marker_multiplier,
            marker_opacity,
            &mut media_storage,
        );

        page_ctx.draw_objects(
            page.objects(),
            pen_multiplier,
            marker_multiplier,
            marker_opacity,
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

pub fn merge_pdfs(pdf_paths: &[&Path], output_path: &Path) -> anyhow::Result<()> {
    use std::sync::Arc;

    let mut merged = krilla::Document::new();

    for &path in pdf_paths {
        let pdf_bytes = std::fs::read(path)?;
        let pdf = krilla::pdf::Pdf::new(pdf_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse PDF {:?}: {e:?}", path))?;
        let pdf_arc = Arc::new(pdf);
        let pages = pdf_arc.pages();
        let page_count = pages.len();
        let pdf_doc = krilla::pdf::PdfDocument::new(Arc::clone(&pdf_arc));

        for i in 0..page_count {
            let page_crop = pages.get(i).unwrap().crop_box();
            let w = (page_crop.x1 - page_crop.x0).abs() as f32;
            let h = (page_crop.y1 - page_crop.y0).abs() as f32;
            let size = Size::from_wh(w, h).unwrap_or_else(|| Size::from_wh(595.0, 842.0).unwrap());

            let mut new_page = merged.start_page_with(PageSettings::new(size));
            new_page.surface().draw_pdf_page(&pdf_doc, size, i);
        }
    }

    let finished_bytes = merged.finish()?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, &finished_bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_two_pdfs() {
        let tmp_dir = std::env::temp_dir();
        let p1 = tmp_dir.join("test_p1.pdf");
        let p2 = tmp_dir.join("test_p2.pdf");
        let p_out = tmp_dir.join("test_merged.pdf");

        // Document 1 생성 (1페이지)
        let mut d1 = krilla::Document::new();
        d1.start_page_with(PageSettings::new(Size::from_wh(200.0, 200.0).unwrap()));
        std::fs::write(&p1, d1.finish().unwrap()).unwrap();

        // Document 2 생성 (2페이지)
        let mut d2 = krilla::Document::new();
        d2.start_page_with(PageSettings::new(Size::from_wh(300.0, 300.0).unwrap()));
        d2.start_page_with(PageSettings::new(Size::from_wh(400.0, 400.0).unwrap()));
        std::fs::write(&p2, d2.finish().unwrap()).unwrap();

        // 병합 실행
        let paths = [&p1 as &Path, &p2 as &Path];
        let res = merge_pdfs(&paths, &p_out);
        assert!(res.is_ok(), "merge_pdfs failed: {:?}", res);

        // 결과 검증: 1 + 2 = 3 페이지여야 함
        let merged_bytes = std::fs::read(&p_out).unwrap();
        let merged_pdf = krilla::pdf::Pdf::new(merged_bytes).unwrap();
        assert_eq!(merged_pdf.pages().len(), 3);

        // 정리
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
        let _ = std::fs::remove_file(p_out);
    }
}

