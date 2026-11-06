/// Generation counter for the cached `/usage` snapshot.
///
/// A refresh that began before an explicit invalidation must not reinstate the
/// snapshot it read, so callers snapshot this value and compare it before
/// writing back.
static PROVIDER_USAGE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn provider_usage_generation() -> u64 {
    PROVIDER_USAGE_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}
