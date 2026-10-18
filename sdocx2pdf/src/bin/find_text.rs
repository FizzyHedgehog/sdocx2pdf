use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let target = "여기서부터는 문단 정렬";

    // Search body text
    if let Some(raw) = doc.body_text().raw_string() {
        if raw.contains(target) {
            println!("FOUND in body text");
        }
    }

    // Search all text boxes on all pages
    for (pi, page) in doc.pages().iter().enumerate() {
        for layer in page.layers() {
            for obj in layer.objects() {
                if let sdocx::DocObject::Text(text) = obj {
                    if let Some(raw) = text.raw_string() {
                        if raw.contains(target) {
                            let r = obj.object_base().rect;
                            println!("FOUND in page {pi} text box at ({:.0},{:.0})-({:.0},{:.0})", r.min.x, r.min.y, r.max.x, r.max.y);
                            println!("  text={:?}", raw);
                        }
                    }
                }
            }
        }
    }
}
