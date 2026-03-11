// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() {
    #[cfg(feature = "esp")]
    embuild::espidf::sysenv::output();
}
