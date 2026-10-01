export const meta = {
  name: 'ccbench-extract-posts',
  description: 'Extract structured crash/root-cause/fix facts from 272 Kernel AI Investigations posts (28 batches)',
  phases: [{ title: 'Extract', detail: 'one agent per 10-post batch file' }],
}

const FIX = {
  type: 'object',
  properties: {
    sha: { type: 'string', description: 'commit hash exactly as written in the post (12-40 hex), or "" if only a title is given' },
    title: { type: 'string', description: 'commit subject line as quoted in the post, or ""' },
    kind: { type: 'string', enum: ['exact_root_cause_fix', 'backport_of_fix', 'mitigation_or_symptom_fix', 'related_or_similar', 'introducing_commit', 'bot_proposed_patch', 'other'] },
    where: { type: 'string', enum: ['upstream_mainline', 'meta_kernel_branch', 'nvidia_or_oot_vendor', 'proposed_patch_only', 'unknown'] },
    claimed_status: { type: 'string', enum: ['absent_from_crash_kernel', 'present_in_crash_kernel', 'landed_after_crash', 'not_yet_landed', 'unknown'] },
    author_if_stated: { type: 'string' },
  },
  required: ['sha', 'title', 'kind', 'where', 'claimed_status', 'author_if_stated'],
}

const POST = {
  type: 'object',
  properties: {
    post_id: { type: 'string' },
    mid: { type: 'string', description: '32-hex MID or "" if absent' },
    category_key: { type: 'string', description: 'crash category from the title, e.g. lockup::queued_spin_lock_slowpath:xa_store:...' },
    crash_type: { type: 'string', enum: ['oops_null_or_bad_ptr', 'page_fault_uaf', 'gpf_noncanonical', 'bug_on', 'warn_on', 'soft_lockup', 'hard_lockup_nmi', 'rcu_stall', 'hung_task', 'deadlock', 'oom', 'list_or_memory_corruption', 'mce_or_hw_error', 'other'] },
    kernel_versions_full: { type: 'array', items: { type: 'string' }, description: 'full uname strings of the CRASH kernel(s) quoted in the post, e.g. 6.16.1-0_fbk2_hardened_0_gf40efc324cc8' },
    investigated_host: { type: 'string' },
    crash_time_unix: { type: 'integer', description: 'unix seconds of the investigated crash if stated or derivable from the post (vmcore path timestamps count), else 0' },
    subsystem: { type: 'string' },
    oot_module: { type: 'string', description: 'out-of-tree module implicated (nvidia-open-kmod, nvidia-vgpu, mlx5 OFED, sched_ext BPF scheduler name, etc.) or "none"' },
    bot_root_cause_category: { type: 'string' },
    bot_confidence: { type: 'string' },
    hosts: { type: 'integer' },
    root_cause_summary: { type: 'string', description: 'the ORIGINATING defect the post claims, 1-2 sentences, name functions/structures' },
    fatal_mechanism: { type: 'string', description: 'what the backtrace shows / how it died, 1 sentence' },
    defect_family_label: { type: 'string', description: 'short reusable label for the defect family, e.g. "csdlock_debug synchronous printk feedback loop on slow serial console"' },
    mechanism_class: { type: 'string', enum: ['data_race_or_missing_sync', 'lock_ordering_deadlock', 'lock_hold_too_long_or_starvation', 'use_after_free_lifetime', 'null_deref_or_missing_check', 'refcount_or_leak', 'accounting_or_invariant_violation', 'livelock_or_feedback_loop', 'resource_exhaustion', 'config_or_tunable', 'hardware_fault', 'firmware_bug', 'oot_driver_bug', 'bpf_or_sched_ext', 'other'] },
    timing_dependent: { type: 'boolean', description: 'true if the bug is a race / timing / concurrency-window / scheduling-dependent defect' },
    timing_note: { type: 'string' },
    claimed_fixes: { type: 'array', items: FIX },
    diffs: { type: 'array', items: { type: 'object', properties: { id: { type: 'string' }, role: { type: 'string' } }, required: ['id', 'role'] } },
    human_signal: {
      type: 'object',
      properties: {
        kind: { type: 'string', enum: ['confirmed', 'refuted_or_wrong', 'fixed_by_named_commit', 'duplicate_of_known_issue', 'acknowledged_or_investigating', 'none'] },
        who: { type: 'string' },
        quote: { type: 'string', description: 'verbatim, <=300 chars' },
        fix_ref: { type: 'string', description: 'commit/branch/SEV/task a human named, or ""' },
      },
      required: ['kind', 'who', 'quote', 'fix_ref'],
    },
    reproducer: { type: 'object', properties: { attempted: { type: 'boolean' }, actually_ran: { type: 'boolean' }, ab_result: { type: 'string' } }, required: ['attempted', 'actually_ran', 'ab_result'] },
    answer_visible_in_input: {
      type: 'object',
      properties: {
        risk: { type: 'string', enum: ['none', 'low', 'high'] },
        why: { type: 'string', description: 'high if the raw crash input alone (dmesg/backtrace/category/panic string) names the fix, the culprit commit, or makes the cause obvious (e.g. MCE banner, explicit driver assert naming the bug, "Fixes:" text in log)' },
      },
      required: ['risk', 'why'],
    },
    notes: { type: 'string' },
  },
  required: ['post_id', 'mid', 'category_key', 'crash_type', 'kernel_versions_full', 'investigated_host', 'crash_time_unix', 'subsystem', 'oot_module', 'bot_root_cause_category', 'bot_confidence', 'hosts', 'root_cause_summary', 'fatal_mechanism', 'defect_family_label', 'mechanism_class', 'timing_dependent', 'timing_note', 'claimed_fixes', 'diffs', 'human_signal', 'reproducer', 'answer_visible_in_input', 'notes'],
}

