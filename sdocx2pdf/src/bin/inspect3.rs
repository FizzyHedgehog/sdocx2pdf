use sdocx::DocObject;
use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    for (pi, page) in doc.pages().iter().enumerate() {
        for layer in page.layers() {
            for obj in layer.objects() {
                let ty: &str = obj.into();
                let b = obj.object_base();
                let angle = b.angle();
                let deg = angle.map(|a| a.to_degrees());
                println!(
                    "p{pi} {ty:8} rect=({:.1},{:.1})-({:.1},{:.1}) angle={:?} ({:?}°)",
                    b.rect.min.x, b.rect.min.y, b.rect.max.x, b.rect.max.y, angle, deg
                );
                if let DocObject::Text(text) = obj {
                    println!("      raw={:?}", text.raw_string().unwrap_or(""));
                }
            }
        }
    }
}
