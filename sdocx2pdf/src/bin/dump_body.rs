use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let raw = doc.body_text().raw_string().unwrap_or("");
    println!("Body text length: {} chars", raw.len());
    println!("Body text (first 500 chars): {:?}", &raw[..raw.len().min(500)]);

    // Print char positions around alignment changes
    let u16_vec: Vec<u16> = raw.encode_utf16().collect();
    for pos in [0, 150, 160, 170, 175, 180, 185, 190, 195, 200] {
        let slice = &u16_vec[pos..(pos+30).min(u16_vec.len())];
        let s = String::from_utf16_lossy(slice);
        println!("pos={}: {:?}", pos, s);
    }
}
