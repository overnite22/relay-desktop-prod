//! Writes a synthetic Guacamole recording (same instructions as guacd for an
//! RDP desktop: image streams, buffers, copies, filled rectangles, `sync`
//! frames) for manual tests of the replay:
//! `cargo run -p relay-desktop-core --example guac_sample -- session.guac`
use base64::Engine;

fn ins(parts: &[&str]) -> String {
    let mut s = parts
        .iter()
        .map(|p| format!("{}.{}", p.chars().count(), p))
        .collect::<Vec<_>>()
        .join(",");
    s.push(';');
    s
}

fn main() {
    let path = std::env::args().nth(1).expect("fichier de sortie");
    let img = image::RgbaImage::from_fn(320, 200, |x, y| {
        image::Rgba([(x * 255 / 320) as u8, (y * 255 / 200) as u8, 160, 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let mut out = String::new();
    let t0 = 1_795_000_000_000u64;
    out += &ins(&["size", "0", "1024", "768"]);
    out += &ins(&["rect", "0", "0", "0", "1024", "768"]);
    out += &ins(&["cfill", "14", "0", "0", "90", "160", "255"]);
    out += &ins(&["size", "-1", "40", "40"]);
    out += &ins(&["rect", "-1", "0", "0", "40", "40"]);
    out += &ins(&["cfill", "14", "-1", "250", "250", "250", "255"]);
    out += &ins(&["sync", &t0.to_string()]);
    out += &ins(&["img", "3", "12", "0", "image/png", "100", "100"]);
    for chunk in b64.as_bytes().chunks(4096) {
        out += &ins(&["blob", "3", std::str::from_utf8(chunk).unwrap()]);
    }
    out += &ins(&["end", "3"]);
    out += &ins(&["sync", &(t0 + 500).to_string()]);
    for i in 0..40u64 {
        let x = (40 + i * 22).to_string();
        out += &ins(&["copy", "-1", "0", "0", "40", "40", "12", "0", &x, "500"]);
        out += &ins(&["rect", "0", "0", "740", "1024", "28"]);
        out += &ins(&["cfill", "12", "0", "30", "30", "30", "255"]);
        out += &ins(&["rect", "0", "8", "746", &(i * 25).to_string(), "16"]);
        out += &ins(&["cfill", "12", "0", "60", "200", "90", "255"]);
        out += &ins(&["sync", &(t0 + 1000 + i * 250).to_string()]);
    }
    std::fs::write(&path, out).unwrap();
    println!("{path} : 10,75 s, 42 images");
}
