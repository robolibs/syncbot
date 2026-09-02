//! Fuzz the canonical wire decode path.
//!
//! Every adapter — including hand-written ones in other languages — hands the
//! core bytes it claims are a canonical datapod. Decoding must reject a
//! malformed one, never panic on it.
//!
//! ```sh
//! cargo +nightly fuzz run canonical_datapod
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = syncbot::wire::fuzz_decode_canonical(data);
});
