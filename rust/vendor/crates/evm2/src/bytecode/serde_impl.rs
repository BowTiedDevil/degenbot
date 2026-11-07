use super::Bytecode;
use alloy_primitives::{Address, Bytes};
use core::fmt;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error, IgnoredAny, MapAccess, Visitor, value::MapAccessDeserializer},
};

// TODO: eventually remove `BytecodeSerdeOld`.

#[derive(Deserialize)]
#[serde(untagged)]
enum BytecodeSerde {
    New(Bytes),
    Old(BytecodeSerdeOld),
}

#[derive(Deserialize)]
enum BytecodeSerdeOld {
    LegacyAnalyzed {
        bytecode: Bytes,
        original_len: usize,
        // MessagePack encodes struct variants as arrays, so revm's trailing field must be
        // consumed.
        #[serde(default, rename = "jump_table")]
        _jump_table: IgnoredAny,
    },
    Eip7702 {
        delegated_address: Address,
    },
}

impl Serialize for Bytecode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            self.original_bytes().serialize(serializer)
        } else {
            serializer.serialize_bytes(self.original_byte_slice())
        }
    }
}

impl<'de> Deserialize<'de> for Bytecode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            match BytecodeSerde::deserialize(deserializer)? {
                BytecodeSerde::New(bytes) => Ok(Self::from_stored(bytes)),
                BytecodeSerde::Old(bytecode_serde_old) => from_old(bytecode_serde_old),
            }
        } else {
            deserializer.deserialize_bytes(BytecodeVisitor)
        }
    }
}

struct BytecodeVisitor;

impl<'de> Visitor<'de> for BytecodeVisitor {
    type Value = Bytecode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EVM bytecode")
    }

    fn visit_bytes<E: Error>(self, value: &[u8]) -> Result<Self::Value, E> {
        Ok(Bytecode::from_stored(Bytes::copy_from_slice(value)))
    }

    // Self-describing binary formats such as MessagePack may hold revm's enum encoding.
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        from_old(BytecodeSerdeOld::deserialize(MapAccessDeserializer::new(map))?)
    }
}

