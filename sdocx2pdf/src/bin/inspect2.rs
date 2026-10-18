use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    println!("model={:?} pages={}", doc.page_model(), doc.pages().len());
    println!("body: {:?}", doc.body_text().raw_string().unwrap_or("").chars().take(400).collect::<String>());

    for (pi, page) in doc.pages().iter().enumerate() {
        let mut pens: BTreeMap<String, usize> = BTreeMap::new();
        let mut lines: BTreeMap<String, usize> = BTreeMap::new();
        let mut other: Vec<String> = Vec::new();
        for layer in page.layers() {
            for obj in layer.objects() {
                if let sdocx::DocObject::Stroke(s) = obj {
                    let n = s.pen_name().map(|n| n.to_string()).unwrap_or_default();
                    let short = n.rsplit('.').next().unwrap_or(&n).to_string();
                    *pens.entry(short).or_default() += 1;
                    *lines.entry(format!("{:?}", s.line_type())).or_default() += 1;
                } else {
                    let ty: &str = obj.into();
                    other.push(ty.to_string());
                }
            }
        }
        println!(
            "p{pi}: pens={pens:?} lines={lines:?} other={other:?}"
        );
    }
}
