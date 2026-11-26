# Team agents: someone owns "to merged"

Status: design only, 2026-11-26. No code, no role edits, no config edits come with this doc. Written by a flow worker
with the architect (see "Architect review" at the end). Kai has not chosen among the open questions yet.

Why now: a triage of 24 open coder and devops PRs found them stalled after approval. Coder opens a PR and starts the
reviewer, and the reviewer approves. Then `kith-merge` was refused by the auto-mode classifier, or it became an approval
card. The reviewer's run ends on a refusal and nothing wakes it again. No agent owns getting a PR to merged. Kai,
2026-11-26: "I think we need new agent idea to fix this, team agents! maybe in a room or now, but work in flow, just
similar to this session." The proposed role-text edits (a `flow:merge-wanted` label, "coder isn't done until merged")
were not applied. This is the new design instead.

Read with: `docs/rooms-people.md`, `docs/architecture.md`, `docs/plans/one-bot-model.md`, and the agent roles under
`~/kith/agents/{coder,reviewer,devops,architect,product-lead}/role/agent.md`.

## 1. What the stall really is

Facts, checked 2026-11-26:

- `kith-merge` (the reviewer's `tools/kith-merge`) already enforces the merge rule M1-M5 itself: exact head SHA, checks
  passed on a head that contains main, a card for protected paths, at most once, and never the author. The reviewer's
  `review/SKILL.md` already covers approve, wait on `ci:`, merge, and resume on a card's yes.
- `auto_merge` is `true` for the reviewer on this Mac (`~/.kith/settings.json`). So a card should come only for a
  protected path or a base branch that is not strict.
- The reviewer's session notes record `kith-merge denied by classifier` on several PRs (#1135, #1138, #1141, #1145, #1180,
  #1181, #1187). A run that ends on a denial leaves no waiter: `~/.kith/sessions/waits/` is empty.
- `kith-wake` has waiter kinds `ci:`, `pid:`, `at:` and task. It has no `card:` or `pr:` kind. A card's yes resumes
  whoever ran `kith-merge`, which is the reviewer, not any other agent.

So there are two separate faults:

1. **A classifier refusal of an owner agent's own, rule-checked action.** That is a bug (Kai's rule). It is not fixed by
   a lead agent.
2. **Nothing re-reads the state.** A run that ends "waiting" depends on a wake that was never registered, a crash, or a
   refusal. Flow's managers had the same disease ("managers go idle") and the cure there was a person watching.

A team design has to fix the second fault for good and must not hide the first.

## 2. What a team is

A team is **agents plus a claim plus a schedule**. It is not a new kernel concept and not a new room type.

- **Lead.** A new owner-only agent, `lead`, written as a role package (builder makes it). It does what a flow manager
  does for one task: preflight, brief the workers, review their PRs, and chase the task until it is merged. It runs on
  Claude (the wake clock cancels wakes for non-Claude engines).
- **Members.** The agents that already exist, each keeping its one capability:

| Role | Agent | Does | Never does |
|---|---|---|---|
| Worker | coder | one job, one worktree, one PR | review or merge its own PR |
| Reviewer | reviewer | review, approve a SHA, `kith-merge` | write the PR's code, change `auto_merge` |
| Design | architect | design before a cutover or cross-cutting package | write code, open PRs |
| CI health | devops | CI backlog and speed briefs | change code (sends a brief to coder via the lead) |
| Reach Kai | product-lead | cards, digest, `kith ask product-lead` blockers | decide for Kai |

- **Claim.** One row per work item (a task, then its PR) saying which run owns it. It lives in Kith, not in the PR text,
  because the PR author can edit the text and two runs could adopt one PR from a text marker. Where the row is kept
  (a core table or a file) is the implementer's choice, but concurrent adoption of one PR needs a model check before it
  ships (the thread-writers skill is the template).
- **Schedule.** A level-triggered sweep (section 4) re-reads GitHub and the claims and acts on what it finds. This is the
  main mechanism. Wakes only make it faster.

### Exactly one owner of "to merged"

| Item state | Owner |
|---|---|
| Task, no PR yet | the lead that holds the claim |
| PR open, claimed by a lead | that lead (it chases; it never merges) |
| PR open, no lead (the 24 today, any PR coder opens alone) | the sweep, run as the reviewer's scheduled run |
| Merge act itself | always the reviewer, through `kith-merge` (M5) |

