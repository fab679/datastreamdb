//! Posting-list codecs: varint packing, group varint, compression.
//!
//! Rust port of the upstream Go `codec` package. [`Encoder`] packs sorted UIDs into
//! delta-encoded [`UidPack`] blocks; [`Decoder`] seeks and unpacks them.
//! Group-varint bytes are wire-compatible with `dgryski/go-groupvarint`.

#![warn(missing_docs)]

pub mod groupvarint;
pub mod pack;
pub mod varint;

pub use datastreamdb_proto::{UidBlock, UidPack};
pub use pack::{
    Decoder, Encoder, SeekPos, approx_len, copy_uid_pack, decode, decode_to_uvarints, encode,
    encode_from_buffer, exact_len,
};
