export const meta = {
  name: 'ccbench-ground-truth',
  description: 'Adjudicate known outcomes for 165 CrashCortex MIDs (git-verified, skeptic-checked) and mine extra fixed crashes from the kernel tree + Hive',
  phases: [
    { title: 'Adjudicate', detail: 'one agent per MID: verify fix commits in the kernel mirror, search for later landed fixes, tier + fairness' },
    { title: 'Mine', detail: 'per-subsystem miners: landed fixes whose changelog carries a crash signature, mapped to Hive MIDs' },
    { title: 'Skeptic', detail: 'adversarial re-check of every tier A/B case' },
  ],
}

const REPO = '/data/users/newton/linux-full.git'
const GITRULES = `Kernel tree: a full bare mirror of Meta's kernel repo at ${REPO}. Use ONLY read-only git commands with --git-dir=${REPO} (log, show, merge-base --is-ancestor, tag --contains, branch --contains, rev-parse, cat-file). Release branches: refs/heads/v6.9-fb v6.11-fb v6.13-fb v6.16-fb v6.19-fb v7.1-fb; upstream mirrors: refs/heads/linus-upstream net-next-upstream main. Personal/scratch branches (scratch/*, <user>/*) are NOT landed. A crash kernel uname like 6.13.2-0_fbk16_rc1_0_g242a742712f1 means crash-kernel commit 242a742712f1. Never modify the repo, never fetch.`

const FIXC = { type: 'object', properties: {
  sha: { type: 'string' }, subject: { type: 'string' }, author: { type: 'string' }, author_email: { type: 'string' },
  commit_date: { type: 'string' }, landed_on: { type: 'string', description: 'release branches / tags containing it, e.g. "v6.16-fb (v6.16-fbk5)"' },
  relation: { type: 'string', enum: ['exact_fix', 'equivalent_backport', 'partial_or_mitigation'] } },
  required: ['sha', 'subject', 'author', 'author_email', 'commit_date', 'landed_on', 'relation'] }

const ADJ = { type: 'object', properties: {
  mid: { type: 'string' },
  gt_tier: { type: 'string', enum: ['A', 'B', 'C', 'X'] },
  exclusion_reason: { type: 'string', description: '"" unless tier C/X' },
  gt_root_cause: { type: 'string', description: 'originating defect, 1-3 sentences, name functions/files' },
  gt_fatal_mechanism: { type: 'string' },
  gt_fix_commits: { type: 'array', items: FIXC },
  gt_evidence: { type: 'array', items: { type: 'string' }, description: 'concrete evidence: changelog quote, human comment quote, merge-base results you ran' },
  crash_kernel_shas: { type: 'array', items: { type: 'string' } },
  crash_kernel_has_fix: { type: 'boolean' },
  fix_available_at_crash_time: { type: 'boolean', description: 'fix committed on a release/upstream branch before the crash/investigation time' },
  bot_was_right: { type: 'string', enum: ['yes', 'partial', 'no', 'unknown'] },
  family_label: { type: 'string', description: 'short reusable defect-family label, e.g. "csdlock_debug synchronous printk feedback loop"' },
  subsystem: { type: 'string' },
  mechanism_class: { type: 'string', enum: ['data_race_or_missing_sync', 'lock_ordering_deadlock', 'lock_hold_too_long_or_starvation', 'use_after_free_lifetime', 'null_deref_or_missing_check', 'refcount_or_leak', 'accounting_or_invariant_violation', 'livelock_or_feedback_loop', 'resource_exhaustion', 'config_or_tunable', 'hardware_fault', 'firmware_bug', 'oot_driver_bug', 'bpf_or_sched_ext', 'other'] },
  timing_dependent: { type: 'boolean' },
  answer_visible_in_input: { type: 'string', enum: ['none', 'low', 'high'] },
  answer_visible_why: { type: 'string' },
  needs_oot_source: { type: 'boolean', description: 'true if diagnosing/fixing requires source not in the kernel tree (NVIDIA, BPF scheduler repo, vendor driver)' },
  difficulty: { type: 'string', enum: ['easy', 'medium', 'hard'] },
  fair_test_rationale: { type: 'string' },
  pin_host: { type: 'string', description: 'investigated host from the post if stated, else ""' },
  pin_time: { type: 'integer', description: 'crash time of that host if stated, else 0' },
  notes: { type: 'string' } },
  required: ['mid', 'gt_tier', 'exclusion_reason', 'gt_root_cause', 'gt_fatal_mechanism', 'gt_fix_commits', 'gt_evidence', 'crash_kernel_shas', 'crash_kernel_has_fix', 'fix_available_at_crash_time', 'bot_was_right', 'family_label', 'subsystem', 'mechanism_class', 'timing_dependent', 'answer_visible_in_input', 'answer_visible_why', 'needs_oot_source', 'difficulty', 'fair_test_rationale', 'pin_host', 'pin_time', 'notes'] }