"Owns to merged" means "is the one who notices it is stuck and moves it". The merge act stays the reviewer's, so review
stays independent of whoever drives the work. A claim moves to the sweep only when the lead run ends or is released.
Two owners at once is a bug the claim row refuses.

### Why not the other shapes

- **A room per team as the control path.** No. A group room is people and agents talking, with a turn budget
  (`#435`, default 10) and late replies that wake nobody. Control must not depend on a chat turn. A room is an optional
  view: the lead posts one outcome line per item there, and Kai can speak in it. `kith group add-bot` (`#448`) is the
  way to put the lead in a room.
- **A config on coder or reviewer.** Reviewer cannot also drive: M5 and independence. Coder cannot also own the merge: it
  is the author.
- **A kernel concept.** A team would be justified in the kernel only by a limit the kernel enforces, such as a budget or
  access per team. We have no such need now. If one comes (a per-team spend cap), revisit.
- **The deleted `team` and `routines` MCP (#1096).** That was a chat-side tool that let one bot call others. This design
  has no such tool. The lead uses `kith ask --bg <agent>` and `kith-wake`, both existing, and each member keeps its own
  permissions.

## 3. How work flows

Same shape as this flow session: main splits, managers own, workers build in worktrees, a reviewer merges.

1. **Task in.** Kai or product-lead sends a task (`kith ask lead -p <ID> "..."`). Asked by a person, a brief with
   "done when" lines. Product-lead asks before paid work (its rule), so a task from it carries Kai's go.
2. **Preflight.** The lead writes acceptance, checks the claims for the same files (two items touching the same files
   are serialised or merged into one), and asks the architect when it is a cutover or cross-cutting (design first).
   Blocking questions go to Kai through product-lead before any worker starts.
3. **Workers.** The lead briefs coder, one job per brief, each in its own worktree (coder's own rule). Parallel workers
   are capped (section 7).
4. **Review.** The lead reads each PR as flow managers do and starts the reviewer (`kith ask --bg reviewer`).
5. **Merge.** The reviewer approves the exact SHA and runs `kith-merge`. The lead then waits on `ci:<repo>#<pr>`, which
   also fires on merge or close.
6. **Done.** Merged: the claim closes, one outcome line goes to Kai (section 5).

### When it goes wrong

Routing is by reason, not "back to coder":

| What happened | Who moves it |
|---|---|
| Approved, merge refused: head moved | reviewer re-reviews the new head |
| Refused: behind main | reviewer runs `gh pr update-branch`, re-approves (its skill already does) |
| Refused: checks not finished | wait on `ci:` again |
| CI red, a known flake in `verify.md` | one rerun, then wait |
| CI red, real | lead briefs coder with the failing job and its error |
| Merge conflict GitHub cannot resolve | lead briefs coder |
| Card pending (protected path, or auto_merge off) | the reviewer's run waits on the card; the lead reports it to Kai as a decision (section 5). Only Kai answers it |
| Classifier refuses `kith-merge` | a bug: lead and sweep file it (section 6), and stop. No retry loop, no other way to merge |
| Worker out of context | coder's own handoff: job file plus WIP commit and PR note; the lead starts a continuation on the same branch |
| Worker or reviewer silent past a bound | the sweep finds it (below); the lead asks once, then tells Kai |
| Duplicate merge attempt | `kith-merge` says "already merged" and exits 0 (M4). Safe |
| A PR with no agent at all | the sweep adopts it (section 9) |

### Idle leads (the "managers go idle" failure)

A lead that ends its turn "waiting on CI" and has no waiter is stalled. A prompt rule ("always register a wake") cannot
be enforced: a crash or a refused call ends a one-shot with none. So:

- **The sweep is the guarantee.** Every pass re-reads, for each open PR: state, head SHA, approval on that SHA, checks,
  behind main, claim holder, age of last activity. It does not trust any earlier run's belief.
- **Wakes are the speed-up.** The lead registers `ci:` waiters when it can. The sweep runs every 30 minutes at first.
- **The lead's notes** (one file per claim in the lead's own memory, appended each step, like flow's manager notes) let a
  restarted lead continue. Waiters and the queue are files delivered once across restarts (`kith-wake` W2), but a
  host chat cannot set waiters yet, so the lead runs from the terminal or `kith ask` only.
- **A lead's own staleness** (no note appended for a bound, with open items) is itself a thing the sweep reports.

### Throughput

With "up to date" required on main, each merge makes the other open PRs behind (one PR took six updates). A GitHub merge
queue on main fixes that churn and the check-then-merge race. Neither a lead nor a sweep can. It is in Open question 4.

## 4. The sweep

One scheduled run (`reviewer`, wake-driven, no new agent). It is read-first and acts through the existing verbs:

- For each open PR with a claim, check the claim holder is alive. Dead, or older than the bound: release the claim.
- For each PR without one: take a claim (one sweep at a time), then act by the table in section 3 (start the reviewer,
  update the branch, or send coder the failing job).
- A PR whose approval or check state is unchanged since the last pass, for N passes, goes into the digest to Kai as
  "stuck: <reason>", once.
- It never merges, never changes `auto_merge`, never writes the protected list.

## 5. How Kai sees it

Kith's rule: screens serve a person's scarce attention. Decisions and outcomes first; plumbing collapsed.

- **Outcomes:** one line per item through `kith-send` (and the room, if one is set): `<repo>#<pr> merged` or
  `<repo>#<pr> stuck: <reason>`. No chatter on each wake.
- **Decisions:** only what truly needs Kai, each with a recommendation: a card for a protected-path merge (Kai's alone),
  a product question, an env change, an irreversible action. These go through product-lead (`kith ask product-lead`), at
  most three cards at once (its rule).
- **Blockers:** a refused action of an owner agent goes to product-lead as a bug report with the exact error, not as a card.
- **Plumbing:** job files, review files, lead notes and claim rows sit in the agents' folders. The app shows them
  collapsed under the item.
- **No new screen at first.** Cards, the digest and the room are enough. An "Items" list (claim, state, owner, age) is
  Phase 3.

## 6. Permissions

- **No permission laundering.** A member never gets a right by asking another agent that has it. The lead cannot ask the
  reviewer to do something the lead was refused, and coder cannot ask the lead to merge. Each agent acts under its own
  profile, and a request from an agent is the owner's request only for work that agent could itself do.
- **Merge rights.** Only the reviewer runs `kith-merge`, and `kith-merge` enforces M1-M5 itself. The lead and the sweep
  hold no merge right. They can start the reviewer, which is the same thing coder already does.
- **Cards are Kai's alone.** No agent approves, auto-answers, or clicks a card. `auto_merge` is Kai's switch; no team
  member changes it.
- **A classifier refusal of an owner agent is a bug.** The team does not retry with another form, write an allow list,
  switch to `dontAsk`, or add a gate. It files the bug (the exact refused command and the classifier's reason) to
  product-lead and marks the item "stuck: refused". Because `kith-merge` already does the rule's checks, the likely fix
  is on the classifier side and is Kai's decision, not the team's (Open question 1).
- **Lead run mode.** Owner-only, like coder and reviewer. It has no secret access and no deploy right.

## 7. Cost and machine load

- **Lead on Sonnet, wake-driven.** A lead mostly waits. It should cost a few small turns per PR. Opus stays with the
  reviewer and architect.
- **Cap parallel workers** (start with 3 per lead, 4 overall). Each worker is a full coder run.
- **Heavy builds go through a slot.** Kith's CI is the test (workers run fmt and one clippy). Where a local build is
  unavoidable, it takes the machine's test slot; the lead does not start another worker while a slot is queued.
- **The ledger.** `kith cost agents` already tags every request by agent. Add a `team` or `item` tag only if the kernel
  ledger can carry it without a schema change. Otherwise the lead reads `kith cost agent <name>` per item and writes the
  total in its closing note. The lead has a per-item budget (Open question 3); on reaching it, it stops and asks Kai.
- **The sweep is cheap by design:** one `gh` read per PR, an agent turn only when something changed.

## 8. Relation to the flow plugin

- **Reused:** the shape (main, manager, worker, reviewer), preflight criteria, the notes file, the decision inbox idea
  (cards plus product-lead), sizes of work, the test slot, worktrees.
- **Replaced:** flow's plugin tools (`ask`, `fyi`, `handover`, `plan`) are Claude Code-session tools and need that session
  alive. In Kith the same jobs are done by `kith ask`, cards, the PR body's Verification section, and the claim.
- **Flow keeps running Kith's own dev work** until a lead has merged a month of PRs without a stall (Phase 4). The two
  must not touch the same PR: the claim row is the rule, and flow's PRs carry a `flow/` branch the sweep skips.

## 9. The 24 stalled PRs

- The sweep's first pass is a **read-only inventory**: for each, state, head SHA, approval on head, checks, behind main,
  why `kith-merge` failed last, last activity.
- Sort into: (a) approved and green, refused by the classifier (waits on Open question 1); (b) approved, card pending
  (Kai's cards; product-lead lists them); (c) red CI (briefs to coder); (d) conflicting (coder); (e) no approval yet
  (start the reviewer); (f) stale or superseded (reported to Kai to close; the team never closes a PR it did not author).
- Adoption is a claim to the sweep. No label is added and no PR text is edited.

## 10. Phases

- [ ] **P0 (smallest useful step): fix the refusal, read the stall.**
  - [ ] Kai answers Open question 1 about the `kith-merge` classifier refusal. The reviewer's existing skill then drains
    group (a) of section 9 with no new agent.
  - [ ] A read-only inventory of the 24 PRs (section 9), posted to Kai as one digest. No permission changes.
- [ ] **P1: a level-triggered sweep.** A schedule that runs section 4 as the reviewer. One sweep at a time. No new agent.
  The claim row for PRs. Model check of concurrent adoption.
- [ ] **P2: the lead agent,** for work before a PR exists (brief, preflight, workers, design first). Role written by
  builder, reviewed by the architect. First tasks: devops's CI backlog (devops sends briefs, the lead runs them).
- [ ] **P3: the room view and an Items list in the app.**
- [ ] **P4: flow retires for Kith's own dev work,** if P2 shows a month of clean merges.

P0 is the only step that unsticks the 24 soonest. It breaks no permission rule if Kai chooses the classifier fix himself
(the team does not).

## 11. Open questions for Kai

1. **The classifier refusal of `kith-merge`.** `kith-merge` already enforces M1-M5 itself, so the classifier guards
   nothing extra. Options: (a) the classifier's cause is found and fixed in Kith's launch for owner agents (a bug fix,
   fits your rule); (b) an allow rule for the reviewer's `kith-merge` path (an allow list, which your rule excludes unless
   you decide it); (c) leave it and merge by hand. Recommended: (a); the classifier's refusal reason is read first. I do
   not propose (b) on my own.
2. **A lead agent at all?** (a) Yes, P2, after the sweep proves out; (b) never, only the sweep. Recommended: (a).
3. **A per-item token or cost budget for a lead.** Options: $5, $15, none. Recommended: $15, then ask Kai.
4. **A GitHub merge queue on main.** Options: (a) turn it on, so merges stop making the rest behind; (b) keep strict
   up-to-date and let the reviewer update branches. Recommended: (a). It is a GitHub setting and Kai's.
5. **How often does the sweep run?** Options: every 30 minutes, hourly, daily. Recommended: every 30 minutes while there
   is a backlog, then hourly.

## Architect review

Outline review (answered, 2026-11-26):

- **Agreed with:** a team is agents plus a schedule, not a kernel concept; no `teams/<name>.md` in the first phases; reviewer
  already owns the merge act and review must stay independent of the driver; the lead is only for work before a PR exists;
  a lead needs Claude (wake clock).
- **Changed because of it:** my first outline made a "lead on every PR" with a `lead:` line in the PR body and "every turn
  ends with a wake". The architect showed a prompt cannot enforce a wake and PR text is editable. So: the level-triggered
  sweep is the guarantee, the claim lives in Kith, wakes are only a speed-up, failures are routed by reason, and a merge
  queue is an open question. The architect's order (P0 unblock, P1 sweep, P2 lead) is adopted.
- **Still differs:** the architect's P0 is "Kai says yes to a Bash allow rule for `kith-merge`". Kai's rule says a classifier
  refusal of an owner agent is a bug and an allow list is not the fix. This doc lists the allow rule as option (b) in
  Open question 1 and recommends fixing the cause (a). Kai decides.

Draft review: **not received.** I sent the full draft with `kith ask architect -c -p kith`. It answered "still running ...
queued" because the architect's earlier background run (`bg-20261121140850-40a903`) did not finish while I waited
(about 15 minutes, three waits). Nothing was refused. The architect has not seen the final text. Treat the draft as
unreviewed until it answers; the PR says the same.
