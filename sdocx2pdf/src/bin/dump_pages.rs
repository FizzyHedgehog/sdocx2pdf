use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    println!("Pages: {}", doc.pages().len());
    println!("Page model: {:?}", doc.page_model());
    println!("Body text chars: {}", doc.body_text().raw_string().unwrap_or("").len());

    for (pi, page) in doc.pages().iter().enumerate() {
        let mut text_count = 0;
        let mut stroke_count = 0;
        let mut other_count = 0;
        for layer in page.layers() {
            for obj in layer.objects() {
                match obj {
                    sdocx::DocObject::Text(_) => text_count += 1,
                    sdocx::DocObject::Stroke(_) => stroke_count += 1,
                    _ => other_count += 1,
                }
            }
        }
        println!("Page {pi}: texts={text_count} strokes={stroke_count} other={other_count}");
    }
}
