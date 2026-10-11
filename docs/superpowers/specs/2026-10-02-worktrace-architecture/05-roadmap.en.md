# Worktrace — Roadmap and Milestones

Status: revised proposal for user review. Date: 2026-10-03. [Chinese](05-roadmap.zh.md).
Upstream: [acceptance](04-functional-spec.en.md), [modules](01-module-breakdown.en.md). [Pending choices](06-review-notes.en.md).

## 0. Review and incremental delivery

This revision changes documentation only, not SQLite/timer/HUD implementation. Review it, then probe DB blocking boundaries and clock policy. HUD/multi-entry probes are independent of the recording loop. Creating all empty directories or pushing documents is not a prerequisite for feature work; publishing/commits follow the user's workflow.

Apply dependency cleanup and decide production dockview after review; preserve copied components. Verify Rust→TS generator maintenance/Tauri 2 compatibility before selecting it.

## 1. Acceptance mapping

| Milestone | Goal | Required criteria |
| --- | --- | --- |
| V0.1 | Capture→time→correct/recover→Today→minimal export/review→backup | F-001…F-011, F-014…F-020 |
| V0.1b | Desktop enhancement: HUD/modes/global capture hotkey | F-012/F-013 plus F-001 hotkey extension |
| V0.2 | Concurrency/interruption/weights/scheduling/reports and **interactive daily/weekly/yearly review charts**/existing-entity search | F-101…F-113 |
| V0.3 | AI/history/feedback/selected-input confirmation | F-201…F-210 |
| V0.4 | Brief facts/decisions, minimal outcomes, references, Agent imports and record search | F-301…F-305 plus F-110 extension |
| V0.5 | Extended review/KPA/knowledge facts/experimental capability model | F-401…F-405 |
| V0.6 | **Distribution & auto-update**: signed update feed, check/download/install, channels, rollback | New capability, F-ID TBD |
| V1.0 | Automated backup/hardened migration/recovery/long-run verification | F-501…F-510 |

V0.1 Today is a daily selection, not a calendar; Goal/Milestone/WBS are not required. Scheduled begins in V0.2. Precise skill scores are experimental and never mandatory under sparse evidence.

## 2. V0.1 vertical delivery

1. M02/M01 minimal Project/Task/Tag CRUD, migrations/constraints/revision/errors; M00 ownership before DB initialization.
2. M04/M05 start/pause/resume/finish with durable work_interval. Test paused finish/restart, concurrent start/resume, clock changes and overlap.
3. M12/M03 Inbox/Today/timer, subscribe-before-snapshot, stale-reply rejection/light revision checks; M11 minimal tray uses the same services.
4. M05/M06 recovery confirmation, manual edits, range clipping and separate confirmed/live/pending figures.
5. M07/M01 JSON detail, Markdown weekly summary, manual backup/full restore; rehearse restore failure before trusting real data.
6. Pass V0.1 criteria, then use personally for two weeks, recording missed timers/forgotten stops/edit frequency/capture-switch effort/review usefulness.

Each step delivers commands/persistence/UI/tests/demo. Specify only the next slice. Estimate dates after probes and the real loop, not from module counts.

## 3. Later scope and dependencies

