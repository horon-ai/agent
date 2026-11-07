//! Process-global bus slot.

use crate::bus::IrcBus;
use std::sync::{Arc, RwLock};

static GLOBAL: RwLock<Option<Arc<IrcBus>>> = RwLock::new(None);

/// Install `bus`, replacing any previous one.
pub fn install(bus: Arc<IrcBus>) {
    let mut slot = match GLOBAL.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = Some(bus);
}

/// The installed bus, if any.
pub fn get() -> Option<Arc<IrcBus>> {
    let slot = match GLOBAL.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.clone()
}

/// Remove the installed bus.
pub fn clear() {
    let mut slot = match GLOBAL.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = None;
}
