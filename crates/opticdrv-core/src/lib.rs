//! Hardware-independent logic of the Optic driver.
//!
//! Everything here is `no_std`, free of WDK dependencies and tested on any
//! host (Linux CI included). The `opticdrv` crate is a thin WDF shell around it.

#![cfg_attr(not(test), no_std)]

pub mod heartbeat;
pub mod ioctl;
pub mod wide;
