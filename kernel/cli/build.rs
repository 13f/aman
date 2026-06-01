// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Build script for the CLI crate.
//!
//! Compiles the `aman.proto` protobuf definitions to generate gRPC client stubs.

fn main() {
    tonic_build::compile_protos("../../proto/aman.proto")
        .expect("failed to compile aman.proto for CLI client stubs");
}