- V0.1b: HUD properties/DPI/displays/modes/hotkey conflicts; Mini scope awaits review. A failed HUD probe may defer delivery, never count degraded behaviour as a pass.
- V0.2: expand M06/M07, concurrency/interruption/time_block/Goal/Milestone/dependencies/WBS. M08 covers estimate errors and knowledge facts only. M13 searches existing objects and tests Chinese/model identifiers before choosing LIKE/FTS5.
- V0.3: M09 selected-input/preview/confirmation before M10 history/feedback. Use existing entities; advanced analyze_context/review_week interfaces follow later capabilities, not all eight at once.
- V0.4: brief ContextFact/Decision, references and reviewed Agent import under 07; M13 searches records/reference titles, never source-file bodies.
- V0.5: usage facts/sample counts before optional experimental ability scores; KPA/review use confirmed facts, acceptance rate does not establish saved time.
- V0.2 (**review & interactive charts**, raised 2026-10-12; design guidelines to be re-planned before freezing):
  charts for daily/weekly/yearly reviews (histogram + pie/donut), interactive (selecting a slice shows its content),
  with selectable dimensions (a day can list concrete work or group by tag/category). Five decisions come first:
  (1) aggregate in the backend — not for speed but for a single source of truth on day-boundary clipping, timezone
  and the human/machine/waiting split; (2) dimension model: task / project / tag / session type / time-of-day /
  weekday / month — switch dimensions, not charts; (3) interaction depth: drill-down on selection plus cross-chart
  linking, which needs "aggregate by dimension" and "fetch details by filter" endpoints; (4) chart-library choice
  (only antd today, no chart library) — any new dependency must pass `check-bundle.ps1` and be size-checked
  (installer is currently 2.9 MB); (5) invariants: never total human + machine, clip across midnight using the
  backend `range.to`, always use the query timezone. Cheaper path: the existing JSON export already carries
  `confirmed / pending / totals` plus per-interval detail, so charts can consume it first.