const SCHEMA = { type: 'object', properties: { posts: { type: 'array', items: POST } }, required: ['posts'] }

const N = 28
const idx = Array.from({ length: N }, (_, i) => i)
phase('Extract')
const results = await pipeline(idx, (i) => {
  const f = `/home/newton/local/ccbench/data/batches/batch_${String(i).padStart(2, '0')}.md`
  return agent(
    `You are extracting structured facts from CrashCortex kernel-crash investigation posts for a benchmark dataset.\n` +
    `Read the file ${f} with the Read tool (read it fully; it holds up to 10 posts separated by lines starting with "======== POST post_id=...").\n` +
    `For EVERY post in the file, return one object in "posts". Extract ONLY what the post text and its comments state; do not browse, do not call other tools, do not guess. Use "" / 0 / [] when absent.\n` +
    `Rules:\n` +
    `- claimed_fixes: every commit the post names as a fix, backport, mitigation, introducing commit, or bot-proposed patch. Copy SHAs exactly. The bot's own kernel_fix.patch counts as kind=bot_proposed_patch (sha "" unless a hash is given).\n` +
    `- kernel_versions_full: only full uname strings of the crashing kernel (they usually end in _g<12hex>, maybe with -hotfixN).\n` +
    `- human_signal: judge ONLY comments by humans (author != CrashCortex). "fixed_by_named_commit" if a human names the real fix (quote it).\n` +
    `- answer_visible_in_input: judge whether the RAW crash input (dmesg/backtrace/panic string/category, as quoted in the post) would by itself reveal the answer. MCE/GHES hardware banners, an explicit assertion naming the defect, or a log line naming the fix => high.\n` +
    `- timing_dependent: true for races, concurrency windows, lock starvation that depends on scheduling/contention, feedback loops under load; false for deterministic logic bugs.\n` +
    `- defect_family_label: short and reusable so that different posts about the same underlying bug class get the same label.\n` +
    `Return all posts in file order.`,
    { label: `extract:batch_${i}`, phase: 'Extract', schema: SCHEMA, effort: 'medium' }
  )
})
const all = results.filter(Boolean).flatMap(r => r.posts)
log(`extracted ${all.length} post records from ${results.filter(Boolean).length}/${N} batches`)
return { n: all.length, posts: all }
