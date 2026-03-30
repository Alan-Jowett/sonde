* SPDX-License-Identifier: MIT
* Copyright (c) 2026 sonde contributors
*
* SS14 Schottky barrier diode model
* Datasheet: generic SS14 (1A / 40V SMA)
* Key parameters:
*   IS  = 1 µA     (saturation current → Vf ≈ 0.3 V at low current)
*   N   = 1.05     (emission coefficient, slightly above ideal)
*   BV  = 30 V     (reverse breakdown, conservative)
*   RS  = 0.05     (series resistance)
*   CJO = 50p      (zero-bias junction capacitance)

.model schottky d (is=1u n=1.05 bv=30 rs=0.05 cjo=50p)
