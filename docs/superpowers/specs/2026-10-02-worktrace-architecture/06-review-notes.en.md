# Worktrace — Revision Summary and Review Choices

Status: R-01–R-08 approved; this revision awaits review; implementation and platform probes remain open. Date: 2026-10-03. [Chinese](06-review-notes.zh.md).

## 1. Changes

| Document | Revision |
| --- | --- |
| 00 Architecture | Core/WebView distinction; pure event types/runtime bus split; atomic business writes; snapshot races/stale replies/lost-final-event repair; correct panic contract |
| 01 Modules | Minimal M06/M07 in V0.1; minimal M09 in V0.3; M08 scoring V0.5; clear dependencies/vertical delivery |
| 02 Data | work_interval, immediate recovery, clipping, audited edits, partial uniqueness/root tags, dual measures, transitions, origin versus confirmation |
| 03 ADRs | Preserve IDs, remove categorical claims, add 011–013 and keep product choices pending |
| 04 Acceptance | Preserve IDs, add correction/export-review/backup/synchronization F-017/F-018/F-019/F-020; fix sleep/recovery/concurrency cases |
| 05 Roadmap | Core loop first, V0.1b, staged search/security before AI, two-week personal-use gate |
| 99 Glossary | Effective intervals versus activity segments, origin/confirmation, revision/ticks, facts/experimental scores |

Both languages updated. PROJECT_SPEC retains original vision with a navigation notice. No application code, dependencies or copied components changed.

## 2. Correctness fixes

Freeze paused budgets; recover immediate restarts; use partial uniqueness for running foreground and separate root-tag uniqueness; validate intervals/state with DB and services; clip report ranges; never substitute broadcasting for transactions; separate origin from confirmation; release abort cannot be converted into normal errors.

work_interval is one proposed implementation. A pause-interval model is also possible if it satisfies the same acceptance cases; field design is not irreversibly locked.

## 3. Approved product choices (R-01…R-08)

| ID | Choice | Approved choice | Alternative/impact |
| --- | --- | --- | --- |
| R-01 | First release scope | Core V0.1, HUD/hotkeys V0.1b | Merge if HUD is essential day one; core still implemented first |
| R-02 | Foreground sleep/lock | Auto-pause, explicit resume | Wall time includes absence; asking on every wake adds interaction |
| R-03 | Historical classification | Recompute using current classification, label reports, freeze exports | Session snapshots retain original categories with greater editing/migration cost; useful for formal audit |
| R-04 | Layout | Fixed first, preserve DockviewDemo | Add dockview/persistence after real multi-panel demand |
| R-05 | AI boundary (updated this revision) | Off by default; select/preview/confirm inputs and destination per action | Remove file classification; external Agent permissions stay with that tool |
| R-06 | Capability profiles and gap actions | Retain self-report/evidence/optional scores connected to practice and review | Validate samples/calibration; effort alone never measures ability |
| R-07 | Mini window | Schedule after real-use evidence | Prioritize Mini before click-through HUD if interactive controls matter more |
| R-08 | Weights | Per-kind total ≤1, Unallocated remainder, no auto-normalization | Normalization must visibly explain changed accounting |

Original R-01–R-08 approved 2026-10-02. User-confirmed scope now updates R-05 to input confirmation and retains capability profiles/optional scores under R-06.

## 4. Implementation probes

| Probe | Required evidence |
| --- | --- |
| DB boundary | Responsive UI under command/timer/backup contention, rollback, selected threading scheme |
| Clock mapping | Normal/sleep/lock/clock-change/restart cases with trusted intervals and correction flow |
| Two windows | Subscription/snapshot race, lost final event, delayed stale reply all converge |
| HUD/build | Windows DPI/displays/click-through/no-focus, multi-entry dev/package paths and limitations |
| Backup/restore | Consistent WAL snapshots, failed migration/restore preserves readable original |
| Chinese search | Real short Chinese/model IDs/paths before choosing FTS5 |

This revision checks document structure, links, acceptance IDs and representative constraints only. Product/platform experiments are not run and features are not accepted.

## 5. Two-week personal-use review

Observe capture/start effort, missed-entry repair, forgotten-stop correction, recovery friction and practical weekly-summary value. Record steps/examples before inventing percentage targets. Then choose whether splits/HUD/scoring/AI complexity earn their cost.

Read [roadmap](05-roadmap.en.md), R-01…R-08, [data](02-data-model.en.md), [acceptance](04-functional-spec.en.md), then [architecture](00-architecture.en.md).

## 6. New revisions awaiting review

- Fresh data_epoch on restore prevents revision rollback and stale responses; session_version filters late ticks and the coordinator serializes DB commit and timer baseline application.
- Trusted paused sessions remain paused. Only uncertain intervals need review; confirmed earlier effort remains counted. Discarding one interval differs from voiding an entire session. This behavioral change needs review.
- ContextFact replacement specifies an atomic three-step sequence compatible with the partial unique index.
- STRICT_LOCAL cannot be overridden by destination grants. Grants bind project/provider/normalized endpoint and are rechecked at send time; revocation blocks unsent requests only.
- Diagrams distinguish views from OS processes, implementation order from dependencies, and common state paths from the normative table. Tray HUD is V0.1b.

This revision changes documents and existing diagrams only. Example constraint checks do not constitute runtime acceptance.

Checks passed: links/fences and bilingual IDs/SQL across 17 Markdown files; strict XML/accessibility structure and HTML-source export for 13 SVGs; six representative SQLite constraint cases and ContextFact replacement/injected rollback/unique-current/FK checks. Application acceptance and per-diagram visual review have not been performed.

Current scope (2026-10-03): personal work records and task management, including capability gaps and KPA evidence. External Agents and companion skills handle file reading, OCR, extraction and full-text search. See [scope and import contract](07-scope-and-agent-import.en.md).


## 7. 2026-10-03 scope revision

Update PROJECT_SPEC and bilingual 00–05/99 scope/modules/entities/acceptance; add 07 import contract. Keep AI task assistance, capability gaps/actions and KPA evidence. External Agents/skills process source files. Archive the former classification diagram; prior extraction/classification history is superseded by this section. No business code or companion skill implemented.
