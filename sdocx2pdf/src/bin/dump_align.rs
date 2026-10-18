use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let bt = doc.body_text();
    let Some(common) = bt.text_common() else {
        println!("no text_common");
        return;
    };
    println!("Body text alignment paragraphs:");
    for (i, p) in common.paragraphs().iter().enumerate() {
        let t = format!("{:?}", p.paragraph_base.paragraph_type);
        if t == "Alignment" {
            let v = p.bytes.get(0..4).map(|b| u32::from_le_bytes([b[0],b[1],b[2],b[3]])).unwrap_or(999);
            println!("  {i}: range={}-{} val={v}", p.paragraph_base.start_pos, p.paragraph_base.end_pos);
        }
    }

    for (pi, page) in doc.pages().iter().enumerate() {
        for layer in page.layers() {
            for obj in layer.objects() {
                if let sdocx::DocObject::Text(text) = obj {
                    let r = obj.object_base().rect;
                    println!("\nPage {pi} Text box rect=({:.0},{:.0})-({:.0},{:.0})", r.min.x, r.min.y, r.max.x, r.max.y);
                    if let Some(common) = text.text_common() {
                        println!("  alignment paragraphs:");
                        for (i, p) in common.paragraphs().iter().enumerate() {
                            let t = format!("{:?}", p.paragraph_base.paragraph_type);
                            if t == "Alignment" {
                                let v = p.bytes.get(0..4).map(|b| u32::from_le_bytes([b[0],b[1],b[2],b[3]])).unwrap_or(999);
                                println!("    {i}: range={}-{} val={v}", p.paragraph_base.start_pos, p.paragraph_base.end_pos);
                            }
                        }
                    }
                    let raw = text.raw_string().unwrap_or("");
                    println!("  text={:?}", &raw[..raw.len().min(80)]);
                }
            }
        }
    }
}