const TIERS = `Tier rules (be strict; the point is KNOWN outcomes, not the bot's opinion):
- A: a landed fix commit exists on a release branch or upstream mirror, authored by a human kernel engineer (NOT the CrashCortex pipeline / dcostantino-authored bot patches), its changelog (READ IT: git log -1 --format=%B <sha>) describes this crash's mechanism or signature, and it is NOT an ancestor of the crash kernel (merge-base). OR a human expert comment explicitly names the fix commit or confirms the root cause (then still locate the fix if possible).
- B: the outcome is known but less independent: the landed fix is a CrashCortex-derived patch (dcostantino or bot pipeline) that was reviewed and landed on a release branch; or a human acknowledged the diagnosis and a landed fix matches it.
- C: only the bot's claim (no landed fix you can find, no human confirmation) -> not a known outcome.
- X: exclude: pure hardware/firmware fault; fix lives only in an out-of-tree vendor/BPF-scheduler repo that is not in this kernel tree; the claimed fix is already in the crash kernel and no other fix exists; the raw crash input alone names the answer; insufficient data.
If the post names no landed fix, SEARCH for one that landed after the crash: e.g. git --git-dir=${REPO} log --since=<crash date> --format='%h %an %cs %s' refs/heads/<release branch> -- <implicated files>, and grep changelogs for the crashing functions (git log --since=... --grep=<func> ...). A fix whose changelog does not clearly match the mechanism does not count.`

phase('Adjudicate')
const mids = args.mids
const minersSpec = args.miners
const cc = args.crashcortex_mids_file

const adjudicated = pipeline(mids, (mid) => agent(
  `You are adjudicating the GROUND TRUTH for one kernel crash (MID ${mid}) for a blind benchmark of kernel-debugging agents.\n` +
  `Read /home/newton/local/ccbench/data/mid_records/${mid}.json : the CrashCortex post extraction(s) (the bot's claims), pre-computed git facts for each fix the bot named (exists/author/branches/in_crash_kernel), the Hive crash population by kernel/model, and any human comments.\n` +
  `${GITRULES}\n${TIERS}\n` +
  `Also judge: bot_was_right (did the original CrashCortex post identify the ground-truth root cause/fix), timing_dependent (race/timing/contention/concurrency window), answer_visible_in_input (would the raw dmesg/backtrace/category alone reveal the fix), needs_oot_source, difficulty, and a one-sentence fair_test_rationale (why an agent given only the frozen crash evidence + the kernel tree cut at crash time can be graded on this).\n` +
  `Put every git command result you relied on into gt_evidence (short). Return the object.`,
  { label: `adj:${mid.slice(0, 8)}`, phase: 'Adjudicate', schema: ADJ, effort: 'high' }
), (adj, mid) => {
  if (!adj || (adj.gt_tier !== 'A' && adj.gt_tier !== 'B')) return adj
  return agent(
    `Adversarially re-check a claimed ground truth for kernel crash MID ${mid}. Default to refuting if you cannot confirm a point yourself.\n` +
    `Claim (JSON): ${JSON.stringify({ tier: adj.gt_tier, root_cause: adj.gt_root_cause, fixes: adj.gt_fix_commits, crash_kernel_shas: adj.crash_kernel_shas, crash_kernel_has_fix: adj.crash_kernel_has_fix, fix_available_at_crash_time: adj.fix_available_at_crash_time })}\n` +
    `Source record: /home/newton/local/ccbench/data/mid_records/${mid}.json\n${GITRULES}\n` +
    `Check independently with git: (1) each fix sha exists and is contained in a release branch or upstream mirror (not only scratch/personal); (2) it is NOT an ancestor of every crash-kernel commit; (3) its changelog really describes this crash's mechanism/signature (quote it); (4) tier A requires a human (non-CrashCortex-pipeline) author or an explicit human confirmation; (5) fix_available_at_crash_time is correct (commit date vs the crash times in the record).\n` +
    `Return verdict upheld=true only if all hold for the stated tier; otherwise give the corrected tier.`,
    { label: `skeptic:${mid.slice(0, 8)}`, phase: 'Skeptic', effort: 'high', schema: { type: 'object', properties: {
      upheld: { type: 'boolean' }, corrected_tier: { type: 'string', enum: ['A', 'B', 'C', 'X'] },
      problems: { type: 'array', items: { type: 'string' } }, checks_run: { type: 'array', items: { type: 'string' } } },
      required: ['upheld', 'corrected_tier', 'problems', 'checks_run'] } }
  ).then(v => ({ ...adj, skeptic: v }))
})