fn from_old<E: Error>(old: BytecodeSerdeOld) -> Result<Bytecode, E> {
    match old {
        BytecodeSerdeOld::LegacyAnalyzed { bytecode, original_len, .. } => {
            if original_len > bytecode.len() {
                return Err(E::custom("original_len is greater than bytecode length"));
            }
            Ok(Bytecode::new_legacy(bytecode.slice(..original_len)))
        }
        BytecodeSerdeOld::Eip7702 { delegated_address } => {
            Ok(Bytecode::new_eip7702(delegated_address))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::BytecodeKind;
    use alloy_primitives::{bytes, hex};

    #[test]
    fn serde_roundtrip() {
        for bytes in [&hex!()[..], &hex!("0x1234")[..]] {
            let bytecode = Bytecode::new_raw_checked(bytes.to_vec().into()).unwrap();
            let json = serde_json::to_string(&bytecode).unwrap();
            let deserialized: Bytecode = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.kind(), BytecodeKind::Legacy);
            assert_eq!(deserialized.eip7702_address(), None);
            assert_eq!(deserialized.original_byte_slice(), bytes);
        }
    }

    #[test]
    fn serde_roundtrip_legacy_with_eip7702_magic() {
        let bytes = bytes!("ef0100");
        let bytecode = Bytecode::new_legacy(bytes.clone());

        let json = serde_json::to_string(&bytecode).unwrap();
        let binary = postcard::to_allocvec(&bytecode).unwrap();
        for restored in [
            serde_json::from_str::<Bytecode>(&json).unwrap(),
            postcard::from_bytes::<Bytecode>(&binary).unwrap(),
        ] {
            assert_eq!(restored.kind(), BytecodeKind::Legacy);
            assert_eq!(restored.original_byte_slice(), &bytes[..]);
        }
    }

    #[test]
    fn serde_roundtrip_7702() {
        let delegated_address = Address::with_last_byte(0x42);
        let bytecode = Bytecode::new_eip7702(delegated_address);

        let json = serde_json::to_string(&bytecode).unwrap();
        let deserialized: Bytecode = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.kind(), BytecodeKind::Eip7702);
        assert_eq!(deserialized.eip7702_address(), Some(delegated_address));
    }

    #[test]
    fn serde_binary_roundtrip() {
        for bytes in [&hex!()[..], &hex!("0x1234")[..]] {
            let bytecode = Bytecode::new_raw_checked(bytes.to_vec().into()).unwrap();
            let encoded = postcard::to_allocvec(&bytecode).unwrap();
            let deserialized: Bytecode = postcard::from_bytes(&encoded).unwrap();

            assert_eq!(deserialized.kind(), BytecodeKind::Legacy);
            assert_eq!(deserialized.eip7702_address(), None);
            assert_eq!(deserialized.original_byte_slice(), bytes);
        }
    }

    #[test]
    fn serde_revm_json_cases() {
        let cases = [
            (
                r#"{"LegacyAnalyzed":{"bytecode":"0x1234","original_len":2,"jump_table":{"order":"bitvec::order::Lsb0","head":{"width":8,"index":0},"bits":0,"data":[]}}}"#,
                &hex!("1234")[..],
                BytecodeKind::Legacy,
                None,
            ),
            (
                r#"{"Eip7702":{"delegated_address":"0x0000000000000000000000000000000000000042"}}"#,
                &hex!("ef01000000000000000000000000000000000000000042")[..],
                BytecodeKind::Eip7702,
                Some(Address::with_last_byte(0x42)),
            ),
        ];

        for (json, bytes, kind, delegated_address) in cases {
            let bytecode: Bytecode = serde_json::from_str(json).unwrap();

            assert_eq!(bytecode.kind(), kind);
            assert_eq!(bytecode.eip7702_address(), delegated_address);
            assert_eq!(bytecode.original_byte_slice(), bytes);

            let bytecode_json = serde_json::to_string(&bytecode).unwrap();
            let bytecode_roundtrip: Bytecode = serde_json::from_str(&bytecode_json).unwrap();
            assert_eq!(bytecode_roundtrip.kind(), kind);
            assert_eq!(bytecode_roundtrip.eip7702_address(), delegated_address);
            assert_eq!(bytecode_roundtrip.original_byte_slice(), bytes);
        }
    }

    #[test]
    fn messagepack_revm_cases() {
        let legacy = bytes!("5b6000");
        let delegated_address = Address::with_last_byte(0x42);
        let cases = [
            (revm_bytecode::Bytecode::new_legacy(legacy.clone()), Bytecode::new_legacy(legacy)),
            (
                revm_bytecode::Bytecode::new_eip7702(delegated_address),
                Bytecode::new_eip7702(delegated_address),
            ),
        ];

        for (revm, expected) in cases {
            let encoded = rmp_serde::to_vec(&revm).unwrap();
            let decoded: Bytecode = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(decoded.kind(), expected.kind());
            assert_eq!(decoded.original_byte_slice(), expected.original_byte_slice());

            let encoded = rmp_serde::to_vec_named(&revm).unwrap();
            let decoded: Bytecode = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(decoded.original_byte_slice(), expected.original_byte_slice());

            let encoded = rmp_serde::to_vec(&expected).unwrap();
            let decoded: Bytecode = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(decoded.original_byte_slice(), expected.original_byte_slice());
        }
    }

    #[test]
    fn serde_preserves_original_bytes_without_analysis() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[0x5b, 0x60]));
        let json = serde_json::to_string(&bytecode).unwrap();
        let binary = postcard::to_allocvec(&bytecode).unwrap();
        assert_eq!(json, r#""0x5b60""#);
        assert!(bytecode.inner().jump_table.get().is_none());

        for restored in [
            serde_json::from_str::<Bytecode>(&json).unwrap(),
            postcard::from_bytes::<Bytecode>(&binary).unwrap(),
            serde_json::from_str::<Bytecode>(
                r#"{"LegacyAnalyzed":{"bytecode":"0x5b60ff","original_len":2,"jump_table":{}}}"#,
            )
            .unwrap(),
        ] {
            assert_eq!(restored, bytecode);
            assert_eq!(restored.bytes_slice(), bytecode.bytes_slice());
            assert!(restored.inner().jump_table.get().is_none());
            assert!(restored.legacy_jump_table().unwrap().is_valid(0));
            assert!(!restored.legacy_jump_table().unwrap().is_valid(1));
        }
    }
}
