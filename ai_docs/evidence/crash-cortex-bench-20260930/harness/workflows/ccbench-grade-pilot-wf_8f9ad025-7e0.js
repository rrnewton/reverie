export const meta = {
  name: 'ccbench-grade-pilot',
  description: 'Blind-grade the 3 pilot runs: two independent judges per run, third judge on any disagreement',
  phases: [{ title: 'Judge', detail: 'judge A (opus) and judge B (sonnet), blind' }, { title: 'Adjudicate', detail: 'third judge only where A and B disagree' }],
}
const G = '/data/users/newton/ccbench/grading'
const RUBRIC = `Rubric (score strictly against the sealed ground truth, not your own opinion of the kernel code):
R1 root_cause (0-3): 0 wrong; 1 right subsystem or only the fatal mechanism; 2 right function/structure but the originating-defect mechanism is incomplete or partly wrong; 3 matches the ground-truth originating defect.
R2 fix (0-3): 0 none/wrong; 1 mitigation/symptom-only fix; 2 equivalent fix per the ground truth's equivalent_fix_criteria; 3 exact (cites a ground-truth commit sha/subject as THE fix, or the patch is functionally identical).
R3 reproducer: attempted (bool); actually_ran (bool: artifacts or transcript facts show a guest kernel booted and the trigger executed; a plan or script that never ran = false); reproduced_failure (bool: the failure signature was observed).
R4 ab_verdict: one of n/a (no A/B claimed), correct (fail-before/pass-after evidence exists AND the tested patch is R2>=2 AND claims match the artifacts), unsupported (claimed without artifact evidence, or evidence does not show what is claimed), wrong (contradicts evidence or ground truth).
R6 calibration: stated_confidence (as written in the report), calibration one of ok / overconfident / underconfident.
Also: integrity_notes (anything suspicious, e.g. claims about tools/data the offline run could not have had).`
const J = { type: 'object', properties: {
  R1: { type: 'integer', minimum: 0, maximum: 3 }, R1_why: { type: 'string' },
  R2: { type: 'integer', minimum: 0, maximum: 3 }, R2_why: { type: 'string' },
  R3_attempted: { type: 'boolean' }, R3_actually_ran: { type: 'boolean' }, R3_reproduced_failure: { type: 'boolean' }, R3_why: { type: 'string' },
  R4: { type: 'string', enum: ['n/a', 'correct', 'unsupported', 'wrong'] }, R4_why: { type: 'string' },
  R6_stated_confidence: { type: 'string' }, R6: { type: 'string', enum: ['ok', 'overconfident', 'underconfident'] },
  integrity_notes: { type: 'string' } },
  required: ['R1', 'R1_why', 'R2', 'R2_why', 'R3_attempted', 'R3_actually_ran', 'R3_reproduced_failure', 'R3_why', 'R4', 'R4_why', 'R6_stated_confidence', 'R6', 'integrity_notes'] }
const runs = args.runs
const prompt = (r) => `You are a blind grader for a kernel-debugging benchmark. Grade ONE anonymous agent run (id ${r.bid}).\n` +
  `Files (read them all): sealed ground truth ${r.sealed}; the agent's report ${G}/${r.bid}.report.blind.md; its patch ${G}/${r.bid}.patch.diff; deterministic facts ${G}/${r.bid}.json (sha/subject citation, patch-vs-ground-truth file/function overlap, QEMU invocations whose output showed a boot, reproducer files); reproducer artifacts under ${G}/${r.bid}-artifacts/ (OUT_LISTING.txt lists everything the run left).\n` +
  `You may run read-only git against /data/users/newton/linux-full.git to compare the patch with the ground-truth fix (git --git-dir=/data/users/newton/linux-full.git show <sha>). Do not read anything under /data/users/newton/ccbench/runs/.\n` +
  (r.mode ? `v1.1 MODE: this run was in ${r.mode} mode; the agent saw ONLY the kernel tree ${r.case_repo} (cut at unix ${r.cutoff}). Judge any 'no upstream fix exists' / 'commit X is the only candidate' claim against THAT repo (git --git-dir=${r.case_repo} ...), not the full mirror; in derive mode the ground-truth fix was deliberately hidden, so not finding it is correct behaviour.\n` : '') +
  `${RUBRIC}\nQuote evidence in each *_why.`
const graded = await pipeline(runs,
  (r) => parallel([
    () => agent(prompt(r), { label: `judgeA:${r.bid}`, phase: 'Judge', schema: J, effort: 'high' }),
    () => agent(prompt(r), { label: `judgeB:${r.bid}`, phase: 'Judge', schema: J, model: 'sonnet', effort: 'high' }),
  ]),
  (pair, r) => {
    const [a, b] = pair
    if (!a || !b) return { bid: r.bid, a, b, final: a || b, adjudicated: false }
    const dis = []
    if (a.R1 !== b.R1) dis.push('R1')
    if (a.R2 !== b.R2) dis.push('R2')
    if (a.R3_actually_ran !== b.R3_actually_ran || a.R3_reproduced_failure !== b.R3_reproduced_failure) dis.push('R3')
    if (a.R4 !== b.R4) dis.push('R4')
    if (!dis.length) return { bid: r.bid, a, b, final: a, disagreements: [], adjudicated: false }
    return agent(prompt(r) + `\n\nTwo earlier judges disagreed on ${dis.join(', ')}. Judge A: ${JSON.stringify(a)}\nJudge B: ${JSON.stringify(b)}\nCheck the evidence yourself and give the final grade for ALL criteria.`,
      { label: `judgeC:${r.bid}`, phase: 'Adjudicate', schema: J, effort: 'max' })
      .then(c => ({ bid: r.bid, a, b, final: c, disagreements: dis, adjudicated: true }))
  })
return graded.filter(Boolean)
