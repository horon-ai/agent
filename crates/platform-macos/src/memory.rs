//! macOS process memory facts through `proc_pidinfo` and `getrusage`.
//!
//! Without this the runtime memory log records no resident set size on every
//! Mac, which makes "did this change help?" unanswerable from the log the
//! incident runbook points at. `libc` already exposes both calls, so no
//! dependency is added for them.

use std::mem::MaybeUninit;

use horon_agent_platform_api::ProcessMemoryFacts;

/// This process's memory facts.
pub(crate) fn process_memory_facts() -> ProcessMemoryFacts {
    let Some((rss_bytes, virtual_bytes, thread_count)) = task_info() else {
        return ProcessMemoryFacts {
            fd_count: fd_count(),
            ..ProcessMemoryFacts::default()
        };
    };
    ProcessMemoryFacts {
        rss_bytes: Some(rss_bytes),
        peak_rss_bytes: peak_rss_bytes(),
        virtual_bytes: Some(virtual_bytes),
        thread_count: Some(thread_count),
        main_stack_bytes: None,
        fd_count: fd_count(),
    }
}

/// Resident size, virtual size and thread count from `PROC_PIDTASKINFO`.
fn task_info() -> Option<(u64, u64, u64)> {
    let mut info = MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>();
    // SAFETY: `info` is a correctly sized, properly aligned, zeroed
    // `proc_taskinfo`, and `size` is exactly its size, which is what
    // `proc_pidinfo` requires for `PROC_PIDTASKINFO`.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            size as libc::c_int,
        )
    };
    if written != size as libc::c_int {
        return None;
    }
    // SAFETY: `proc_pidinfo` reported that it wrote the full struct.
    let info = unsafe { info.assume_init() };
    // A kernel thread count is never negative; the fallback keeps the field a
    // measurement instead of dropping the whole snapshot.
    let thread_count = u64::try_from(info.pti_threadnum).unwrap_or(0);
    Some((info.pti_resident_size, info.pti_virtual_size, thread_count))
}

/// Peak resident set size from `getrusage(RUSAGE_SELF)`.
///
/// `proc_taskinfo` carries no high-water mark, but `ru_maxrss` is exactly that;
/// on macOS it is reported in bytes.
fn peak_rss_bytes() -> Option<u64> {
    let mut usage = MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` writes a full `rusage` for `RUSAGE_SELF`.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: a zero return means the struct was filled in.
    let usage = unsafe { usage.assume_init() };
    // `ru_maxrss` is a byte count, so it is never negative.
    Some(u64::try_from(usage.ru_maxrss).unwrap_or(0))
}

/// Open descriptors via `proc_pidinfo(PROC_PIDLISTFDS)`.
///
/// The kernel returns as many `proc_fdinfo` records as fit in the buffer, so a
/// full buffer means the answer is truncated and the query is retried with room
/// for twice as many. `written` is a byte count, not a record count, and is
/// negative on failure.
fn fd_count() -> Option<u64> {
    let record = std::mem::size_of::<libc::proc_fdinfo>();
    let mut capacity = 256usize;
    for _ in 0..8 {
        let mut buffer: Vec<libc::proc_fdinfo> = Vec::with_capacity(capacity);
        let bytes = capacity.checked_mul(record)?;
        // A request larger than `c_int` can report is clamped; the kernel then
        // fills what it can and the truncation branch below retries larger.
        let size = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
        // SAFETY: the buffer has room for `capacity` records, `bytes` is
        // exactly that many bytes, and `proc_fdinfo` is plain old data.
        let written = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                size,
            )
        };
        if written < 0 {
            return None;
        }
        // `written` is non-negative here, so the conversion cannot fail.
        let written = usize::try_from(written).unwrap_or(0);
        if written < bytes {
            return Some((written / record) as u64);
        }
        capacity = capacity.checked_mul(2)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_real_memory() {
        let facts = process_memory_facts();
        let rss = facts.rss_bytes.expect("rss_bytes present on macOS");
        assert!(rss > 0, "rss_bytes must be a real measurement, got {rss}");
        assert!(
            facts.peak_rss_bytes.is_some_and(|peak| peak >= rss),
            "peak rss must be at least the current rss"
        );
        assert!(facts.virtual_bytes.is_some_and(|virtual_bytes| virtual_bytes >= rss));
        assert!(facts.thread_count.is_some_and(|threads| threads >= 1));
        assert!(
            facts.fd_count.is_some_and(|fds| fds >= 3),
            "a live process holds at least stdin, stdout and stderr"
        );
    }

    #[test]
    fn fd_count_sees_descriptors_this_test_holds() {
        // A lower bound rather than a delta: other test threads share the
        // process's descriptor table, so only "at least these are open" is
        // deterministic.
        let held: Vec<std::fs::File> = (0..8)
            .map(|_| std::fs::File::open("/dev/null").expect("open /dev/null"))
            .collect();
        let count = fd_count().expect("fd count available on macOS");
        assert!(
            count >= held.len() as u64,
            "the process must report at least the {0} descriptors this test holds, got {count}",
            held.len()
        );
    }
}
