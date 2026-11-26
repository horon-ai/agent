use super::*;

/// A PNG of `width` by `height` pixels, one colour.
fn png(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 40]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(picture)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("encode");
    bytes
}

fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|span| span.content.as_ref()).collect()
}

#[test]
fn a_send_writes_the_skill_and_image_markers_into_the_line() {
    let mut attachments = Attachments::default();
    attachments.set_skill("hyper");
    let first = attachments.add_image(Image::from_bytes(png(4, 2)).expect("image"));
    let second = attachments.add_image(Image::from_bytes(png(2, 4)).expect("image"));
    assert_eq!((first, second), (1, 2));

    assert_eq!(
        attachments.compose("  fix this  "),
        "/skill:hyper [Image #1] [Image #2] fix this"
    );
    assert!(attachments.is_empty(), "a send empties the draft's blocks");
    // The images outlive the draft: the queued line still names them.
    let images = attachments.images_for("/skill:hyper [Image #1] [Image #2] fix this");
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].0, "image/png");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&images[0].1)
            .expect("base64"),
        png(4, 2)
    );

    // A skill alone, and images alone.
    attachments.set_skill("review");
    assert_eq!(attachments.compose(""), "/skill:review");
    attachments.add_image(Image::from_bytes(png(1, 1)).expect("image"));
    assert_eq!(attachments.compose(""), "[Image #3]");
}

#[test]
fn markers_name_each_known_image_once() {
    let mut attachments = Attachments::default();
    attachments.add_image(Image::from_bytes(png(1, 1)).expect("image"));
    attachments.clear();
    assert_eq!(attachments.images_for("[Image #1] and [Image #1] again").len(), 1);
    assert!(attachments.images_for("[Image #9] [Image #] [Image #1x]").is_empty());
}

#[test]
fn backspace_takes_the_newest_image_before_the_skill() {
    let mut attachments = Attachments::default();
    attachments.set_skill("hyper");
    attachments.add_image(Image::from_bytes(png(1, 1)).expect("image"));
    assert!(attachments.pop());
    assert_eq!(attachments.draft_images(), &[] as &[u32]);
    assert_eq!(attachments.skill(), Some("hyper"));
    assert!(attachments.pop());
    assert!(attachments.is_empty());
    assert!(!attachments.pop(), "an empty draft has nothing to take");
}

#[test]
fn blocks_are_drawn_before_the_draft() {
    let mut attachments = Attachments::default();
    attachments.set_skill("hyper");
    attachments.add_image(Image::from_bytes(png(1, 1)).expect("image"));
    let drawn: String = attachments
        .chip_spans()
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(drawn, "✦ hyper 📷 #1 ");
    assert_eq!(attachments.chip_cells(), 14);
}

#[test]
fn a_card_shows_the_number_the_picture_and_the_size() {
    let mut attachments = Attachments::default();
    assert_eq!(attachments.panel_rows(), 0, "no image, no panel");
    attachments.add_image(Image::from_bytes(png(1568, 384)).expect("image"));
    attachments.add_image(Image::from_bytes(png(526, 200)).expect("image"));
    assert_eq!(attachments.panel_rows(), PANEL_ROWS);

    let lines = attachments.panel_lines(80);
    assert_eq!(lines.len(), usize::from(PANEL_ROWS));
    let top = text(&lines[0]);
    assert!(top.contains("📷 #1") && top.contains("📷 #2"), "{top}");
    let bottom = text(&lines[lines.len() - 1]);
    assert!(bottom.contains("1568x384") && bottom.contains("526x200"), "{bottom}");
    for line in &lines {
        assert_eq!(text(line).width(), 2 * CARD_WIDTH + CARD_GAP, "{:?}", text(line));
    }
    // The wide picture fills the card's width; its rows are drawn in colour.
    assert!(text(&lines[2]).contains('▀'));

    // A pane too narrow for the second card shows only the first.
    let narrow = attachments.panel_lines(30);
    assert!(!text(&narrow[0]).contains("#2"));
}

#[test]
fn a_large_or_unusual_image_is_sent_as_a_png_within_the_cap() {
    let image = Image::from_bytes(png(3000, 10)).expect("image");
    assert_eq!(
        (image.width, image.height),
        (3000, 10),
        "the card names the pasted size"
    );
    let sent = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .expect("base64");
    let decoded = image::load_from_memory(&sent).expect("decode");
    assert_eq!(decoded.width(), MAX_SIDE);
    assert!(Image::from_bytes(b"not an image".to_vec()).is_err());
}

#[test]
fn a_dropped_file_path_is_read_as_an_image() {
    let directory = tempfile::tempdir().expect("dir");
    let file = directory.path().join("my shot.PNG");
    std::fs::write(&file, png(2, 2)).expect("write");
    let spelled = file.display().to_string();

    assert_eq!(image_path(&spelled.replace(' ', "\\ ")), Some(file.clone()));
    assert_eq!(image_path(&format!("'{spelled}'")), Some(file.clone()));
    assert_eq!(
        image_path(&format!("file://{}", spelled.replace(' ', "%20"))),
        Some(file.clone())
    );
    assert_eq!(image_path(&format!("  \"{spelled}\"\n")), Some(file));

    let text_file = directory.path().join("notes.txt");
    std::fs::write(&text_file, "hi").expect("write");
    assert_eq!(image_path(&text_file.display().to_string()), None);
    assert_eq!(
        image_path(&directory.path().join("gone.png").display().to_string()),
        None
    );
    assert_eq!(image_path("look at this\nand that"), None);
}

#[test]
fn clipboard_hex_is_decoded_and_malformed_hex_is_refused() {
    assert_eq!(decode_hex("89504e47"), Ok(vec![0x89, 0x50, 0x4e, 0x47]));
    assert!(decode_hex("895").is_err());
    assert!(decode_hex("zz").is_err());
}
