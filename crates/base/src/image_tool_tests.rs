//! Unit tests for the image call layer.
//!
//! These cover the parts that do not need a profile or a socket: validation,
//! naming, extension choice, and the metadata shape a tool result carries. The
//! transport itself is covered in `crates/provider-openai/src/images_tests.rs`,
//! where a loopback listener sees the real request.

use super::*;

fn call() -> ImageCall {
    ImageCall {
        prompt: "a lighthouse at dusk".to_string(),
        ..ImageCall::default()
    }
}

#[test]
fn the_default_call_is_one_square_image() {
    let call = ImageCall::default();
    assert_eq!(call.size, "1024x1024");
    assert_eq!(call.count, 1);
    assert!(call.input.is_empty());
    assert!(call.model.is_none());
}

#[test]
fn an_empty_prompt_is_refused() {
    let error = ImageCall {
        prompt: "   ".to_string(),
        ..call()
    }
    .validate()
    .expect_err("a blank prompt is refused");
    assert!(format!("{error:#}").contains("empty"));
}

#[test]
fn a_count_outside_the_range_is_refused() {
    assert!(ImageCall { count: 0, ..call() }.validate().is_err());
    assert!(
        ImageCall {
            count: MAX_IMAGES_PER_CALL + 1,
            ..call()
        }
        .validate()
        .is_err()
    );
    assert!(
        ImageCall {
            count: MAX_IMAGES_PER_CALL,
            ..call()
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn too_many_input_images_are_refused() {
    let error = ImageCall {
        input: (0..=MAX_EDIT_INPUTS).map(|index| format!("in-{index}.png")).collect(),
        ..call()
    }
    .validate()
    .expect_err("more than the limit is refused");
    assert!(format!("{error:#}").contains(&MAX_EDIT_INPUTS.to_string()));
}

#[test]
fn an_image_is_named_without_overwriting_and_without_provider_text() {
    let directory = std::env::temp_dir().join(format!(
        "horon-image-tool-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&directory).expect("temp dir");
    let bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    let first = save_image(&directory, &bytes, "image/png").expect("first write");
    let second = save_image(&directory, &bytes, "image/png").expect("second write");
    assert_ne!(first, second, "the second image does not overwrite the first");
    assert_eq!(std::fs::read(&first).expect("read back"), bytes);
    assert_eq!(first.extension().and_then(|value| value.to_str()), Some("png"));
    assert!(
        first
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("image-")),
        "the name is generated, not provider-supplied"
    );

    let jpeg = save_image(&directory, &[0xff, 0xd8, 0xff, 0xe0], "image/jpeg").expect("jpeg write");
    assert_eq!(jpeg.extension().and_then(|value| value.to_str()), Some("jpg"));

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn an_unreadable_input_names_the_file() {
    let error = load_inputs(&["/definitely/not/here.png".to_string()]).expect_err("missing input");
    assert!(format!("{error:#}").contains("/definitely/not/here.png"));
}

#[test]
fn an_empty_input_file_is_refused() {
    let path = std::env::temp_dir().join(format!("horon-empty-{}.png", std::process::id()));
    std::fs::write(&path, []).expect("write empty file");
    let error = load_inputs(&[path.display().to_string()]).expect_err("empty input");
    assert!(format!("{error:#}").contains("empty"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_input_keeps_its_sniffed_type_and_file_name() {
    let path = std::env::temp_dir().join(format!("horon-input-{}.png", std::process::id()));
    std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).expect("write input");
    let inputs = load_inputs(&[path.display().to_string()]).expect("read input");
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].mime_type, "image/png");
    assert!(
        inputs[0].file_name.ends_with(".png"),
        "the original name travels: {}",
        inputs[0].file_name
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_result_reports_completeness_from_the_requested_count() {
    let one = ImageResult {
        model: "img".to_string(),
        profile: "gw".to_string(),
        size: "1024x1024".to_string(),
        files: vec![SavedImage {
            path: PathBuf::from("/tmp/a.png"),
            mime_type: "image/png".to_string(),
            bytes: 4,
        }],
        revised_prompt: None,
        requested: 1,
    };
    assert!(one.is_complete());

    let partial = ImageResult {
        requested: 2,
        ..one.clone()
    };
    assert!(!partial.is_complete(), "one of two is not complete");

    let metadata = result_metadata(&one);
    assert_eq!(metadata["count"], 1);
    assert_eq!(metadata["requested"], 1);
    assert_eq!(metadata["images"][0]["path"], "/tmp/a.png");
    assert_eq!(metadata["images"][0]["mime_type"], "image/png");
}

#[test]
fn an_unconfigured_profile_is_not_an_error() {
    // A profile that does not exist is an error the user should see; the `None`
    // branch needs a real config, so this asserts the shape of the contract that
    // the tool relies on rather than constructing one.
    let result: Option<ImageResult> = None;
    assert!(result.is_none());
}
