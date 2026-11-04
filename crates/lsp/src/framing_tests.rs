use super::*;
use tokio::io::BufReader;

#[tokio::test]
async fn frames_round_trip_through_the_parser() {
    let a = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
    let b = br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
    let mut bytes = encode(a);
    bytes.extend_from_slice(&encode(b));

    let mut reader = BufReader::new(bytes.as_slice());
    assert_eq!(read_message(&mut reader).await.unwrap().unwrap(), a);
    assert_eq!(read_message(&mut reader).await.unwrap().unwrap(), b);
    assert!(read_message(&mut reader).await.unwrap().is_none());
}

#[tokio::test]
async fn header_names_are_case_insensitive_and_extra_headers_are_ignored() {
    let body = br#"{"ok":true}"#;
    let raw = format!(
        "content-type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-LENGTH: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let mut reader = BufReader::new(raw.as_bytes());
    assert_eq!(read_message(&mut reader).await.unwrap().unwrap(), body);
}

#[tokio::test]
async fn a_missing_content_length_is_an_error() {
    let raw = "Content-Type: application/json\r\n\r\n{}";
    let mut reader = BufReader::new(raw.as_bytes());
    let error = read_message(&mut reader).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn an_oversized_header_block_is_refused() {
    let mut raw = vec![b'x'; MAX_HEADER_BYTES + 1];
    raw.push(b'\n');
    let mut reader = BufReader::new(raw.as_slice());
    let error = read_message(&mut reader).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn an_oversized_body_is_refused_before_reading_it() {
    let raw = format!("Content-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1);
    let mut reader = BufReader::new(raw.as_bytes());
    let error = read_message(&mut reader).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn a_truncated_body_is_an_unexpected_eof() {
    let raw = b"Content-Length: 10\r\n\r\nshort";
    let mut reader = BufReader::new(raw.as_slice());
    let error = read_message(&mut reader).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}
