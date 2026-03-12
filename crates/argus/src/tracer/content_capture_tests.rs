use super::*;
use crate::cas::LocalCas;

fn test_cas() -> LocalCas {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cas");
    // Prevent cleanup so the CAS directory outlives the test helper.
    dir.keep();
    LocalCas::new(path).expect("LocalCas::new")
}

#[test]
fn zero_length_write_returns_none() {
    let cas = test_cas();
    let result = capture_write_buffer(&cas, Pid::from_raw(1), 0x1000, 0);
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn null_addr_write_returns_none() {
    let cas = test_cas();
    let result = capture_write_buffer(&cas, Pid::from_raw(1), 0, 100);
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn zero_iovec_count_returns_none() {
    let cas = test_cas();
    let result = capture_iovec_buffer(&cas, Pid::from_raw(1), 0x1000, 0);
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn null_iovec_addr_returns_none() {
    let cas = test_cas();
    let result = capture_iovec_buffer(&cas, Pid::from_raw(1), 0, 5);
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn try_capture_flat_null_returns_none() {
    let cas = test_cas();
    assert!(try_capture_flat(&cas, Pid::from_raw(1), 0, 100).is_none());
}

#[test]
fn try_capture_flat_zero_len_returns_none() {
    let cas = test_cas();
    assert!(
        try_capture_flat(&cas, Pid::from_raw(1), 0x1000, 0).is_none()
    );
}

#[test]
fn try_capture_iovec_null_returns_none() {
    let cas = test_cas();
    assert!(try_capture_iovec(&cas, Pid::from_raw(1), 0, 3).is_none());
}

#[test]
fn try_capture_iovec_zero_cnt_returns_none() {
    let cas = test_cas();
    assert!(
        try_capture_iovec(&cas, Pid::from_raw(1), 0x1000, 0).is_none()
    );
}

#[test]
fn max_single_read_is_16mib() {
    assert_eq!(MAX_SINGLE_READ, 16 * 1024 * 1024);
}

#[test]
fn iovec_size_matches_64bit_platform() {
    assert_eq!(IOVEC_SIZE, 16);
}

#[test]
fn try_capture_flat_bad_pid_returns_none() {
    let cas = test_cas();
    // PID 999999 almost certainly does not exist and is not traced.
    let result = try_capture_flat(&cas, Pid::from_raw(999_999), 0x1000, 8);
    assert!(result.is_none());
}

#[test]
fn try_capture_iovec_bad_pid_returns_none() {
    let cas = test_cas();
    let result =
        try_capture_iovec(&cas, Pid::from_raw(999_999), 0x1000, 2);
    assert!(result.is_none());
}

#[test]
fn iov_cnt_exceeding_max_is_rejected() {
    let cas = test_cas();
    let result =
        capture_iovec_buffer(&cas, Pid::from_raw(1), 0x1000, 1025);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("UIO_MAXIOV"));
}
