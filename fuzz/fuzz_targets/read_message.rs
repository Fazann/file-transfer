//! Feeds arbitrary bytes to `protocol::read_message`, the first thing that touches data from the network.
//!
//! It must never panic, hang or allocate without bound: every input ends in a message or an error.

#![no_main]

use lanshare_core::protocol::read_message;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // `read_message` is async; a single-threaded runtime drives it to the end without any real I/O.
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else { return };
    runtime.block_on(async {
        let mut reader = data;
        // Read frames until the input runs out or one fails, as a receiver would on one stream.
        while read_message(&mut reader).await.is_ok() {}
    });
});
