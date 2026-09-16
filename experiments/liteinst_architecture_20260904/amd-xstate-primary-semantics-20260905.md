# AMD XSAVE/XRSTOR in-use semantics: bounded primary-source check

Date: 2026-09-05. Status: primary-source evidence unavailable; AMD permission
question unresolved. This is not an ISA conformance verdict.

## Question and supplied observation

Can restoring a component whose register values equal architectural initial
values change its subsequently saved XSTATE_BV bit without changing those values?

The requester reports AMD EPYC native signal return changing only byte 512,
XSTATE_BV `0x2a7 -> 0x227`, with component 7 (Hi16_ZMM) payload zero both times;
the in-process Tool path reportedly has the same effect. These are supplied
observations, not independently executed or inspected here. The bitmap XOR is
`0x80`, i.e. bit 7 only. Zero saved payload alone is not an independent proof
that every live register value was preserved.

## Primary-source availability and provenance

- Local filename searches in the project/experiments, Documents, Downloads,
  cache, and a bounded /tmp search found no relevant AMD or Intel primary manual.
  A supplementary home-directory filename scan was stopped after roughly
  41 seconds without finding a relevant manual. Targeted text searches in
  project ai_docs, this experiment directory, and /usr/share/doc found no
  relevant primary text. This is not an exhaustive filesystem absence claim.
- AMD candidate: *AMD64 Architecture Programmer's Manual, Volume 2: System
  Programming*, publication 24593. Attempted vendor URL:
  https://www.amd.com/system/files/TechDocs/24593.pdf
  The configured access policy denied the vendor destination before document
  retrieval. The URL's current validity, actual edition, revision, pages, and
  sections were therefore not verified. No alternate route was attempted.
- Intel comparison candidate returned by public search: *Intel 64 and IA-32
  Architectures Software Developer's Manual Documentation Changes*:
  https://cdrdv2-public.intel.com/812380/252046-sdm-change-document.pdf
  The configured document reader refused external-page access. No alternate
  fetch was attempted. Revision, pages, sections, and relevant contents were
  not available. A search-result title is not evidence of instruction semantics.
- Consequently there are **no verified primary-manual quotations** to report.
  Neither Intel behavior nor an AMD rule is inferred from search snippets,
  secondary discussions, remembered wording, or the supplied experiment.

## What remains unknown specifically for AMD

1. The precise AMD rule relating current component values/internal in-use
   tracking to XSTATE_BV written by ordinary XSAVE (not an assumed rule for
   XSAVEOPT, XSAVEC, or XSAVES).
2. Whether AMD XRSTOR is expressly allowed to clear or recompute in-use tracking
   when the input component bit is set but the restored data equal architectural
   initial values; alternatively, whether it must retain that bit's information
   for a subsequent save. This is the decisive missing permission/requirement.
3. Any applicable qualifications for component 7 and the specific processor's
   supported extensions, save/restore instructions, and requested/enabled masks.

A source establishing that an unset input component bit restores initial values
would not, by itself, establish permission to change a *set* bit after restoring
an explicit all-initial payload. Likewise, a statement that a clear in-use bit
implies initial values need not establish the converse. Those logical directions
must not be conflated. Intel primary wording, even if later obtained, must be
labelled comparison and cannot substitute for an AMD architectural guarantee.

## Correctness versus repeat determinism

Whole-image before/after equality is a stronger property than equality of live
architectural register values: it additionally constrains the saved representation.
Without the missing AMD text, this check cannot establish that AMD either supplies
or disclaims the relevant bitmap round-trip guarantee. It is therefore premature
to answer the permission question with either an architectural “yes” or “no.”

The supplied native signal-return observation is useful empirical evidence that
the symptom is not unique to the Tool path. It is not primary specification text,
does not isolate XRSTOR from everything else in signal handling, and does not by
itself prove either path correct or repeat-deterministic.

If a program observes XSAVE bytes, those bytes are guest-observable even if the
restored register values are unchanged. An architectural allowance for multiple
representations, if established, would not imply that repeated runs actually vary,
nor would it excuse repeat-parity differences. Before/after restoration and
cross-run full-observation parity are different claims. This note authorizes no
masking of XSTATE_BV, no normalization of payloads, and no weakening of comparators.

Only this research note was written. No native guest/probe, runtime test, kernel
investigation, product-code edit, agent spawn, or GitHub request was performed.
To resolve the remaining question, provide an accessible local primary AMD manual
edition covering XSAVE/XRSTOR and AVX-512 state, or permitted vendor access; exact
revision/page/section quotations are still required.
