//! Compact identifier formatting.
//!
//! Memory ids are 128-bit triples; entity / statement ids are 16-byte
//! UUIDs. The full forms dominate a terminal line; the table renderers
//! want a short canonical form humans can copy out of `psql`-style
//! output and feed back as a query. These helpers centralize that
//! convention so the shell, the CLI, and (later) the TUI all agree.

use brain_db_sdk::wire::types::WireMemoryId;

/// Packed-`MemoryId` bit layout (high → low):
///   shard    : bits 112..128 (`u16`)
///   slot     : bits  64..112 (`u48`)
///   version  : bits  32..64  (`u32`)
///   reserved : bits   0..32  (zero)
const MAX_SLOT_INDEX: u64 = (1u64 << 48) - 1;

/// Shard component of a packed [`WireMemoryId`].
#[must_use]
fn memory_id_shard(raw: WireMemoryId) -> u16 {
    (raw >> 112) as u16
}

/// Slot component of a packed [`WireMemoryId`].
#[must_use]
fn memory_id_slot(raw: WireMemoryId) -> u64 {
    ((raw >> 64) as u64) & MAX_SLOT_INDEX
}

/// Version component of a packed [`WireMemoryId`].
#[must_use]
fn memory_id_version(raw: WireMemoryId) -> u32 {
    (raw >> 32) as u32
}

/// Pack a `(shard, slot, version)` triple into a [`WireMemoryId`].
///
/// `slot` is masked to 48 bits. Mirrors the server's `MemoryId::pack`.
/// Test-only: production renderers receive already-packed ids over the wire.
#[cfg(test)]
#[must_use]
fn memory_id_pack(shard: u16, slot: u64, version: u32) -> WireMemoryId {
    let slot_48 = slot & MAX_SLOT_INDEX;
    ((shard as u128) << 112) | ((slot_48 as u128) << 64) | ((version as u128) << 32)
}

/// Compact `s{shard}/m{slot}/v{version}` form of a packed [`WireMemoryId`].
///
/// Inverse of `brain-shell`'s `parse_short_form` — the canonical short
/// form a user types back to refer to a recalled memory.
///
/// ```
/// use brain_shell::render::util::short_id::memory_id_short_form;
///
/// // s7/m42/v3 packed into a WireMemoryId (u128).
/// let raw: u128 = (7u128 << 112) | (42u128 << 64) | (3u128 << 32);
/// assert_eq!(memory_id_short_form(raw), "s7/m42/v3");
/// ```
#[must_use]
pub fn memory_id_short_form(raw: WireMemoryId) -> String {
    format!(
        "s{}/m{}/v{}",
        memory_id_shard(raw),
        memory_id_slot(raw),
        memory_id_version(raw)
    )
}

/// First 4 bytes (8 hex chars) + `…` form of a 16-byte UUID-shaped id.
///
/// Used for entity ids, statement ids, space ids, request ids, and any
/// other UUID-shaped identifier whose full form is too long for a table
/// cell. Eight hex chars give 32 bits of disambiguation — enough that
/// collisions in a recalled batch are effectively impossible.
#[must_use]
pub fn uuid_short_form(bytes: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}…",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

/// Short form of a 16-byte entity id. Today identical to
/// [`uuid_short_form`]; kept as its own function so the EntityId
/// representation can evolve (e.g. a typed wrapper) without rippling.
#[must_use]
pub fn entity_id_short_form(bytes: &[u8; 16]) -> String {
    uuid_short_form(bytes)
}

/// Short form of a 16-byte statement id. See [`entity_id_short_form`].
#[must_use]
pub fn statement_id_short_form(bytes: &[u8; 16]) -> String {
    uuid_short_form(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_id_short_form_roundtrips() {
        // The short form is the canonical user-facing identifier; it
        // must round-trip cleanly through the pack/unpack accessors so
        // the shell's parser can reconstruct the id from the rendered
        // string.
        let id = memory_id_pack(7, 42, 3);
        assert_eq!(memory_id_short_form(id), "s7/m42/v3");

        let id = memory_id_pack(0, 0, 0);
        assert_eq!(memory_id_short_form(id), "s0/m0/v0");

        // u16::MAX shard, large slot, version near u32::MAX.
        let id = memory_id_pack(u16::MAX, 0x1234_5678, 99_999);
        assert_eq!(memory_id_short_form(id), "s65535/m305419896/v99999");
    }

    #[test]
    fn uuid_short_form_uses_first_four_bytes() {
        let bytes = [
            0xab, 0xcd, 0xef, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x11, 0x22, 0x33,
            0x44, 0x55,
        ];
        assert_eq!(uuid_short_form(&bytes), "abcdef12…");
    }

    #[test]
    fn uuid_short_form_zero_bytes() {
        let bytes = [0u8; 16];
        assert_eq!(uuid_short_form(&bytes), "00000000…");
    }
}
