use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let bt = doc.body_text();
    let raw = bt.raw_string().unwrap_or("");
    let u16_vec: Vec<u16> = raw.encode_utf16().collect();

    // Find paragraph starts in page 7 (section [7]: start=539, count=213)
    let page_start = 539usize;
    let page_end = (539 + 213).min(u16_vec.len());
    println!("Page 7 text (pos {}-{}):", page_start, page_end);

    let mut para_start = page_start;
    let mut para_idx = 0;
    for i in page_start..page_end {
        if u16_vec[i] == 0x000A || i == page_end - 1 {
            let slice = &u16_vec[para_start..(i+1).min(page_end)];
            let s = String::from_utf16_lossy(slice);
            println!("  Para {para_idx} at pos {para_start}: {:?}", s.trim());
            para_start = i + 1;
            para_idx += 1;
        }
    }

    // Now check alignment paragraphs around these positions
    println!("\nAlignment paragraphs near page 7:");
    if let Some(common) = bt.text_common() {
        for p in common.paragraphs() {
            let t = format!("{:?}", p.paragraph_base.paragraph_type);
            if t == "Alignment" {
                let start = p.paragraph_base.start_pos;
                if start >= 500 && start <= 800 {
                    let v = p.bytes.get(0..4).map(|b| u32::from_le_bytes([b[0],b[1],b[2],b[3]])).unwrap_or(999);
                    println!("  pos={start} val={v}");
                }
            }
        }
    }
}
