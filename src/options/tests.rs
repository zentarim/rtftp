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

#[test]
fn find_rollover_accepts_zero_and_one() {
    for value in [0, 1] {
        let mut options = HashMap::new();
        options.insert(ROLLOVER.to_string(), value.to_string());
        let rollover = Rollover::find_in(&options).unwrap();
        assert_eq!(
            rollover.as_key_pair(),
            (ROLLOVER.to_string(), value.to_string())
        );
    }
}

#[test]
fn find_rollover_rejects_other_values() {
    for value in ["2", "65535", "abc"] {
        let mut options = HashMap::new();
        options.insert(ROLLOVER.to_string(), value.to_string());
        assert!(Rollover::find_in(&options).is_none());
    }
    assert!(Rollover::find_in(&HashMap::new()).is_none());
}

#[test]
fn rollover_zero_block_numbers() {
    let rollover = Rollover::default();
    assert_eq!(rollover.block_number(0), 0);
    assert_eq!(rollover.block_number(65535), 65535);
    assert_eq!(rollover.block_number(65536), 0);
    assert_eq!(rollover.block_number(65537), 1);
}

#[test]
fn rollover_one_block_numbers() {
    let mut options = HashMap::new();
    options.insert(ROLLOVER.to_string(), "1".to_string());
    let rollover = Rollover::find_in(&options).unwrap();
    assert_eq!(rollover.block_number(0), 0);
    assert_eq!(rollover.block_number(65535), 65535);
    assert_eq!(rollover.block_number(65536), 1);
    assert_eq!(rollover.block_number(65537), 2);
    assert_eq!(rollover.block_number(131070), 65535);
    assert_eq!(rollover.block_number(131071), 1);
}

#[test]
fn rollover_one_limits_window_size() {
    let mut options = HashMap::new();
    options.insert(ROLLOVER.to_string(), "1".to_string());
    let rollover = Rollover::find_in(&options).unwrap();
    assert_eq!(Rollover::default().max_window_size(), WINDOW_SIZE_UPPER_CAP);
    assert_eq!(rollover.max_window_size(), WINDOW_SIZE_UPPER_CAP - 1);

    let mut requested = HashMap::new();
    requested.insert(WINDOW_SIZE.to_string(), WINDOW_SIZE_UPPER_CAP.to_string());
    let capped = WindowSize::find_in(&requested)
        .unwrap()
        .limited_to(rollover.max_window_size());
    assert_eq!(capped.get_size(), 65534);

    requested.insert(WINDOW_SIZE.to_string(), 10.to_string());
    let unchanged = WindowSize::find_in(&requested)
        .unwrap()
        .limited_to(rollover.max_window_size());
    assert_eq!(unchanged.get_size(), 10);
}
