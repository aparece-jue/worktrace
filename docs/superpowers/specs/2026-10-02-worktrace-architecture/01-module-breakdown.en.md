# Worktrace — Module Breakdown

Status: revised draft for review; date: 2026-10-02.
[Architecture](00-architecture.en.md) · [Other language](01-module-breakdown.zh.md)

## 1. Responsibilities and milestones

| ID | Module | Owns | Dependencies | Milestone |
| --- | --- | --- | --- | --- |
| M00 | Platform | Single instance, lock/suspend, hotkeys, native adapters; no business rules | No domain dependencies | V0.1; HUD V0.1b |
| M01 | Storage | Migrations, transactions, constraints, range queries, backup/restore, revision persistence | M02 pure types | V0.1 |
| M02 | Domain | Entities, transitions, range/overlap/tree validation; no IO | M03 pure event types | V0.1, expanded by milestone |
| M03 | Events | Pure types and separate broadcaster, revision protocol; no required business follow-up | Types independent; broadcaster Tauri | V0.1 |
| M04 | Timer | Monotonic clock, countdown budget, ticks, lock/suspend reconciliation | M02, M03, M00 | V0.1; Pomodoro V0.2 |
| M05 | Sessions | Intervals, pause/resume/finish, recovery confirmation, manual correction | M01, M02, M04 | V0.1; concurrency/interruption V0.2 |
| M06 | Statistics | Range clipping, human/machine separation, weights, reconcilable DTOs | M01, M02 | Minimal V0.1; extended V0.2 |
| M07 | Reports | Export/simple weekly review; extended ranges and KPA | M06, M01 | Minimal V0.1; reports V0.2; KPA V0.5 |
| M08 | Knowledge/estimates | Error and knowledge usage; experimental capability model | M01, M02, M06 | Error V0.2; scores V0.5 |
| M09 | Context | Outbound whitelist/classification first; facts/documents/decisions later | M01, M02 | Minimal V0.3; full V0.4 |
| M10 | AI | Provider calls, suggestion history/input versions/adoption/feedback; optional | M09, M01, M02 | V0.3 |
| M11 | Windows | Window/tray lifecycle; native callbacks call services | M00, M03, service interfaces | Tray V0.1; HUD V0.1b; Mini open |
| M12 | Frontend | Shell/pages/mirror/generated types/recovery and correction UI | Service IPC | V0.1 onward |
| M13 | Search | Only existing entities; test Chinese and engineering identifiers | M01, M02 | Basic V0.2; context V0.4 |

## 2. Boundaries and critical path

Module IDs identify responsibilities, not 14 crates or a requirement for a large separate specification before every implementation. Split pure event types from runtime broadcasting; domain references types only. M01 persists revision; M03 owns protocol/distribution, avoiding storage/bus dependency cycles.

Core implementation chain: M02 → M01 → M04/M05 → minimal M06 → minimal M07 export/review. M12 shell may start with mocks and connect incrementally. M00 single-instance ownership precedes persistence initialization; M11 connects tray to services. HUD probes do not block this chain.

V0.3 implements minimal M09 outbound policy before M10; V0.4 expands document extraction. M13 is independent of M06 and never requires future tables.

## 3. Implementation deliverables

Each vertical feature delivers command contracts, persistence, UI, meaningful tests and a demonstrable acceptance case. Avoid empty directories created only to look layered. Test transitions, rollback, recovery, concurrency and accounting boundaries rather than mechanically mirroring every function. Scope/dependencies follow 04/05; pending product choices follow 06.
