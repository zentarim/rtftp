use super::*;

fn negotiate_blksize(entries: &[(&str, &str)]) -> (usize, HashMap<String, String>) {
    let options = entries
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let (blksize, pairs) = Blksize::negotiate(&options);
    (blksize.get_size(), pairs.into_iter().collect())
}

#[test]
fn find_block_size() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, "1468")]);
    assert_eq!(block_size, 1468);
    assert_eq!(
        pairs,
        HashMap::from([(BLKSIZE.to_string(), "1468".to_string())])
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

fn negotiated_ack_timeout(entries: &[(&str, &str)]) -> AckTimeout {
    let options = entries
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    AckTimeout::negotiate(&options).0
}

fn assert_backoff(entries: &[(&str, &str)], expected: [Duration; 5]) {
    let ack_timeout = negotiated_ack_timeout(entries);
    for (index, expected_wait) in expected.into_iter().enumerate() {
        let attempt = u16::try_from(index + 1).unwrap();
        assert_eq!(ack_timeout.duration(attempt), expected_wait);
    }
}

#[test]
fn backoff_doubles_a_five_second_timeout() {
    assert_backoff(
        &[(TIMEOUT, "5")],
        [5, 10, 20, 40, 80].map(Duration::from_secs),
    );
}

#[test]
fn backoff_clamps_long_timeouts_at_255_seconds() {
    for (seconds, expected) in [
        (128, [128, 255, 255, 255, 255]),
        (200, [200, 255, 255, 255, 255]),
        (255, [255, 255, 255, 255, 255]),
    ] {
        let text = seconds.to_string();
        assert_backoff(&[(TIMEOUT, &text)], expected.map(Duration::from_secs));
    }
}

#[test]
fn backoff_doubles_a_50_millisecond_utimeout() {
    assert_backoff(
        &[(UTIMEOUT, "50000")],
        [50, 100, 200, 400, 800].map(Duration::from_millis),
    );
}

#[test]
fn backoff_doubles_utimeout_range_boundaries() {
    assert_backoff(
        &[(UTIMEOUT, "10000")],
        [10, 20, 40, 80, 160].map(Duration::from_millis),
    );
    assert_backoff(
        &[(UTIMEOUT, "255000000")],
        [255; 5].map(Duration::from_secs),
    );
}

#[test]
fn test_block_size_bottom() {
    let bottom_cap = (BLOCK_SIZE_BOTTOM_CAP - 1).to_string();
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, &bottom_cap)]);
    assert_eq!(block_size, 512);
    assert!(pairs.is_empty());
}

#[test]
fn test_block_size_cap() {
    let upper_cap = (BLOCK_SIZE_UPPER_CAP + 1).to_string();
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, &upper_cap)]);
    assert_eq!(block_size, 512);
    assert!(pairs.is_empty());
}

#[test]
fn absent_blksize_options_use_the_default() {
    let (block_size, pairs) = negotiate_blksize(&[]);
    assert_eq!(block_size, 512);
    assert!(pairs.is_empty());
}

#[test]
fn blksize2_rounds_down_to_a_power_of_two() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE2, "1468")]);
    assert_eq!(block_size, 1024);
    assert_eq!(
        pairs,
        HashMap::from([(BLKSIZE2.to_string(), "1024".to_string())])
    );
}

#[test]
fn blksize2_caps_large_requests_at_32768() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE2, "100000")]);
    assert_eq!(block_size, 32768);
    assert_eq!(
        pairs,
        HashMap::from([(BLKSIZE2.to_string(), "32768".to_string())])
    );
}

#[test]
fn blksize2_accepts_eight_and_rejects_smaller_or_unparsed_values() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE2, "8")]);
    assert_eq!(block_size, 8);
    assert_eq!(
        pairs,
        HashMap::from([(BLKSIZE2.to_string(), "8".to_string())])
    );

    for value in ["7", "abc"] {
        let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE2, value)]);
        assert_eq!(block_size, 512);
        assert!(pairs.is_empty());
    }
}

#[test]
fn blksize_rejects_blksize2_when_the_agreed_size_is_not_a_power_of_two() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, "1468"), (BLKSIZE2, "1468")]);
    assert_eq!(block_size, 1468);
    assert_eq!(
        pairs,
        HashMap::from([(BLKSIZE.to_string(), "1468".to_string())])
    );
}

#[test]
fn larger_blksize2_drops_a_smaller_blksize() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, "1024"), (BLKSIZE2, "5000")]);
    assert_eq!(block_size, 4096);
    assert_eq!(
        pairs,
        HashMap::from([(BLKSIZE2.to_string(), "4096".to_string())])
    );
}

#[test]
fn matching_power_of_two_confirms_both_blksize_options() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, "4096"), (BLKSIZE2, "5000")]);
    assert_eq!(block_size, 4096);
    assert_eq!(
        pairs,
        HashMap::from([
            (BLKSIZE.to_string(), "4096".to_string()),
            (BLKSIZE2.to_string(), "4096".to_string()),
        ])
    );
}

#[test]
fn equal_512_confirms_both_blksize_options() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, "512"), (BLKSIZE2, "512")]);
    assert_eq!(block_size, 512);
    assert_eq!(
        pairs,
        HashMap::from([
            (BLKSIZE.to_string(), "512".to_string()),
            (BLKSIZE2.to_string(), "512".to_string()),
        ])
    );
}

#[test]
fn out_of_range_blksize_is_ignored_beside_blksize2() {
    let (block_size, pairs) = negotiate_blksize(&[(BLKSIZE, "70000"), (BLKSIZE2, "4096")]);
    assert_eq!(block_size, 4096);
    assert_eq!(
        pairs,
        HashMap::from([(BLKSIZE2.to_string(), "4096".to_string())])
    );
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
