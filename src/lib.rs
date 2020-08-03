//#[warn(missing_docs)]
pub mod client;
pub mod harvester;
pub mod indexer;
pub mod metadata;
pub mod traits;

pub use client::InProcess;

use sp_core::storage::StorageKey;

pub fn storage_key_at(module: &[u8], storage: &[u8]) -> StorageKey {
    StorageKey(
        sp_core::hashing::twox_128(module)
            .iter()
            .chain(sp_core::hashing::twox_128(storage).iter())
            .cloned()
            .collect(),
    )
}

//#HACK this is a quick and dirty way to get to a specific storage item because we are assuming.  The Caller must first prehash the key
//using whatever the module uses as the hasher.
pub fn storage_key_with_hashed(module: &[u8], storage: &[u8], key_hashed: &[u8]) -> StorageKey {
    StorageKey(
        sp_core::hashing::twox_128(module)
            .iter()
            .chain(sp_core::hashing::twox_128(storage).iter())
            .chain(key_hashed.iter())
            .cloned()
            .collect(),
    )
}

pub fn blake2_128_concat(x: &[u8]) -> Vec<u8> {
    sp_core::hashing::blake2_128(x)
        .iter()
        .chain(x.iter())
        .cloned()
        .collect::<Vec<_>>()
}
