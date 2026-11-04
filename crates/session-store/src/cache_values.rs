//! Bounded inline values or authenticated references to immutable original payloads.
use crate::cache_io::json_error;
use crate::index::RecordRef;
use crate::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

// Cache representation budgets, not provider context or message-count policies.
pub(crate) const INLINE_VALUE_BYTES: usize = 4096;
/// Maximum canonical bytes of message bodies a checkpoint keeps resident.
/// This is the budget the released-prefix promise is bounded by, not a
/// provider-context or message-count policy.
pub const INLINE_TAIL_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum PayloadField {
    Whole,
    Metadata,
    Message,
    Replacement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PayloadSource {
    pub record: RecordRef,
    pub field: PayloadField,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "storage", content = "value")]
pub(crate) enum CachedValue<T> {
    Inline(T),
    Original(PayloadSource),
}

impl<T: Clone + Serialize> CachedValue<T> {
    pub fn capture(value: &T, source: PayloadSource) -> Result<Self, StoreError> {
        if source.record.blob.is_none() && serde_json::to_vec(value).map_err(json_error)?.len() <= INLINE_VALUE_BYTES {
            Ok(Self::Inline(value.clone()))
        } else {
            Ok(Self::Original(source))
        }
    }
}

impl<T: Clone + DeserializeOwned> CachedValue<T> {
    pub fn resolve(&self, store: (&SessionPaths, &SessionId), probe: &ReadProbe) -> Result<T, StoreError> {
        match self {
            Self::Inline(value) => Ok(value.clone()),
            Self::Original(source) => source.resolve(store, probe),
        }
    }
}

impl PayloadSource {
    pub fn resolve<T: DeserializeOwned>(
        &self,
        store: (&SessionPaths, &SessionId),
        probe: &ReadProbe,
    ) -> Result<T, StoreError> {
        let (paths, id) = store;
        let mut payload = match &self.record.blob {
            Some(reference) => {
                let bytes = probe.read(&paths.blob_file(id, &reference.sha256)?)?;
                if reference.encoding != PayloadEncoding::CanonicalJson
                    || u64::try_from(bytes.len()).ok() != Some(reference.size)
                    || sha256_hex(&bytes) != reference.sha256
                {
                    return Err(StoreError::invalid_request(
                        StoreErrorCode::PayloadDigestMismatch,
                        "referenced payload changed",
                    ));
                }
                serde_json::from_slice(&bytes).map_err(json_error)?
            }
            None => crate::index::read_record(store, &self.record, probe)?
                .payload
                .ok_or_else(|| crate::recovery::corrupt("inline original payload absent"))?,
        };
        let field = match self.field {
            PayloadField::Whole => None,
            PayloadField::Metadata => Some("metadata"),
            PayloadField::Message => Some("message"),
            PayloadField::Replacement => Some("replacement"),
        };
        if let Some(field) = field {
            payload = payload
                .as_object_mut()
                .and_then(|v| v.remove(field))
                .ok_or_else(|| crate::recovery::corrupt("referenced payload field absent"))?;
        }
        serde_json::from_value(payload).map_err(json_error)
    }
}
