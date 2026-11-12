// SPDX-License-Identifier: AGPL-3.0-or-later
//! OS observations shared by authorization and evidence. No device I/O.
pub mod clock;
pub mod files;
pub mod private;
pub mod process;
#[cfg(windows)]
pub mod windows;
