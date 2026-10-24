# MakiMapCreator

## Overrides of global rules
- No cap of 3 parallel subagents in this project. Run as many as there are independent tasks (own worktree each, two writers never touch the same files).

## Multi-agent workflow
- Model per task, chosen before launch, stated in one line with the reason:
  - Opus: concurrency, state/replay/serialization, rendering backends, security-sensitive parsing, cross-module refactors, unexplained failures.
  - Sonnet: well-specified mechanical work (modules that follow existing patterns, UI screens, CLI wiring, config parsing, docs, small fixes, branch merges).
- Never pull full logs into context. Builds: filter to `error|warning:|FAILED`, `head -40`. Tests: failures only, `tail -40`. CI: failing job's tail only.
- Implementers do not poll CI. They run `ci.bat`, push, report. The orchestrator merges on a green `ci.bat` without waiting for CI, then checks the CI result the next time it touches the repo; if red, it sends the failing job and error back to the same implementer (continue it, do not start a new one).
- Build/test/code conventions live in `docs/CONVENTIONS.md` (create with the first build; keep under ~8 KB; add only binding rules, delete stale ones). Machine-specific facts go in a notes file outside the repo.
- Implementers do not edit shared notes. They end their report with at most 10 bullets of new conventions; the orchestrator folds them in at merge.
- Run independent tickets in parallel worktrees in the background; do not read subagent transcripts, wait for reports.
- Fast-forward merges directly; use a merger subagent only for real conflicts.
- When a dependency or network setting blocks progress, stop and tell the user exactly which host or setting to change.
- Code lookup: prefer the official LSP plugin for the project language (local sessions only). Consider Serena or another index only past ~50k lines.

## Implementer prompt template
    You are an implementer subagent. Implement ticket <NN> = <issue ref>.
    Read: the ticket (<how>), the spec (<path>), docs/CONVENTIONS.md, and the
    machine notes (<path>).
    Branch: <ticket/NN-slug> from <integration branch> (reset onto it if your
    worktree isn't based on it). Stay inside <module dirs>; edit shared files
    only by adding lines (another agent is working on <X> in parallel).
    Build test-first (Skill `tdd`). The orchestrator has agreed these seams;
    don't ask the user: <seams>.
    Local checks before pushing: `ci.bat` (plus <extra commands>). Keep output filtered to
    errors and failures. Do not poll CI.
    Commit with "Refs #<n>" and the attribution trailers. Merge the
    integration-branch tip before pushing.
    Report briefly: branch, SHA, status of each acceptance criterion, anything
    unverified, and at most 10 bullet points of new conventions.

## Project skills
- `.claude/skills/` holds Matt Pocock's skills v1.3.1 (upstream 49dd158). To update, re-copy the folders listed in upstream `.claude-plugin/plugin.json`.

## Agent skills

### Issue tracker

Issues live in GitHub Issues for MakiNoAtorie/MakiMapCreator (`gh` CLI). See `docs/agents/issue-tracker.md`.

### Triage labels

Default five-label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one root `GLOSSARY.md` + `docs/adr/`. See `docs/agents/domain.md`.
