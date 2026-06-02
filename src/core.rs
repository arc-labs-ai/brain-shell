//! Shell-local domain identifiers.
//!
//! The wire protocol ships memory ids as a packed `u128` and agent ids as
//! raw 16-byte UUIDs. The shell needs richer, typed handles for those
//! nouns: a `MemoryId` that knows its shard / slot / version triple so the
//! parser can round-trip the `s{shard}/m{slot}/v{version}` form users paste,
//! and an `AgentId` newtype so identity resolution doesn't pass bare arrays.
//! These are the shell's own domain types, mapped onto the SDK's wire shapes
//! at the call boundary.

use uuid::Uuid;

/// Slot occupies 48 bits; the parser masks user input to this width so a
/// pasted short-form id can never overflow the packed layout.
const MAX_SLOT_INDEX: u64 = (1u64 << 48) - 1;

/// A packed memory identifier.
///
/// Bit layout (high → low): shard `u16` (112..128), slot `u48` (64..112),
/// version `u32` (32..64), reserved (0..32). The packed `u128` is what
/// crosses the wire as `WireMemoryId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemoryId(u128);

impl MemoryId {
    /// Wrap a raw packed `u128` (e.g. an `EncodeResponse.memory_id`).
    #[must_use]
    pub fn from_raw(raw: u128) -> Self {
        Self(raw)
    }

    /// The packed `u128` form for the wire.
    #[must_use]
    pub fn raw(self) -> u128 {
        self.0
    }

    /// Pack a `(shard, slot, version)` triple. `slot` is masked to 48 bits.
    #[must_use]
    pub fn pack(shard: u16, slot: u64, version: u32) -> Self {
        let slot_48 = slot & MAX_SLOT_INDEX;
        Self(((shard as u128) << 112) | ((slot_48 as u128) << 64) | ((version as u128) << 32))
    }

    /// Shard component.
    #[must_use]
    pub fn shard(self) -> u16 {
        (self.0 >> 112) as u16
    }

    /// Slot component.
    #[must_use]
    pub fn slot(self) -> u64 {
        ((self.0 >> 64) as u64) & MAX_SLOT_INDEX
    }

    /// Version component.
    #[must_use]
    pub fn version(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

/// An agent identity — a 16-byte UUIDv7 the connection authenticates as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentId(pub Uuid);

impl AgentId {
    /// Mint a fresh time-ordered identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// The raw 16-byte form the SDK's `ClientConfig.agent_id` expects.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 16] {
        *self.0.as_bytes()
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

/// An idempotency-key for a write request — a 16-byte UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestId(pub Uuid);

impl RequestId {
    /// The raw 16-byte form the wire request structs expect.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 16] {
        *self.0.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_round_trips() {
        let id = MemoryId::pack(7, 42, 3);
        assert_eq!(id.shard(), 7);
        assert_eq!(id.slot(), 42);
        assert_eq!(id.version(), 3);
    }

    #[test]
    fn raw_round_trips() {
        let id = MemoryId::from_raw(0xdead_beef);
        assert_eq!(id.raw(), 0xdead_beef);
    }
}