- V0.6: **auto-update**. Six decisions must be settled before implementation:
  1. **Update feed & hosting**: GitHub Releases / self-hosted static site / internal mirror.
  2. **Signing-key ownership**: the public key is embedded and the private key stays offline — losing it means
     no already-installed build can ever auto-update again, so backup and rotation must be written down first.
  3. **Check timing & user visibility**: silent startup check plus a user-confirmed install (recommended) versus
     silent download and restart-to-install; a manual "check for updates" entry point is required, and being
     offline must never block normal use.
  4. **Channels**: whether stable/beta fork, and how beta rolls back to stable.
  5. **Failure & rollback**: interrupted downloads, signature mismatch and failed installs must leave the old
     build runnable; after data has been written by a newer version the older one must refuse to write
     (existing "future-version write refusal" rule), so a pre-upgrade backup should be the default.
  6. **Relationship to the current install shape**: NSIS, `INSTALLMODE=currentUser`, installed under
     `%LOCALAPPDATA%\Worktrace`, with data in `%APPDATA%\com.worktrace.desktop\` (must survive the upgrade);
     a portable form would need separate treatment.
  7. **Packaging form fixed to NSIS, identity triple frozen** (decided 2026-10-12): narrow `bundle.targets`
     from `"all"` to `["nsis"]` (`all` emits both MSI and NSIS — two install shapes of one app is the classic
     "must uninstall first" trap); set `installMode` explicitly to `"currentUser"`; never change the install
     directory or `identifier`, and never put data inside the install directory (data lives in
     `%APPDATA%\com.worktrace.desktop\` — correct). Keep those three and updates never need an uninstall.
  8. **Already-installed builds will not auto-update**: 0.1.0 was built without `createUpdaterArtifacts`,
     so the first updater-enabled release requires one manual reinstall. Expected behaviour, not a defect —
     make it an acceptance criterion.
  9. **Two kinds of signing**: the updater minisign signature (free, protects the update payload) is not the
     same as Windows code signing (removes the SmartScreen prompt, paid certificate); keep separate keys.
  Note (observed): downloads have already failed under a restricted network here (NSIS tool download hit socket
  permission 10013, proxy unreachable), so feed reachability, timeouts and "check failed" messaging must assume
  the feed can be unavailable for long stretches.
- V1.0: automate/harden existing protection and conduct fault/long-run tests; data protection is not first implemented here.

## 4. Risks and gates

Core chain: M02→M01→M04/M05→minimal M06→minimal M07. M12 shell can mock first. Search is independent of statistics; HUD is off the core chain; minimal M09 precedes M10.

| Risk | Verify before delivery |
| --- | --- |
| DB blocking/contention | Timer/query/backup concurrency, busy and disk-full paths |
| Wrong accounting | Pauses across midnight, clock changes, immediate restart, provisional/recovery confirmation |
| Stale windows | Snapshot races, lost final event, late old replies, hidden reopen |
| Data loss | Consistent backup, migration failures, future-schema write refusal, failed restore preserves original |
| Product overhead | Two weeks of real use before adding HUD/splits/complex categorization |

## 5. Slice specification

Each spec lists scope/F-IDs, commands/DTOs, transactions/invariants, errors/retries, tests, manual acceptance and deviations. Track implementation separately as not-started/in-progress/accepted. Meaningful invariant tests replace mechanical one-test-per-function rules.

Implementation checks for this revision: M01 validates epoch switching and ContextFact replacement; M04/M05 validate commit/baseline serialization and stale ticks; M06 retains trusted closed effort within recovering. R-01–R-08 are approved; this protocol/recovery revision awaits review.

Current scope (2026-10-03): personal work records and task management, including capability gaps and KPA evidence. External Agents and companion skills handle file reading, OCR, extraction and full-text search. See [scope and import contract](07-scope-and-agent-import.en.md).


Capability loop: evidence → user-confirmed gap → learning/practice task → work samples/self-assessment → review; optional proficiency retained. KPA collects traceable outcomes/dates, never grades people.

See [08: timing, phases, AI and report snapshots](08-implementation-contracts.en.md).

V0.1 probes 08 clock mapping/checkpoints; V0.2 break never counts effort; V0.3 adds F-209/F-210; V0.4 minimal outcome storage precedes import; V0.5 confirmed report snapshots and capability action reviews.

V0.2 delivers pomodoro_cycle and work_interval.cycle_index together; verify unified pause/resume and current-cycle recovery before phase UI, no duplicate public work commands.

## 6. Workload baseline (measured 2026-10-12)

Scope: first commit 2026-10-02 to 2026-10-12, **302 commits**; tracked source/scripts/docs only
(`node_modules` / `target` / `gen` / `dist` / icons / lockfiles excluded).

| Area | Lines |
| --- | --- |
| **Rust business code** (domain 1,167 / services 11,823 / storage 3,692 / platform 3,008 / commands 3,348) | **23,949** |
| **Rust tests** (`src-tauri/tests/`) | **49,557** |
| **Frontend source** (pages 8,534 / state 3,263 / types 1,677 / components 705; 9,114 of it tests) | **16,505** |
| Contract snapshots (30 files) | 710 |
| Scripts (ps1/py) | 445 |
| Docs (`docs/*.md`) | 12,152 |
| Acceptance manuals (`manual-*.md`) | 1,507 |
| **Total** | **~104,800** |

Three takeaways for future estimates: business code is only ~30% (Rust 24k + frontend business ~7.4k ≈ **31k lines**);
**tests are 2.07x the business code** (verification, not features, dominates the effort); framework scaffolding is a few
hundred lines, while `platform/` + `storage/` (6.7k) is self-written infrastructure rather than template.

Throughput anchor: P8 = 19 commits / 20,405 net insertions delivering 13 commands, 8 pages, the bundle gate, tray
routing and two acceptance documents; V0.1 as a whole took 10 days for 20 F-items.

Estimated remaining work (±50%; later versions reuse the expensive foundation — schema/migration, revision+epoch
protocol, error contract, timer coordinator, crash recovery): V0.1b 2.5–4k business lines / 6–9k total / 3–5 days;
V0.2 8–12k / 25–35k / 2–3 weeks; V0.3 4–7k / 12–20k / 1.5–2.5 weeks; V0.4 5–8k / 15–24k / 2 weeks;
V0.5 4–6k / 12–18k / 1.5–2 weeks; V0.6 0.5–1k / 2–4k / 3–5 days; V1.0 3–5k / 10–15k / 2 weeks plus ≥2 weeks of
long-run observation. **Roughly 28–43k business lines / 80–125k lines including tests remain** — another V0.1 in
size, but spread over 3–4 months because the foundation exists. Biggest uncertainties: V0.2 (spans M05/M06/M07) and
V0.3 (AI reproducibility/evaluation engineering is expensive under this project's verification discipline).

> Note: this section and the V0.2/V0.6 items above are **recorded for later decision** — the user asked on
> 2026-10-12 to record them first and re-plan the design guidelines afterwards, so these numbers and decision
> points are **not yet approved scope**.
