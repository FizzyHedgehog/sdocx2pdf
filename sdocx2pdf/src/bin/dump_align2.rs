use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let bt = doc.body_text();
    let raw = bt.raw_string().unwrap_or("");
    let u16_vec: Vec<u16> = raw.encode_utf16().collect();

    println!("All body text alignment paragraphs:");
    if let Some(common) = bt.text_common() {
        for (i, p) in common.paragraphs().iter().enumerate() {
            let t = format!("{:?}", p.paragraph_base.paragraph_type);
            if t == "Alignment" {
                let v = p.bytes.get(0..4).map(|b| u32::from_le_bytes([b[0],b[1],b[2],b[3]])).unwrap_or(999);
                let start = p.paragraph_base.start_pos as usize;
                let end = p.paragraph_base.end_pos as usize;
                let slice = &u16_vec[start..end.min(u16_vec.len())];
                let s = String::from_utf16_lossy(slice);
                println!("  {i}: range={start}-{end} val={v} text={s:?}");
            }
        }
    }

    println!("\nSection data (page breaks):");
    for (start, end) in bt.section_data() {
        println!("  page range: {start}-{end}");
        let slice = &u16_vec[*start as usize..(*end as usize).min(u16_vec.len())];
        let s = String::from_utf16_lossy(slice);
        println!("    text={:?}", &s[..s.len().min(60)]);
    }
}
