//! Network transports. Phase 1 has plain TCP only; the `Transport`/`Connection` traits arrive with QUIC in phase 4.

mod tcp;

pub use tcp::{connect, listen, CONNECT_TIMEOUT, DEFAULT_PORT};
