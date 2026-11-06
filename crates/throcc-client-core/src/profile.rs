use std::sync::Arc;

use serde::{Deserialize, Serialize};
use throcc_proto::ids::Hash;

/// Our name and picture, kept on this computer and given to every server we join.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default, with = "base64_bytes")]
    pub picture: Option<Arc<[u8]>>,
}

impl Profile {
    pub fn avatar(&self) -> Option<Hash> {
        self.picture.as_deref().map(Hash::of)
    }
}

mod base64_bytes {
    use std::sync::Arc;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Option<Arc<[u8]>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => s.serialize_some(&STANDARD.encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<[u8]>>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|text| {
                STANDARD
                    .decode(text)
                    .map(Arc::from)
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}
