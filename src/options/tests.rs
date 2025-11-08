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
    options.insert(TIMEOUT.to_string(), (ACK_TIMEOUT_LIMIT + 1).to_string());
    let find_result = AckTimeout::find_in(&options);
    assert!(find_result.is_none());
}

#[test]
fn test_block_size_cap() {
    let mut options = HashMap::new();
    options.insert(BLKSIZE.to_string(), (BLOCK_SIZE_LIMIT + 1).to_string());
    let find_result = Blksize::find_in(&options);
    assert!(find_result.is_none());
}
