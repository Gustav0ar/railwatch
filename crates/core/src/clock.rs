/// CLOCK_BOOTTIME includes suspend, so resuming never integrates the sleep interval.
pub fn monotonic_ms() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let ok = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) };
    assert_eq!(ok, 0, "CLOCK_BOOTTIME unavailable");
    time.tv_sec as u64 * 1000 + time.tv_nsec as u64 / 1_000_000
}
