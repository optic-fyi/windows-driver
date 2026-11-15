// Copyright (c) 2026 Wilson Brous, Ayden Yang, Carter Alfers and Andre Hamghalam,
// trading as Optic.fyi. All rights reserved.
// SPDX-License-Identifier: LicenseRef-Optic-Source-Available
//
// Source-available, not open source. You may read this file to inspect how Optic
// uses its kernel privileges. All other rights are reserved: no use, copying,
// modification, redistribution, or use in a competing product without written
// permission from the copyright holders. See the LICENSE file at the repository root.

//! Emits the linker flags that turn the cdylib into a Windows kernel driver.

fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::configure_wdk_binary_build()
}
