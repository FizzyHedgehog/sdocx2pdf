use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let bt = doc.body_text();
    let raw = bt.raw_string().unwrap_or("");
    let u16_vec: Vec<u16> = raw.encode_utf16().collect();

    println!("All body text paragraphs:");
    if let Some(common) = bt.text_common() {
        for (i, p) in common.paragraphs().iter().enumerate() {
            let start = p.paragraph_base.start_pos as usize;
            let end = p.paragraph_base.end_pos as usize;
            let t = format!("{:?}", p.paragraph_base.paragraph_type);
            let v = p.bytes.get(0..4).map(|b| u32::from_le_bytes([b[0],b[1],b[2],b[3]])).unwrap_or(999);
            let slice = &u16_vec[start..end.min(u16_vec.len())];
            let s = String::from_utf16_lossy(slice);
            println!("  {i}: type={t:20} range={start:4}-{end:4} val={v:3} text={:?}", &s[..s.len().min(20)]);
        }
    }
}
