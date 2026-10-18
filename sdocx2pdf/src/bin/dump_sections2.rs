use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let bt = doc.body_text();
    let raw = bt.raw_string().unwrap_or("");
    let u16_vec: Vec<u16> = raw.encode_utf16().collect();

    for (i, (start, count)) in bt.section_data().iter().enumerate() {
        let start_us = *start as usize;
        let end_us = (*start + *count) as usize;
        if start_us >= u16_vec.len() { continue; }
        let slice = &u16_vec[start_us..end_us.min(u16_vec.len())];
        let s = String::from_utf16_lossy(slice);
        println!("=== Page {i} (pos {start}-{end_us}) ===");
        println!("{}", s);
    }
}
