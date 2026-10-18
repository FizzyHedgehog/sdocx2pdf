use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let pi = 7;
    let page = &doc.pages()[pi];
    for layer in page.layers() {
        for (i, obj) in layer.objects().iter().enumerate() {
            if let sdocx::DocObject::Text(text) = obj {
                let r = obj.object_base().rect;
                let raw = text.raw_string().unwrap_or("");
                println!("Page {pi} Text[{i}]: rect=({:.0},{:.0})-({:.0},{:.0})", r.min.x, r.min.y, r.max.x, r.max.y);
                println!("  text={:?}", raw);
            }
        }
    }
}
