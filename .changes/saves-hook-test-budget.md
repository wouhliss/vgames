---
audience: internal
component: launcher
type: fixed
---
PLAY-06: the save hook's pull budget is a field (5 s in the product) so hook tests that expect a finished pull no longer fail when a loaded CI runner needs longer; the budget test uses a 1 s budget.