const MINED = { type: 'object', properties: { cases: { type: 'array', items: { type: 'object', properties: {
  mid: { type: 'string' }, category_key: { type: 'string' }, sample_host: { type: 'string' }, sample_time: { type: 'integer' },
  crash_kernel: { type: 'string' }, n_hosts: { type: 'integer' }, fix: FIXC,
  signature_match_evidence: { type: 'string', description: 'why this MID is the crash the changelog fixes (frames/panic string matched)' },
  crash_kernel_lacks_fix: { type: 'boolean' }, gt_root_cause: { type: 'string' }, family_label: { type: 'string' },
  subsystem: { type: 'string' }, mechanism_class: { type: 'string' }, timing_dependent: { type: 'boolean' },
  answer_visible_in_input: { type: 'string', enum: ['none', 'low', 'high'] }, fair_test_rationale: { type: 'string' } },
  required: ['mid', 'category_key', 'sample_host', 'sample_time', 'crash_kernel', 'n_hosts', 'fix', 'signature_match_evidence', 'crash_kernel_lacks_fix', 'gt_root_cause', 'family_label', 'subsystem', 'mechanism_class', 'timing_dependent', 'answer_visible_in_input', 'fair_test_rationale'] } } },
  required: ['cases'] }

const mined = pipeline(minersSpec, (m) => agent(
  `You are mining ADDITIONAL known-outcome kernel crashes for a benchmark, in the slice: ${m.slice} (paths: ${m.paths}).\n` +
  `${GITRULES}\n` +
  `Method: (1) list human-authored non-merge commits landed on Meta release branches (v6.13-fb v6.16-fb v6.19-fb v7.1-fb, also v6.9-fb v6.11-fb) since 2025-10-01 in those paths whose changelog contains a crash signature (e.g. "BUG:", "WARNING:", "soft lockup", "hard LOCKUP", "rcu_sched self-detected stall", "general protection fault", "NULL pointer dereference", "list_add corruption", "KASAN", a Call Trace, or "Fixes:" plus a described crash). Example: git --git-dir=${REPO} log --no-merges --since=2025-10-01 --format='%H%x09%an%x09%ae%x09%cs%x09%s' refs/heads/v6.16-fb -- <paths> then read candidates with git log -1 --format=%B.\n` +
  `(2) For a promising fix, take its crashing frames and find matching fleet crashes in Hive: meta presto.query execute --namespace infrastructure --output json --sql "SELECT mid, category_key, kernel, COUNT(*) n, COUNT(DISTINCT hostname) nh, MIN(time) t0, MAX(time) t1, ARBITRARY(hostname) h FROM scuba_kernel_logview WHERE ds >= '2025-10-06' AND category_key LIKE '%<func>%' GROUP BY 1,2,3 ORDER BY nh DESC LIMIT 30" (keep queries narrow; always filter on ds).\n` +
  `(3) Keep a case only if: the MID's category/frames match the changelog's crash, >=3 distinct hosts, the crash kernel commit (_g<sha> in the uname) does NOT contain the fix (git merge-base --is-ancestor), and the raw crash would not name the fix. Pick a concrete sample host + time (a row with that kernel).\n` +
  `(4) Skip MIDs listed in ${cc} (already in the CrashCortex set). Skip out-of-tree vendor drivers.\n` +
  `Return up to ${m.target} solid cases (quality over quantity; zero is acceptable). Include the fix sha/subject/author/date/branch.`,
  { label: `mine:${m.key}`, phase: 'Mine', schema: MINED, effort: 'high' }
), (res, m) => parallel((res?.cases || []).map(c => () => agent(
  `Adversarially re-check a mined benchmark case. Default to refuting if you cannot confirm it yourself.\nCase: ${JSON.stringify(c)}\n${GITRULES}\n` +
  `Verify with git and one narrow Hive query (meta presto.query execute --namespace infrastructure --output json --sql "SELECT hostname, time, kernel, category_key FROM scuba_kernel_logview WHERE ds >= '2025-10-06' AND mid = '<mid>' LIMIT 20"): the MID exists with that category, the fix is landed (release/upstream branch), its changelog matches this crash signature, the sample crash kernel lacks it, author is human. Return upheld and corrections.`,
  { label: `skeptic-mined:${(c.mid || '').slice(0, 8)}`, phase: 'Skeptic', effort: 'medium', schema: { type: 'object', properties: {
    upheld: { type: 'boolean' }, problems: { type: 'array', items: { type: 'string' } }, corrected_sample_host: { type: 'string' }, corrected_sample_time: { type: 'integer' } },
    required: ['upheld', 'problems', 'corrected_sample_host', 'corrected_sample_time'] } }
).then(v => ({ ...c, miner: m.key, skeptic: v })))))

const [adj, mn] = await Promise.all([adjudicated, mined])
const A = adj.filter(Boolean)
const M = mn.filter(Boolean).flat().filter(Boolean)
log(`adjudicated ${A.length}/${mids.length}; tiers: ` + ['A', 'B', 'C', 'X'].map(t => `${t}=${A.filter(a => a.gt_tier === t).length}`).join(' ') +
    `; skeptic-upheld A/B: ${A.filter(a => a.skeptic && a.skeptic.upheld).length}; mined ${M.length}, upheld ${M.filter(c => c.skeptic && c.skeptic.upheld).length}`)
return { adjudicated: A, mined: M }
