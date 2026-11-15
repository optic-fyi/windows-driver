// Copyright (c) 2026 Optic.fyi. All rights reserved.
// SPDX-License-Identifier: LicenseRef-Optic-Source-Available
//
// Source-available, not open source. You may read this file to inspect how Optic
// uses its kernel privileges. All other rights are reserved: no use, copying,
// modification, redistribution, or use in a competing product without written
// permission from Optic.fyi. See the LICENSE file at the repository root.

//! Hardware-independent logic of the Optic driver.
//!
//! Everything here is `no_std`, free of WDK dependencies and tested on any
//! host (Linux CI included). The `opticdrv` crate is a thin WDF shell around it.

#![cfg_attr(not(test), no_std)]

pub mod heartbeat;
pub mod ioctl;
pub mod wide;
