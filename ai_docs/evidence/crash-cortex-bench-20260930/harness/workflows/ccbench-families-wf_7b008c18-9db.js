export const meta = {
  name: 'ccbench-families',
  description: 'Cluster 125 known-outcome crash candidates into canonical defect families, with an independent critic pass',
  phases: [{ title: 'Cluster', detail: 'two independent clusterings' }, { title: 'Reconcile', detail: 'critic merges into one canonical family map' }],
}
const F = { type: 'object', properties: { families: { type: 'array', items: { type: 'object', properties: {
  family_id: { type: 'string', description: 'F01, F02, ...' }, family_name: { type: 'string', description: 'short: subsystem + defect mechanism, e.g. "btrfs: lock held across long scan (cond_resched_lock)"' },
  definition: { type: 'string', description: 'one sentence: what makes a case a member' }, members: { type: 'array', items: { type: 'string' }, description: 'MIDs' } },
  required: ['family_id', 'family_name', 'definition', 'members'] } } }, required: ['families'] }
const base = `Read /home/newton/local/ccbench/data/cluster_input.json (125 kernel-crash benchmark candidates: mid, family_label, subsystem, mechanism, root_cause, fix_subjects, category_key).\n` +
  `Group them into canonical DEFECT FAMILIES for stratified sampling. A family = same subsystem area AND same defect mechanism class (e.g. "btrfs: missing/incorrect error-path handling leading to NULL deref", "sched_ext: task/cgroup lifecycle race in kernel core", "mm: slab/free-path recursion or accounting", "csd-lock/printk console feedback loop"). Two crashes of the same underlying bug (same fix commit) MUST be in the same family. Aim for roughly 25-45 families; avoid singleton families unless genuinely unique, and avoid families bigger than ~12 (split by mechanism). Every MID must appear in exactly one family.`
phase('Cluster')
const [a, b] = await parallel([
  () => agent(base + `\nCluster primarily by DEFECT MECHANISM within subsystem.`, { label: 'cluster:mechanism', phase: 'Cluster', schema: F, effort: 'high' }),
  () => agent(base + `\nCluster primarily by the CODE AREA the fix touches (fix_subjects), then mechanism.`, { label: 'cluster:code-area', phase: 'Cluster', schema: F, effort: 'high' }),
])
phase('Reconcile')
const final = await agent(base + `\nTwo independent clusterings were produced. A (mechanism-first): ${JSON.stringify(a)}\nB (code-area-first): ${JSON.stringify(b)}\n` +
  `Produce ONE reconciled canonical family map. Check: every MID exactly once; same-fix duplicates together; names precise; ids F01.. in descending family size. Output the final families.`,
  { label: 'reconcile', phase: 'Reconcile', schema: F, effort: 'high' })
return final
