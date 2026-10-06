---
audience: internal
component: launcher
type: added
---
INS-01: `catalog::release::select_release` picks the build a computer installs per 09-compatibility §1 (native first; Windows on Arm through emulation; Linux through Proton; Apple silicon through Rosetta 2, then Wine), reusing the one `host_platform` of the launch path; supersedes PR #85.
