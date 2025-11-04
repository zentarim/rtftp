use super::*;

#[test]
fn extract_ushort() {
    let buffer: Vec<u8> = vec![0x00, 0x0A, 0x00, 0x00, 0x00, 0xab, 0xcd, 0xef];
    let mut cursor = ReadCursor::new(&buffer);
    let result = cursor.extract_ushort();
    assert_eq!(result.unwrap(), 0x0A);
}

#[test]
fn extract_ushort_not_enough_data() {
    let buffer: Vec<u8> = vec![0x00, 0x0A, 0xFF];
    let mut cursor = ReadCursor::new(&buffer);
    cursor.extract_ushort().unwrap();
    let result = cursor.extract_ushort();
    assert!(matches!(result.unwrap_err(), ParseError::NotEnoughData));
}

#[test]
fn extract_string() {
    let buffer: Vec<u8> = b"Arbitrary_string\x00\x0A".to_vec();
    let mut cursor = ReadCursor::new(&buffer);
    let result = cursor.extract_string();
    assert_eq!(result.unwrap(), "Arbitrary_string");
}

#[test]
fn extract_string_not_enough_data() {
    let buffer: Vec<u8> = b"Arbitrary_string\x00".to_vec();
    let mut cursor = ReadCursor::new(&buffer);
    let result = cursor.extract_string();
    assert_eq!(result.unwrap(), "Arbitrary_string");
    let error = cursor.extract_string();
    assert!(matches!(error.unwrap_err(), ParseError::NotEnoughData));
}

#[test]
fn extract_string_non_utf() {
    let buffer: Vec<u8> = b"Arbitrary_\xFFstring\x00\x0A".to_vec();
    let mut cursor = ReadCursor::new(&buffer);
    let result = cursor.extract_string();
    assert!(matches!(result.unwrap_err(), ParseError::Generic(_)));
}

#[test]
fn extract_non_terminated_string() {
    let buffer: Vec<u8> = b"Arbitrary_string".to_vec();
    let mut cursor = ReadCursor::new(&buffer);
    let result = cursor.extract_string();
    assert!(matches!(result.unwrap_err(), ParseError::Generic(_)));
}
