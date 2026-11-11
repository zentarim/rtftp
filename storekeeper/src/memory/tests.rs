use super::memory_line;

const PAGE_KIB: u64 = 4;

fn sample_meminfo() -> String {
    [
        "MemFree:        200000 kB",
        "MemAvailable:   220000 kB",
        "Unevictable:      3891 kB",
        "Mlocked:             0 kB",
        "Shmem:               0 kB",
        "AnonPages:        1126 kB",
        "SUnreclaim:       4200 kB",
        "KernelStack:       100 kB",
        "PageTables:        200 kB",
        "Percpu:             80 kB",
    ]
    .join("\n")
}

fn sample_zoneinfo() -> String {
    [
        "Node 0, zone      DMA",
        "        pages free     80",
        "        spanned  128",
        "        present  100",
        "        managed  80",
        "Node 0, zone    DMA32",
        "        pages free     50000",
        "        spanned  65536",
        "        present  60000",
        "        managed  59000",
        "Node 0, zone   Normal",
        "        pages free     0",
        "        spanned  0",
        "        present  0",
        "        managed  0",
    ]
    .join("\n")
}

#[test]
fn report_splits_kernel_initramfs_tasks_and_free() {
    assert_eq!(
        memory_line(
            &sample_meminfo(),
            &sample_zoneinfo(),
            PAGE_KIB,
            Some("/boot/vmlinuz")
        ),
        "memory: ram 234.8 MiB, kernel 8.3 MiB, initramfs 3.8 MiB, tasks 1.3 MiB, other 6.6 MiB, free 214.8 MiB, sent /boot/vmlinuz"
    );
}

#[test]
fn boot_report_has_no_sent_path() {
    assert_eq!(
        memory_line(&sample_meminfo(), &sample_zoneinfo(), PAGE_KIB, None),
        "memory: ram 234.8 MiB, kernel 8.3 MiB, initramfs 3.8 MiB, tasks 1.3 MiB, other 6.6 MiB, free 214.8 MiB"
    );
}

#[test]
fn boot_report_failure_has_no_sent_path() {
    let zoneinfo = sample_zoneinfo().replace("present", "resident");
    assert_eq!(
        memory_line(&sample_meminfo(), &zoneinfo, PAGE_KIB, None),
        "memory: report failed: zoneinfo has no present"
    );
}

#[test]
fn missing_counter_keeps_the_path() {
    let meminfo = sample_meminfo().replace("MemAvailable:", "Available:");
    let line = memory_line(
        &meminfo,
        &sample_zoneinfo(),
        PAGE_KIB,
        Some("/boot/vmlinuz"),
    );
    assert_eq!(
        line,
        "memory: report failed: meminfo has no MemAvailable, sent /boot/vmlinuz"
    );
}

#[test]
fn missing_present_pages_keeps_the_path() {
    let zoneinfo = sample_zoneinfo().replace("present", "resident");
    let line = memory_line(&sample_meminfo(), &zoneinfo, PAGE_KIB, Some("/file"));
    assert_eq!(
        line,
        "memory: report failed: zoneinfo has no present, sent /file"
    );
}

#[test]
fn malformed_present_pages_keeps_the_path() {
    let zoneinfo = sample_zoneinfo().replace("present  100", "present  bad");
    let line = memory_line(&sample_meminfo(), &zoneinfo, PAGE_KIB, Some("/file"));
    assert_eq!(
        line,
        "memory: report failed: zoneinfo present is not a number, sent /file"
    );
}

#[test]
fn negative_kernel_footprint_is_a_failed_report() {
    let zoneinfo = sample_zoneinfo().replace("managed  80", "managed  999999");
    let line = memory_line(&sample_meminfo(), &zoneinfo, PAGE_KIB, Some("/file"));
    assert_eq!(
        line,
        "memory: report failed: kernel footprint is negative, sent /file"
    );
}

#[test]
fn tmpfs_root_counts_as_initramfs() {
    let meminfo = sample_meminfo()
        .replace("Unevictable:      3891 kB", "Unevictable:         0 kB")
        .replace("Shmem:               0 kB", "Shmem:           28660 kB")
        .replace("MemAvailable:   220000 kB", "MemAvailable:   191340 kB");
    let line = memory_line(
        &meminfo,
        &sample_zoneinfo(),
        PAGE_KIB,
        Some("/boot/vmlinuz"),
    );
    assert!(line.contains("initramfs 28.0 MiB"), "{line}");
}

#[test]
fn negative_initramfs_footprint_is_a_failed_report() {
    let meminfo =
        sample_meminfo().replace("Mlocked:             0 kB", "Mlocked:          5000 kB");
    let line = memory_line(&meminfo, &sample_zoneinfo(), PAGE_KIB, Some("/file"));
    assert_eq!(
        line,
        "memory: report failed: initramfs footprint is negative, sent /file"
    );
}

#[test]
fn negative_other_footprint_is_a_failed_report() {
    let meminfo =
        sample_meminfo().replace("MemAvailable:   220000 kB", "MemAvailable:   999999 kB");
    let line = memory_line(&meminfo, &sample_zoneinfo(), PAGE_KIB, Some("/file"));
    assert_eq!(
        line,
        "memory: report failed: other footprint is negative, sent /file"
    );
}

#[test]
fn mib_rounds_half_up() {
    let meminfo = [
        "MemAvailable:     1024 kB",
        "Unevictable:         0 kB",
        "Mlocked:             0 kB",
        "Shmem:               0 kB",
        "AnonPages:        1280 kB",
        "SUnreclaim:          0 kB",
        "KernelStack:         0 kB",
        "PageTables:          0 kB",
        "Percpu:              0 kB",
    ]
    .join("\n");
    let zoneinfo = ["        present  1024", "        managed  1024"].join("\n");
    let line = memory_line(&meminfo, &zoneinfo, PAGE_KIB, Some("/file"));
    assert_eq!(
        line,
        "memory: ram 4.0 MiB, kernel 0.0 MiB, initramfs 0.0 MiB, tasks 1.3 MiB, other 1.8 MiB, free 1.0 MiB, sent /file"
    );
}
