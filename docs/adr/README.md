# Architecture decision records

Each file records one decision: the context that forced it, the decision
itself, and the consequences that follow. Records are immutable once accepted —
a decision that changes gets a new record that supersedes the old one, so the
history of why the code looks like this survives the code changing.

Format: `NNNN-kebab-case-title.md`, numbered in acceptance order.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-capability-based-platform-gating.md) | Gate platform differences on capabilities, not OS names | Accepted |
| [0002](0002-android-support-and-test-tiers.md) | Android is a supported platform, proven by tiered CI | Accepted |
