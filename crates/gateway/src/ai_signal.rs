// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

/// Build-time signal marker — referenced via TypeId so the compiler
/// links it into the binary even though no code path uses it directly.
pub struct AmanSignalV1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_type_present() {
        let id = std::any::TypeId::of::<AmanSignalV1>();
        assert_ne!(id, std::any::TypeId::of::<()>());
    }
}
