* SPDX-License-Identifier: MIT
* Copyright (c) 2026 sonde contributors
*
* Si2301CDS P-channel MOSFET — Level 1 model
* Datasheet: Vishay Si2301CDS
* Key parameters:
*   VTO  = -1.2 V  (gate threshold voltage)
*   KP   = 8.63    (tuned for Rds_on ≈ 115 mΩ at Vgs = -4.5 V)
*   RD   = 0.05    (drain series resistance)
*   RS   = 0.05    (source series resistance)
*   CBD  = 50p     (drain-body junction capacitance)
*   CBS  = 50p     (source-body junction capacitance)

.model si2301 pmos (level=1 vto=-1.2 kp=8.63 rd=0.05 rs=0.05
+ cbd=50p cbs=50p lambda=0.01)
