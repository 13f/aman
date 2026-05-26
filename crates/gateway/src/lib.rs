// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

pub mod runtime;

#[allow(dead_code)]
mod __aman_proof {
    include!(concat!(env!("OUT_DIR"), "/proof.rs"));
}
