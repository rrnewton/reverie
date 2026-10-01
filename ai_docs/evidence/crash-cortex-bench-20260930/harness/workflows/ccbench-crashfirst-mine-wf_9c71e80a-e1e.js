export const meta = {
  name: 'ccbench-crashfirst-mine',
  description: 'Crash-first mining: for top fleet MIDs outside the CrashCortex set, find landed human fixes in the kernel tree; skeptic-check each case',
  phases: [{ title: 'Mine', detail: '8 agents x 50 MIDs' }, { title: 'Skeptic', detail: 'refute-by-default re-check of each mined case' }],
}
const REPO = '/data/users/newton/linux-full.git'
const GITRULES = `Kernel tree: full bare mirror at ${REPO}; read-only git with --git-dir=${REPO} (log, show, merge-base --is-ancestor, tag --contains, rev-parse). Release branches refs/heads/v6.9-fb v6.11-fb v6.13-fb v6.16-fb v6.19-fb v7.1-fb; upstream mirrors refs/heads/linus-upstream net-next-upstream. scratch/* and <user>/* branches are NOT landed. A uname like 6.13.2-0_fbk16_rc1_0_g242a742712f1 => crash-kernel commit 242a742712f1. Never modify/fetch.`
const FIXC = { type: 'object', properties: { sha: { type: 'string' }, subject: { type: 'string' }, author: { type: 'string' }, author_email: { type: 'string' }, commit_date: { type: 'string' }, landed_on: { type: 'string' }, relation: { type: 'string', enum: ['exact_fix', 'equivalent_backport', 'partial_or_mitigation'] } }, required: ['sha', 'subject', 'author', 'author_email', 'commit_date', 'landed_on', 'relation'] }
const MINED = { type: 'object', properties: { cases: { type: 'array', items: { type: 'object', properties: {
  mid: { type: 'string' }, category_key: { type: 'string' }, sample_host: { type: 'string' }, sample_time: { type: 'integer' }, crash_kernel: { type: 'string' }, n_hosts: { type: 'integer' }, fix: FIXC,
  signature_match_evidence: { type: 'string' }, crash_kernel_lacks_fix: { type: 'boolean' }, production_validation: { type: 'string', description: 'e.g. "0 crashes of this MID on kernels containing the fix since <date> while those kernels had N crashes of other MIDs" or ""' },
  gt_root_cause: { type: 'string' }, family_label: { type: 'string' }, subsystem: { type: 'string' }, mechanism_class: { type: 'string' }, timing_dependent: { type: 'boolean' },
  answer_visible_in_input: { type: 'string', enum: ['none', 'low', 'high'] }, fair_test_rationale: { type: 'string' } },
  required: ['mid', 'category_key', 'sample_host', 'sample_time', 'crash_kernel', 'n_hosts', 'fix', 'signature_match_evidence', 'crash_kernel_lacks_fix', 'production_validation', 'gt_root_cause', 'family_label', 'subsystem', 'mechanism_class', 'timing_dependent', 'answer_visible_in_input', 'fair_test_rationale'] } } }, required: ['cases'] }
const idx = Array.from({ length: 8 }, (_, i) => i)
const out = await pipeline(idx, (i) => agent(
  `You are mining KNOWN-OUTCOME kernel crashes for a benchmark ("crash-first"). Read /home/newton/local/ccbench/data/crashfirst_chunk_${i}.json: 50 fleet crash MIDs (mid, category_key, distinct hosts, first/last time). None were investigated by CrashCortex.\n${GITRULES}\n` +
  `For each MID worth a look (skip out-of-tree vendor drivers, hardware/MCE, "unknown" categories, pure userspace-triggered BUG with no kernel defect), do: (1) get 1-3 concrete rows: meta presto.query execute --namespace infrastructure --output json --sql "SELECT hostname, time, kernel, category_key, panic FROM scuba_kernel_logview WHERE ds >= '2025-10-06' AND mid = '<mid>' ORDER BY time DESC LIMIT 5" (always filter on ds; never select dmesg). (2) From the category's top frames, search for a LANDED human fix after the crash kernel's commit on the release branch that kernel belongs to, or upstream: git log --no-merges --since=<crash kernel commit date> --format='%h %an %ae %cs %s' refs/heads/<branch> -- <files of the top frames>, and grep changelogs for the function names. Read candidate changelogs (git log -1 --format=%B). (3) Accept only if: the changelog clearly describes this crash's mechanism/signature, the crash kernel lacks it (merge-base --is-ancestor), author is a human kernel engineer, the raw crash would not name the fix. Optional but valuable: production_validation from Hive (does the MID stop on kernels that contain the fix?).\n` +
  `Return up to 7 solid cases (quality over quantity; zero is fine). Budget: spend at most ~40 minutes.`,
  { label: `crashfirst:${i}`, phase: 'Mine', schema: MINED, effort: 'high' }
), (res, i) => parallel((res?.cases || []).map(c => () => agent(
  `Adversarially re-check a mined benchmark case; default to refuting if you cannot confirm it yourself.\nCase: ${JSON.stringify(c)}\n${GITRULES}\n` +
  `Verify: the MID exists with that category (meta presto.query execute --namespace infrastructure --output json --sql "SELECT hostname, time, kernel, category_key FROM scuba_kernel_logview WHERE ds >= '2025-10-06' AND mid = '<mid>' LIMIT 10"); the fix is landed on a release/upstream branch; its changelog matches this crash; the sample crash kernel lacks it; the author is human; the sample host/time row exists. Return upheld and corrections.`,
  { label: `skeptic-cf:${(c.mid || '').slice(0, 8)}`, phase: 'Skeptic', effort: 'medium', schema: { type: 'object', properties: { upheld: { type: 'boolean' }, problems: { type: 'array', items: { type: 'string' } }, corrected_sample_host: { type: 'string' }, corrected_sample_time: { type: 'integer' } }, required: ['upheld', 'problems', 'corrected_sample_host', 'corrected_sample_time'] } }
).then(v => ({ ...c, miner: `crashfirst:${i}`, skeptic: v })))))
const M = out.filter(Boolean).flat().filter(Boolean)
log(`crash-first mined ${M.length}, upheld ${M.filter(c => c.skeptic && c.skeptic.upheld).length}`)
return { mined: M }
