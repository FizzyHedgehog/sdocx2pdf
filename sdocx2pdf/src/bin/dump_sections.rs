use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (doc, _media) = sdocx::Document::from_zip(Path::new(&path))
        .or_else(|_| sdocx::Document::from_dir(Path::new(&path)))
        .unwrap();

    let bt = doc.body_text();
    let raw = bt.raw_string().unwrap_or("");
    let u16_vec: Vec<u16> = raw.encode_utf16().collect();

    println!("Body text length: {} UTF-16 units", u16_vec.len());
    println!("Section data entries: {}", bt.section_data().len());
    for (i, (start, count_or_end)) in bt.section_data().iter().enumerate() {
        println!("  [{i}]: start={start} count/end={count_or_end}");
        let start_us = *start as usize;
        let end_us = (*start + *count_or_end) as usize;
        if start_us < u16_vec.len() {
            let slice = &u16_vec[start_us..end_us.min(u16_vec.len())];
            let s = String::from_utf16_lossy(slice);
            let chars: Vec<char> = s.chars().collect();
            println!("       text (as count): {:?}", chars[..chars.len().min(40)].iter().collect::<String>());
        }
        if (*count_or_end as usize) < u16_vec.len() && *count_or_end > *start {
            let slice2 = &u16_vec[start_us..(*count_or_end as usize).min(u16_vec.len())];
            let s2 = String::from_utf16_lossy(slice2);
            let chars2: Vec<char> = s2.chars().collect();
            println!("       text (as end):   {:?}", chars2[..chars2.len().min(40)].iter().collect::<String>());
        }
    }
}
