const LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

fn rank(effort: &str) -> Option<usize> {
    LEVELS
        .iter()
        .position(|level| level.eq_ignore_ascii_case(effort.trim()))
}

/// Pick the highest supported effort up to both the requested effort and the cap.
/// `None` preserves the request for providers without a selectable effort ladder.
pub fn clamp_spawn_effort<'a>(
    requested: Option<&'a str>,
    maximum: &str,
    available: &[&'a str],
) -> Result<Option<&'a str>, &'static str> {
    let cap = rank(maximum).ok_or("invalid configured effort")?;
    if available.is_empty() {
        return match requested {
            Some(effort) if rank(effort).is_none_or(|level| level > cap) => {
                Err("the model cannot apply its requested effort within the ceiling")
            }
            _ => Ok(requested),
        };
    }
    let ceiling = requested.and_then(rank).map_or(cap, |level| cap.min(level));
    available
        .iter()
        .copied()
        .filter_map(|effort| {
            rank(effort)
                .filter(|level| *level <= ceiling)
                .map(|level| (level, effort))
        })
        .max_by_key(|(level, _)| *level)
        .map(|(_, effort)| Some(effort))
        .ok_or("the model has no selectable effort within the ceiling")
}
