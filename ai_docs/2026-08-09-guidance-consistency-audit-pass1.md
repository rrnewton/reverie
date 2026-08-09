# Guidance Consistency Audit — Pass 1 (2026-08-09)

===== A. INVENTORY (file -> conflict -> resolution -> owner-source) =====

1) MERGE POLICY (cited #97 rebase+FF vs #76 squash)
   Files: AGENTS.md:38, AGENTS.md:313, AGENTS.md:437, ai_docs/agents-md-policy-rationale.md:49, :88
   Conflict: NONE FOUND. Guidance is uniformly rebase-merge. AGENTS.md:38 "The commit trailer survives rebase merge"; :313 "Rebase merges preserve it on main"; rationale:49 names `gh pr merge --rebase` as "the final arbiter"; rationale:88 explicitly reasons "under rebase-merge".
   Only squash mention: .claude/skills/pr-landing-mechanics-merge-gate-uptodate-chase/SKILL.md:9, and that line ALREADY frames the `--admin --squash` escape as something that "conflict[s] with the current landing contract". That skill is itself already labeled "Deprecated compatibility alias for historical PR-landing mechanics."
   RESOLUTION: already reconciled to rebase+FF. NO FIX NEEDED. Owner-source: PROJECT_VISION.md + the rationale doc's landing contract (in-repo, dereferenceable).

2) BACKEND LIST (cited #10 drop-SaBRe vs #34/#65 keep-SaBRe)
   Files: PROJECT_VISION.md:7, :86, :112-114; .claude/skills/hermit-sabre/SKILL.md
   Conflict: NONE FOUND. A targeted search for drop/deprecate/retire/remove-sabre across AGENTS.md, PROJECT_VISION.md, both skills trees, docs/, and the rationale doc returns ZERO hits. PROJECT_VISION.md keeps SaBRe in three independent places: the Mission line ("ptrace, KVM, Liteinst first party, plus DBI, e9patch, sabre with 3rd party deps"), the per-backend agent roster ("hermit-kvm", "hermit-dbi", "hermit-sabre", "hermit-liteinst", "hermit-e9patch"), and a full "### Sabre Backend" section. A live hermit-sabre skill exists.
   RESOLUTION: **KEEP SABRE**; already consistent. NO FIX NEEDED. Owner-source: PROJECT_VISION.md, which is the owner-authored standing directive and the latest written statement.

3) SPECULATIVE -> FRONTIER RENAME (cited #25)
   Files: .claude/skills/research-planning-persona/SKILL.md:3, 13, 319, 343, 364
   Conflict: NONE. Zero references to a "speculative" BRANCH remain anywhere in the guidance set. The five surviving hits all use "speculative agents" — parallel exploratory agents that prototype candidate approaches — which is a SEPARATE AND STILL-CORRECT SENSE of the word, unrelated to the renamed branch.
   RESOLUTION: rename is COMPLETE. **DO NOT TOUCH THESE FIVE LINES.** Recording them here explicitly as a known intentional false positive, because a future regex sweep for "speculative" will re-flag them and a naive fix would corrupt the meaning of that skill.

4) KVM /dev/kvm ACCESS (cited #92/#93 vs #94)
   Files: AGENTS.md:334 (single occurrence in the whole guidance set)
   Conflict: NO INTERNAL CONTRADICTION. There is exactly one statement — /dev/kvm plus real-PMU counters are the "genuine residue" of privileged self-hosted execution, "each given minimum privilege" — and no second statement anywhere that disagrees with it.
   RESOLUTION: nothing to reconcile in-repo. Whether the CURRENT owner decision differs from that sentence cannot be determined without the numbered memory store (section C). ASK — see B3.

5) NOTIFICATION FORMAT (cited #84 vs #85-87, H2-headers vs no-headers)
   Files: NONE. Zero occurrences across AGENTS.md, PROJECT_VISION.md, both skills trees, docs/, ai_docs/agents-md-policy-rationale.md, scripts/, ci-hub/health/.
   RESOLUTION: there is NO guidance-file inconsistency because the policy is NOT WRITTEN DOWN ANYWHERE agents read. ASK — see B1.

6) GCHAT TAGGING (cited #109 vs #110)
   Files: NONE. Zero occurrences of gchat / google chat / @here / @all / tag-owner across the same set.
   RESOLUTION: same as (5) — not written down. ASK — see B2.

7) STRUCTURAL (not a cited hotspot; found during the sweep)
   CLAUDE.md is a SYMLINK to AGENTS.md (`CLAUDE.md -> AGENTS.md`, 9 bytes). They are byte-identical by construction and CANNOT diverge. Any future audit reporting an "AGENTS.md vs CLAUDE.md inconsistency" is a spurious finding; recording it so nobody re-derives it or tries to "sync" them.

