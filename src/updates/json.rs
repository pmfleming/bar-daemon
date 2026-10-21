//! Measure serialized catalog JSON without retaining a second copy of it.
use serde::Serialize;
use std::{collections::hash_map::DefaultHasher, hash::Hasher, io};

#[derive(Default)]
pub(super) struct Measure {
    pub hash: DefaultHasher,
    pub bytes: usize,
}
impl Measure {
    pub fn of(value: &impl Serialize) -> serde_json::Result<Self> {
        let mut measure = Self::default();
        serde_json::to_writer(&mut measure, value)?;
        Ok(measure)
    }
}
impl io::Write for Measure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.hash.write(bytes);
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Measure;
    use serde::{Serialize, Serializer, ser::Error};
    use std::{collections::hash_map::DefaultHasher, hash::Hasher};

    #[test]
    fn streamed_measurement_matches_bytes_including_unicode_and_escaping() {
        for value in [
            serde_json::json!(null),
            serde_json::json!({"text":"é\n\"\\", "array":[1,true,null]}),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap();
            let mut hash = DefaultHasher::new();
            hash.write(&bytes);
            let measured = Measure::of(&value).unwrap();
            assert_eq!(measured.bytes, bytes.len());
            assert_eq!(measured.hash.finish(), hash.finish());
        }
    }

    #[test]
    fn serialization_errors_are_not_empty_fingerprints() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(S::Error::custom("invalid test value"))
            }
        }
        assert!(Measure::of(&Invalid).is_err());
    }
}
