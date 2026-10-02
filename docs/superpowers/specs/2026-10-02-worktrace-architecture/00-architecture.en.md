# Worktrace — Overall Architecture

Status: revised draft for user review. Date: 2026-10-02. [Chinese](00-architecture.zh.md).
See [product vision](../../../PROJECT_SPEC.md) and [review notes](06-review-notes.en.md). This is target design, not implemented behaviour; the repository still has the initial frontend and greet command.

## 1. Scope and document authority

A local task, effort-recording and review tool for individual engineering work. Keep Local First, AI Optional and User > Rule > AI. No generic agent, plugin marketplace, Office/CAD replacement or workflow engine.

PROJECT_SPEC preserves vision/original milestones; 04 owns acceptance scope, 05 dependencies/order, 02 accounting/data, 03 technical rationale and 99 terminology. New product trade-offs are pending in 06, not user-approved merely because an ADR exists. Chinese is the working master, English updated together. Resolve conflicts rather than giving every document overriding authority.

## 2. Stack and topology

Tauri 2, React 19, TypeScript 6, Vite 8, pnpm. Ant Design is the proposed sole component library; dependency cleanup follows design review. Preserve copied components. DockviewDemo is not automatically product UI; production dockview use remains open.

There is one authoritative Rust application core; WebView/system rendering can use additional OS processes. Rust owns business authority, SQLite persistence, React rebuildable query caches/UI state. Windows may briefly disagree; synchronization converges rather than guaranteeing identical displays at every instant.

Each main/HUD/Mini window has its own JS context. Window closure does not stop the core. Tray actions call the same services. Acquire single-instance ownership before database/timer initialization; a second launch may create a temporary process which forwards activation and exits, never another core.

Separate lightweight HUD/Mini entries are proposed; one entry with lazy loading is a valid fallback. Validate multi-entry configuration against installed Vite 8 rather than assuming old option names. Grant least-needed window capabilities; HUD is read-only by default. Rust controls files, outbound requests and mutations.

## 3. Boundaries and transactions

| Directory | Responsibility | Dependencies |
| --- | --- | --- |
| domain/ | Pure types, transitions, validation | Pure event types, no runtime bus or IO |
| storage/ | SQLite, migrations, transactions, backups, queries | domain, pure event types |
| platform/ | OS windows/tray/lock/hotkeys/single instance | Tauri/OS adapters, no business decisions |
| services/ | Intent orchestration, timer/statistics/context/AI | domain, storage, platform, events |
| commands/ | Inputs, errors/DTOs, service calls | services, domain types |
| events/ | Pure types plus separate runtime dispatch adapter | Pure types dependency-free; dispatch may use Tauri |

lib.rs assembles the application. Commands never directly issue SQL; domain has no IO; storage does not call platform. M11/composition root connects native callbacks to services.

Complete consistency-critical operations directly in one service transaction: finish sessions, update task result and increment revision. Never depend on asynchronous event subscribers to finish required writes. Statistics aggregate on demand.

Run synchronous DB work inside a controlled blocking boundary; never hold connection locks across await. Choose a serialized DB worker or controlled spawn_blocking after an M01 probe. Validate invariants in write transactions with database constraints as backup. Pause writes for backup/restore; specify busy_timeout, WAL, FKs, pre-migration backup and disk-full handling. Use a consistent SQLite backup API or verified VACUUM INTO, not a raw copy of a live WAL main file.

## 4. IPC and errors

Commands follow intent and return aggregate views without N+1. Generate TS DTO types from Rust after validating tooling. Mutations carry expected_row_version; conflicts return VERSION_CONFLICT. AI requests retain input versions and do not apply to changed tasks.

Expected failures use Result<T,AppError> with code/message/redacted detail. Panic is a defect; current release panic=abort terminates the process and cannot be promised convertible to AppError. Diagnostics/recovery remain necessary; avoid unwrap on normal user/IO failures.

Post-commit broadcast failure is diagnostic, not transaction failure. Prevent duplicate submissions. Automatic retries of non-idempotent commands require request_id plus a replay result stored in the same transaction; otherwise do not retry blindly.

## 5. Revision and synchronization

Persist app_meta.revision once per successful business write transaction. Coalesce multiple changes into one domain.changed invalidation carrying affected types/IDs. No-op operations, heartbeats and timer.tick do not increment it. Read snapshot data and revision in one read transaction.

Envelope: event/revision/at (Unix milliseconds)/payload. Broadcast committed transactions in revision order; clients still handle delay, duplication and reordering.

1. Subscribe and buffer before fetching a consistent snapshot.
2. Apply snapshot; discard notifications at or below its revision, then invalidate affected queries.
3. Query replies carry revision. Discard replies older than a view's applied/required revision; coalesce requests.
4. Check get_revision on focus/reopen/resume/reconnection and at most every 30 seconds while visible. Hidden windows validate before display. This catches a lost final event without polling the full database.
5. Refetch a snapshot on gaps/reordering when consistency cannot be established. Temporary display latency is allowed.

timer.tick uses separate run_id/tick_seq/as_of/session_id/active_ms/remaining_ms. Discard old runs/sequences; it never persists a business revision. Reopened windows query timing immediately rather than waiting for another tick.

## 6. Frontend and outbound data

One domainState subscription entry per JS context with cleanup and hooks for pages. Business events invalidate queries; ticks replace display values. Begin with a simple store such as useSyncExternalStore, but permit a cache/state library when justified; library avoidance is not what establishes Rust authority.

AI receives only an M09-whitelisted Context Bundle. V0.3 includes minimal task/project security; full document extraction is V0.4. Default STRICT_LOCAL. Explicit provider-specific authorization is required for outbound items. STRICT_LOCAL/CONFIDENTIAL are cloud-blocked by default, INTERNAL needs explicit authorization, PUBLIC may transmit after cloud AI is enabled. Derived content inherits the strictest source level. Logs record IDs/classification/counts, not sensitive text. Credentials use OS credential storage, never SQLite/localStorage. Adopting a suggestion is not outbound authorization.

## 7. Open verification and index

Probe Windows HUD click-through/no-focus/DPI, multi-entry dev/package paths, database threads and shutdown/backup races, and timer behaviour on offline/lock/suspend/clock changes. No empirical outcome is claimed yet. HUD probes do not block the recording loop.

[Modules](01-module-breakdown.en.md) · [Data](02-data-model.en.md) · [ADRs](03-adr.en.md) · [Acceptance](04-functional-spec.en.md) · [Roadmap](05-roadmap.en.md) · [Glossary](99-glossary.en.md)
