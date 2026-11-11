use std::fs;

#[cfg(test)]
mod tests;

struct Footprint {
    ram_kib: u64,
    kernel_kib: u64,
    initramfs_kib: u64,
    tasks_kib: u64,
    other_kib: u64,
    free_kib: u64,
}

pub(crate) fn report_boot() {
    eprintln!("{}", report_text(None));
}

pub(crate) fn report_sent(path: &str) {
    eprintln!("{}", report_text(Some(path)));
}

fn report_text(sent: Option<&str>) -> String {
    match (
        read_text("/proc/meminfo"),
        read_text("/proc/zoneinfo"),
        page_kib(),
    ) {
        (Ok(meminfo), Ok(zoneinfo), Ok(page_kib)) => {
            memory_line(&meminfo, &zoneinfo, page_kib, sent)
        }
        (Err(reason), _, _) | (_, Err(reason), _) | (_, _, Err(reason)) => {
            with_sent(format!("report failed: {reason}"), sent)
        }
    }
}

fn read_text(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))
}

fn page_kib() -> Result<u64, String> {
    // SAFETY: `_SC_PAGESIZE` takes no pointer and only reads a process constant.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return Err("page size is unavailable".to_string());
    }
    let page_size = u64::try_from(page_size).map_err(|_| "page size is unavailable".to_string())?;
    if page_size % 1024 != 0 {
        return Err("page size is not a multiple of 1 KiB".to_string());
    }
    Ok(page_size / 1024)
}

fn footprint_text(meminfo: &str, zoneinfo: &str, page_kib: u64) -> String {
    match footprint(meminfo, zoneinfo, page_kib) {
        Ok(footprint) => format!(
            "ram {}, kernel {}, initramfs {}, tasks {}, other {}, free {}",
            format_mib(footprint.ram_kib),
            format_mib(footprint.kernel_kib),
            format_mib(footprint.initramfs_kib),
            format_mib(footprint.tasks_kib),
            format_mib(footprint.other_kib),
            format_mib(footprint.free_kib),
        ),
        Err(reason) => format!("report failed: {reason}"),
    }
}

fn memory_line(meminfo: &str, zoneinfo: &str, page_kib: u64, sent: Option<&str>) -> String {
    with_sent(footprint_text(meminfo, zoneinfo, page_kib), sent)
}

fn with_sent(detail: String, sent: Option<&str>) -> String {
    match sent {
        Some(path) => format!("memory: {detail}, sent {path}"),
        None => format!("memory: {detail}"),
    }
}

fn footprint(meminfo: &str, zoneinfo: &str, page_kib: u64) -> Result<Footprint, String> {
    let present = zone_pages(zoneinfo, "present")?;
    let managed = zone_pages(zoneinfo, "managed")?;
    let reserved = present
        .checked_sub(managed)
        .ok_or_else(|| "kernel footprint is negative".to_string())?;
    let ram_kib = present
        .checked_mul(page_kib)
        .ok_or_else(|| "ram footprint overflowed".to_string())?;
    let sunreclaim = meminfo_kib(meminfo, "SUnreclaim")?;
    let kernel_stack = meminfo_kib(meminfo, "KernelStack")?;
    let percpu = meminfo_kib(meminfo, "Percpu")?;
    let kernel_kib = reserved
        .checked_mul(page_kib)
        .and_then(|reserved_kib| reserved_kib.checked_add(sunreclaim))
        .and_then(|sum| sum.checked_add(kernel_stack))
        .and_then(|sum| sum.checked_add(percpu))
        .ok_or_else(|| "kernel footprint overflowed".to_string())?;
    let initramfs_kib = initramfs_kib(
        meminfo_kib(meminfo, "Shmem")?,
        meminfo_kib(meminfo, "Unevictable")?,
        meminfo_kib(meminfo, "Mlocked")?,
    )?;
    let tasks_kib = meminfo_kib(meminfo, "AnonPages")?
        .checked_add(meminfo_kib(meminfo, "PageTables")?)
        .ok_or_else(|| "tasks footprint overflowed".to_string())?;
    let free_kib = meminfo_kib(meminfo, "MemAvailable")?;
    let other_kib = ram_kib
        .checked_sub(kernel_kib)
        .and_then(|left| left.checked_sub(initramfs_kib))
        .and_then(|left| left.checked_sub(tasks_kib))
        .and_then(|left| left.checked_sub(free_kib))
        .ok_or_else(|| "other footprint is negative".to_string())?;
    Ok(Footprint {
        ram_kib,
        kernel_kib,
        initramfs_kib,
        tasks_kib,
        other_kib,
        free_kib,
    })
}

fn zone_pages(text: &str, field: &str) -> Result<u64, String> {
    let mut total: Option<u64> = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(field) {
            continue;
        }
        let Some(number) = parts.next() else {
            return Err(format!("zoneinfo {field} has no value"));
        };
        let pages: u64 = number
            .parse()
            .map_err(|_| format!("zoneinfo {field} is not a number"))?;
        total = Some(match total {
            Some(sum) => sum
                .checked_add(pages)
                .ok_or_else(|| format!("zoneinfo {field} overflowed"))?,
            None => pages,
        });
    }
    total.ok_or_else(|| format!("zoneinfo has no {field}"))
}

fn meminfo_kib(text: &str, name: &str) -> Result<u64, String> {
    let label = format!("{name}:");
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(label.as_str()) {
            continue;
        }
        let Some(number) = parts.next() else {
            return Err(format!("meminfo {name} has no value"));
        };
        if parts.next() != Some("kB") {
            return Err(format!("meminfo {name} is not in kB"));
        }
        return number
            .parse()
            .map_err(|_| format!("meminfo {name} is not a number"));
    }
    Err(format!("meminfo has no {name}"))
}

fn initramfs_kib(shmem: u64, unevictable: u64, mlocked: u64) -> Result<u64, String> {
    let ramfs = unevictable
        .checked_sub(mlocked)
        .ok_or_else(|| "initramfs footprint is negative".to_string())?;
    shmem
        .checked_add(ramfs)
        .ok_or_else(|| "initramfs footprint overflowed".to_string())
}

fn format_mib(kib: u64) -> String {
    let tenths = (u128::from(kib) * 10 + 512) / 1024;
    format!("{}.{} MiB", tenths / 10, tenths % 10)
}
