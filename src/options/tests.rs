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

fn negotiate_timeout(entries: &[(&str, &str)]) -> (Duration, HashMap<String, String>) {
    let options = entries
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let (ack_timeout, pairs) = AckTimeout::negotiate(&options);
    (ack_timeout.timeout, pairs.into_iter().collect())
}

#[test]
fn find_timeout() {
    let (timeout, pairs) = negotiate_timeout(&[(TIMEOUT, "10")]);
    assert_eq!(timeout, Duration::from_secs(10));
    assert_eq!(
        pairs,
        HashMap::from([(TIMEOUT.to_string(), "10".to_string())])
    );
}

#[test]
fn test_timeout_cap() {
    let upper_cap = (ACK_TIMEOUT_UPPER_CAP + 1).to_string();
    let (timeout, pairs) = negotiate_timeout(&[(TIMEOUT, &upper_cap)]);
    assert_eq!(timeout, Duration::from_secs(5));
    assert!(pairs.is_empty());
}

#[test]
fn test_timeout_bottom() {
    let bottom_cap = (ACK_TIMEOUT_BOTTOM_CAP - 1).to_string();
    let (timeout, pairs) = negotiate_timeout(&[(TIMEOUT, &bottom_cap)]);
    assert_eq!(timeout, Duration::from_secs(5));
    assert!(pairs.is_empty());
}

#[test]
fn utimeout_alone_sets_microseconds() {
    let (timeout, pairs) = negotiate_timeout(&[(UTIMEOUT, "500000")]);
    assert_eq!(timeout, Duration::from_micros(500_000));
    assert_eq!(
        pairs,
        HashMap::from([(UTIMEOUT.to_string(), "500000".to_string())])
    );
}

#[test]
fn matching_timeout_and_utimeout_are_both_echoed() {
    let (timeout, pairs) = negotiate_timeout(&[(TIMEOUT, "2"), (UTIMEOUT, "2000000")]);
    assert_eq!(timeout, Duration::from_micros(2_000_000));
    assert_eq!(
        pairs,
        HashMap::from([
            (TIMEOUT.to_string(), "2".to_string()),
            (UTIMEOUT.to_string(), "2000000".to_string()),
        ])
    );
}

#[test]
fn utimeout_overrides_a_different_timeout() {
    let (timeout, pairs) = negotiate_timeout(&[(TIMEOUT, "2"), (UTIMEOUT, "500000")]);
    assert_eq!(timeout, Duration::from_micros(500_000));
    assert_eq!(
        pairs,
        HashMap::from([(UTIMEOUT.to_string(), "500000".to_string())])
    );
}

#[test]
fn invalid_utimeout_falls_back_to_timeout() {
    let (timeout, pairs) = negotiate_timeout(&[(TIMEOUT, "2"), (UTIMEOUT, "5000")]);
    assert_eq!(timeout, Duration::from_secs(2));
    assert_eq!(
        pairs,
        HashMap::from([(TIMEOUT.to_string(), "2".to_string())])
    );
}

#[test]
fn invalid_timeout_and_utimeout_use_the_default() {
    let (timeout, pairs) = negotiate_timeout(&[(TIMEOUT, "0"), (UTIMEOUT, "abc")]);
    assert_eq!(timeout, Duration::from_secs(5));
    assert!(pairs.is_empty());
}

#[test]
fn absent_timeout_options_use_the_default() {
    let (timeout, pairs) = negotiate_timeout(&[]);
    assert_eq!(timeout, Duration::from_secs(5));
    assert!(pairs.is_empty());
}

#[test]
fn utimeout_accepts_range_boundaries() {
    for value in [UTIMEOUT_BOTTOM_CAP, UTIMEOUT_UPPER_CAP] {
        let text = value.to_string();
        let (timeout, pairs) = negotiate_timeout(&[(UTIMEOUT, &text)]);
        assert_eq!(timeout, Duration::from_micros(value));
        assert_eq!(pairs, HashMap::from([(UTIMEOUT.to_string(), text)]));
    }
}

#[test]
fn utimeout_rejects_values_outside_the_range() {
    for value in [UTIMEOUT_BOTTOM_CAP - 1, UTIMEOUT_UPPER_CAP + 1] {
        let text = value.to_string();
        let (timeout, pairs) = negotiate_timeout(&[(UTIMEOUT, &text)]);
        assert_eq!(timeout, Duration::from_secs(5));
        assert!(pairs.is_empty());
    }
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
