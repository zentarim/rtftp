use super::*;

#[test]
fn find_block_size() {
    let mut options = HashMap::new();
    options.insert(BLKSIZE.to_string(), "1468".to_string());
    let blk_size = Blksize::find_in(&options).unwrap();
    assert_eq!(blk_size.block_size, 1468);
    assert_eq!(
        blk_size.as_key_pair(),
        (BLKSIZE.to_string(), "1468".to_string())
    );
}

#[test]
fn find_tsize() {
    let mut options = HashMap::new();
    options.insert(TSIZE.to_string(), "0".to_string());
    assert!(TSize::is_requested(&options));
}

#[test]
fn find_timeout() {
    let mut options = HashMap::new();
    let timeout_value: usize = 10;
    options.insert(TIMEOUT.to_string(), timeout_value.to_string());
    let timeout = AckTimeout::find_in(&options).unwrap();
    assert_eq!(timeout.timeout, timeout_value);
}

#[test]
fn test_timeout_cap() {
    let mut options = HashMap::new();
    let upper_cap = ACK_TIMEOUT_UPPER_CAP + 1;
    options.insert(TIMEOUT.to_string(), upper_cap.to_string());
    let find_result = AckTimeout::find_in(&options);
    assert!(find_result.is_none());
}

#[test]
fn test_timeout_bottom() {
    let mut options = HashMap::new();
    let bottom_cap = ACK_TIMEOUT_BOTTOM_CAP - 1;
    options.insert(TIMEOUT.to_string(), bottom_cap.to_string());
    let find_result = AckTimeout::find_in(&options);
    assert!(find_result.is_none());
}

#[test]
fn test_block_size_bottom() {
    let mut options = HashMap::new();
    let bottom_cap = BLOCK_SIZE_BOTTOM_CAP - 1;
    options.insert(BLKSIZE.to_string(), bottom_cap.to_string());
    let find_result = Blksize::find_in(&options);
    assert!(find_result.is_none());
}

#[test]
fn test_block_size_cap() {
    let mut options = HashMap::new();
    let upper_cap = BLOCK_SIZE_UPPER_CAP + 1;
    options.insert(BLKSIZE.to_string(), upper_cap.to_string());
    let find_result = Blksize::find_in(&options);
    assert!(find_result.is_none());
}

#[test]
fn test_window_size() {
    let mut options = HashMap::new();
    options.insert(WINDOW_SIZE.to_string(), 10.to_string());
    let find_result = WindowSize::find_in(&options);
    assert!(find_result.is_some());
}

#[test]
fn test_window_bottom() {
    let mut options = HashMap::new();
    let bottom_cap = WINDOW_SIZE_BOTTOM_CAP - 1;
    options.insert(WINDOW_SIZE.to_string(), bottom_cap.to_string());
    let find_result = WindowSize::find_in(&options);
    assert!(find_result.is_none());
}

#[test]
fn test_window_cap() {
    let mut options = HashMap::new();
    let upper_cap = WINDOW_SIZE_UPPER_CAP + 1;
    options.insert(WINDOW_SIZE.to_string(), upper_cap.to_string());
    let find_result = WindowSize::find_in(&options);
    assert!(find_result.is_none());
}