===== B. QUESTIONS FOR THE OWNER (per instruction: ask, do not guess) =====

B1. NOTIFICATION FORMAT: what is the current decision — H2 headers, or no headers? It appears in NO guidance file, so agents cannot follow it. Please state it and I will write it into AGENTS.md (Communication Precision).
B2. GCHAT TAGGING: same — current tagging rule is absent from all guidance. What is it?
B3. KVM /dev/kvm: AGENTS.md:334 currently says /dev/kvm + real-PMU are the accepted "genuine residue... each given minimum privilege". Is that still the decision, or has it been superseded? No in-repo contradiction exists, so I will not touch it without a positive instruction.

===== C. WHY ZERO FIXES LANDED, AND THE STRUCTURAL BLOCKER =====

The task cites numbered owner memories (#10 #25 #27 #34 #65 #76 #84 #85-87 #92 #93 #94 #97 #109 #110) as the authority for "the owner's ACTUAL latest decision". Those numbers come from the ORC SESSION memory store, exposed as `orc.sessionMemories()`.

Per .orc/plugins/hermit-dev/memory-skill-sync.ts (lines 24-31), that store is documented as a MEASURED FACT to be:
  - "a SEPARATE store from the file-based one"
  - "the API returns a formatted markdown STRING numbered-list, no ids, no timestamps"
  - "**A bash gate cannot read that store.**"

I am a shell agent with no `orc.*` surface. The ORC coordinator has `orc.*` but, per AGENTS.md Role Boundary, HAS NO SHELL. So **no single actor can currently perform the reconciliation this task specifies**: the side that can read the decisions cannot read or edit the files, and the side that can edit the files cannot read the decisions. I believe this is why this has become a recurring standing mission that does not converge.

I deliberately did NOT guess at the three unreadable policies. Writing a notification-format or GChat rule I cannot dereference would be INVENTING policy, which this task explicitly forbids ("Reconcile only, don't invent policy").

UNBLOCK OPTIONS (owner's choice):
  (i) Paste the relevant memory texts into a task note here and I (or a successor) will reconcile mechanically; or
  (ii) have the coordinator dump `orc.sessionMemories()` to a file under ai_docs/ on a cadence, giving shell agents a dereferenceable mirror — this also fixes the problem permanently for every future sweep; or
  (iii) wire the already-authored-but-unwired memory-skill-sync heartbeat (that file documents the WIRING steps) so drift is reported automatically.
Option (ii) is the one I would pick: it converts an unreadable authority into a citable one, which is the root cause rather than the symptom.

===== D. SCOPE AND STATUS =====
This pass covered ALL SIX named hotspots plus the AGENTS/CLAUDE structural check, across AGENTS.md, CLAUDE.md, PROJECT_VISION.md, .agents/skills/, .claude/skills/, docs/, ai_docs/agents-md-policy-rationale.md, scripts/, ci-hub/health/. Four hotspots are settled from in-repo evidence (1,2,3 + the structural one); three need owner input (4,5,6).
NOT done: the open-ended remainder of "sweep ALL guidance/reference files" beyond the named hotspots — ai_docs/ alone holds 100+ documents. That is a fresh-agent job; I am near context exhaustion (~450k) and just handed off bump-reverie-pin-to-main-tip for the same reason.
Task left IN_PROGRESS (not closed): it is explicitly a STANDING mission, and B1-B3 are outstanding questions. Closing it would misreport unresolved questions as resolved.
