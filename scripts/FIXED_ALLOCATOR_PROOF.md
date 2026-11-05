# Fixed allocator proof for the Detcore validate profile

The finite instruction recipes in `m1_artifact.rs` bind complete functions in normal and allocator-fixture Detcore validate images. Only independently authenticated four-byte address operands vary. Every branch, register, span constant, field offset, status tag, length and remaining opcode is fixed. Qualification checks the complete transfer census and data bindings; this argument explains why the enumerated guarded terminal edges are unreachable under the stated inputs. Unknown compiler output remains refused.

All premises remain the frozen SysV x86_64 ABI/clearDF, valid GlobalAlloc Layout/live-pointer inputs, authentic native capture/exclusive INITIALIZING, fixed immutable backing and intact locked metadata. This is trusted-runtime placement/allocator proof; guest protection and TLS isolation are not established. No arbitrary panic, unwinder, source name or external allocator is admitted.

Compiler shims: alloc/zeroed preserve RDI=size/RSI=align until CMP size,(2^63−align); valid Layout implies size≤2^63−align, so JA to the panic arm is false. They move size to RDX and the exact image-bound ZST marker to RDI before their sole method tail. Dealloc saves oldsize to RCX, compares oldsize against2^63−oldalign(RDX), moves ptr RDI→RSI, then supplies the marker. Realloc first preserves newsize RCX→R8 and oldsize RSI→RCX, performs the same old-Layout check, then forwards ptr/oldalign/oldsize/newsize unchanged. No pow2 check is fabricated: valid input Layout is the explicit existing premise. The otherwise-retained panic_fmt transfer, complete259B text, tagged ESI519, and full24B Detcore Location29:1 are still physically bound and censused.

FixedPatch.alloc241: ready backing returns the immutable40B record. Cursor at record+32 is checked8-aligned/nonzero; its authenticated value is METADATA+24. The exact named TLS call receives the module descriptor and reads the authenticated LOCAL8 cell. A zero cell tail-calls FixedTool.alloc with unchangedalign/size; a positive cell selects the patch bump. Checked additions for base+cursor and alignment return NULL on overflow. Aligned user≥patchbase, so the only debug SUB-underflow edge at+201 is excluded by that bound. Adding requested size checks carry and≤32MiB before CMPXCHG on the exact cursor. No fallback malloc/copy exists. Retained normal/diagnostic operands0xa0/0xe0 and observed event-removal operands0x98/0xd8 each equal their actual authenticated cell; no offset is inferred from another profile. FnOnce22 returns modulebase+that same cell with exact stack restoration; both actual TLS calls reach the same authenticated existing GLIBC_2.3 PLT, not an imported allocator.

FixedPatch.dealloc232: saves original ptr/align/size; exact backing yields patchbase and cursor; fixedbase+32MiB cannot overflow. A pointer in the patch span returns without pretending bump storage reusable. Other pointers enter actual require-owned, then exact FixedTool.dealloc with original fields. Realloc414 keeps original pointer/old Layout/newsize; patch-owned addresses use checked current-span membership and checked new Layout, bump storage, then fixed_copy_bytes for min(oldsize,newsize); reusable addresses reach exact FixedTool.realloc. Fixed backing constants exclude the retained add/sub-overflow and null/alignment terminals. These edges remain in the transfer list and cannot be deleted or independently used as permission.

FixedTool.alloc116/dealloc97/realloc254 all first reject INITIALIZING, require published non-error REGION, and use literal toolspan metadata0x600000023000/base0x600010000000/capacity32MiB. The exact original refusal55 forwards exit_group231/status127 through raw_syscall6 and the existing trusted gate, followed by UD2s. It is a genuine refusal path, not a swallowed error. Realloc constructs the new Layout checks, obtains a new allocation or NULL, copies min(old,new), and frees the genuine old header through the same deallocator. No foreign libc allocation call occurs.

ToolHeapSpan.allocate660 preserves its exact locked free-list/bump/split implementation. Under intact metadata, free-node offsets are nonzero8-aligned within32MiB, and metadata/base are authentic, excluding null/misalignment calls+544/+565/+601/+614. Arithmetic that could exceed a request/available block returns NULL or skips the free node. In the free-list path, checked additions and end≥aligned-start maintain end≥base, excluding SUB-underflow+578 (guards+211/+423). The selected rounded end is≥the current node offset, excluding split SUB+636 (guard+265). Bump allocation starts at roundedcursor≤capacity; after checkedend≤capacity, end≥roundedcursor excludes SUB+636's alternate guard+467. Header/free-link updates stay8-aligned and in the locked span. Deallocate475 inserts by offset and coalesces only equal adjacent boundaries; all ten retained panic calls are null/alignment guards on authentic metadata/free-node/predecessor/successor accesses. Valid live header plus the intact sorted metadata invariant excludes those arms; no loss of the separated-live-block requirement. Allocation's final _Unwind_Resume+655 is reached only by a panic/EH path; there is no ordinary normal call entering it. With the actual normal helpers nonthrowing and all panic arms excluded, it is dead. Its edge/full extent remain retained; no general libunwind closure is accepted. Existing allocator semantic/COW/overflow/free-order tests are still required independently.

Publication/native preparation: from_native329 binds RSI to the exact closed-origin NativeRecord (constructor+0x30 or generic+0xa0), requires INITIALIZING, verifies actual RSP in the owned8MiB interior and gettid186>0 matching record+64, version1/body-nonzero/captured-RSP residue8, and REGION absent. Literal CMPXCHG changes phase1→2. It tests metadata readiness and constructs exactly[0x100,0x902) for bitmap validation before invoking exact setup. The only retained pointer panic is from the already-authenticated8-aligned record, not a tolerated wrong pointer. Setup247 checks both literal guard/interior ranges0xffff..0x12001 and0x13fff..0x16001, mprotects exactly the two32MiB interiors through scalar syscall, initializes only authentic aligned METADATA fields, marks the two ranges after success and retains exacterrno on failure. Bitmap239 uses only this literal bounded range and authentic metadata; its two pointer panics are excluded by the native input. Bitmaprange102 compares every index against0x100000; caller's0x100..0x902 excludes bounds panic. No arbitrary Range input gets that premise.

Publication.get13 returns NULL unless the exact ready byte at object+24 is set, otherwise returns the same object address. Publication.set59, called under the exclusive gate, checks that same REGION+24; if already ready it returns the unchanged input payload as failure, without overwriting published data. Otherwise it copies the24B value into REGION(+16 and first16B), sets ready+24=1, then writes the exact success tag0xfe to its result. No std OnceLock, allocation/futex/interposable helper, or wait closure exists. Each whole266B adopter passes origin1/2 and actual saved RSP to from_native; only a successful owner proceeds, stores preparation-RSP+88, publishes, tests success0xfe, writes phase3, clears INITIALIZING, then rechecks REGION. Failure returns1; success returns0. Pointer checks on original authenticated record and returned authentic owner are dead panic arms. The final cannot_unwind landing edge is EH-only after this complete nonthrowing closure; it remains recorded. Body dispatch is exclusively the unchanged native CALLR15 recipe, never a Rust indirect-call exception.

Scalar syscall23 has exactly PUSH8/MOV[rsp]=0/XOR R8D/XOR R9D/CALL local-GOT trusted_syscall/POP8/RET: original RDI number, RSI/RDX/RCX three arguments are unmodified; args4/5 and the scalar stack arg6 are zero, and stack restored. raw_syscall6 has the original exact39B six-argument marshaling. zero8 forwards lengthRSI→RCX, zeroes AL, sole REPSTOSB; copy15 forwards source RDI→RSI, destination RSI→RDI and lengthRDX→RCX, sole REPMOVSB. The clearDF and nonoverlapping valid copy ranges are the existing explicit premises. No callee-saved register is changed by either REP helper.

Literal guarded terminal pairs are conditional on these complete recipes and source/input invariants. Supporting canonical symbols are finite catalog entries with actual private whole scopes, not semantic evidence by their names. Physically derived data addresses need independent canonical LOAD/section/RELRO joins and sole-loader-writer evidence; a Location pointer has exactly one local RELATIVE writer and its full externally expected filename has none. Independently resolved source context supplies the complete expected filename; it cannot alter the frozen line/column or any instruction/branch. Unknown byte, profile shape, opcode, transfer or data layout remains refusal.

Standalone Hermit source copies explicitly use declared-content authority rather than unavailable Git provenance. Fresh locked/offline Cargo must bind the independently supplied full revision, both source revision positions, exact package IDs, manifests and target types. Thirteen declared Reverie files must match reviewed literal SHA256 and length expectations; the Cargo-resolved host qualifier additionally matches its separately compiled source digest. The six nested Hermit build inputs are current-content snapshots. Sorted relative filenames, lengths and validated digests form location-independent inventories, rechecked at every use; these do not authenticate undeclared files or invent Git heads/trees. Canonical package identities remain distinct from exact retained compiler filenames. Only the observed Cargo-home root mapping and the host qualifier's specific declared ../scripts target spelling are supported; inner aliases, retargeted roots, changed files and remapped compiler prefixes are refused. The original Git-backed factory and every whole-function, transfer and data-binding requirement remain in force.

Mapping diagnostic helper family: the five exact complete templates form one authenticated cohort, not an independent union. The original cohort is constructor266/generic266/set59/dealloc97/realloc254; the observed mapping diagnostic cohort is constructor277/generic277/set49/dealloc85/realloc242. After all whole-body, typed/read-only/GOT operand checks and the complete actual transfer census, the real operation closure requires exactly one full cohort. Every qualified incoming setter edge must be the sole direct constructor/generic call at+114 (original) or+125 (mapping diagnostic) to the same independently selected setter. No receipt label or extent classifies a family. Original callers supply RSI=the24B payload and RDI=result storage; the59B setter binds actual REGION/base+16/ready+24 through its exact global operands. New callers bind RSI to that same authenticated REGION through the literal LEA+108, supply RDX=payload and RDI=result, and the49B setter consumes that receiver without an intervening register overwrite. Both preserve24B payload, ready+24, error0xff/success0xfe and the exact old panic/owner/gate/postpublication proof. Get13, alloc116, ownership58 and from_native329 retain their original complete recipes. Shorter dealloc85/realloc242 retain the same actual REGION/get/null/error rejection, live pointer/layout/data paths and complete allocator/copy/refusal joins; the literal shift25/equality span check proves the same32MiB interval as the original shift28/lower-and-upper pair. All30 nonuniform five-role combinations must pass individual whole recipe/census authority and then be refused by the cohort join. Synthetic controls do not establish physical ELF qualification.

Standalone event-pointer TLS correction: the DEV177B alloc retains its complete immutable Some-LocalKey/FnOnce/get/with/try/getter/callback/read chain and actual38B getter at retained0x48 or observed0x40. New DEV0x40 requires the complete typed/hash chain and exact direct LocalKey transfer; stripping or relabeling it cannot make an optimized record. Optimized186B alloc and22B FnOnce retain literal cell displacements at retained0x48/0x88 or observed0x40/0x80, with the complete None-LocalKey chain. The four new optimized accesses use the existing whole-authenticated TLS-chain/transfer-census route; PUBLICATION_WRAPPER_RECIPES remains unchanged. Detcore241B alloc/FnOnce22 likewise adds only observed0x98/0xd8 under the full None chain, preserving0xa0/0xe0. Every actual encoded displacement is checked against the independently authenticated LOCAL8 source identity, module descriptor, exact named TLS provider and physical loader/source joins. Structural shape discriminators do not qualify machine code. Original host mutations/recipes, all-profile physical qualification and original runtime placement/allocator/lifecycle suites remain required.

## Constant-specialized publication and outlined Tool heap

The native `exec-native-injection` images from the capability-descriptor and
signal-delivery changes emit the same ten complete function shapes after only
the enumerated address operands are normalized. Their source allocator and
region implementations are unchanged. Local ThinLTO places the publication
getter with its callers, specializes the receiver to `REGION`, and outlines
`fixed_tool_heap`. This is a different machine-code composition, not permission
to omit publication, ownership, or transfer checks. The existing profile is
unchanged. Setting codegen-units to one would change optimization across the
whole runtime, would not establish any of these invariants, and would require
requalifying the other generated code as well.

The admitted composition is getter23, heap79, backing57, require-owned75,
alloc85, dealloc85, realloc222, from-native315 and constructor/generic255,
with the already qualified global setter59 and three-band setup291. Whole
bodies, every internal branch, every non-address operand and every call/tail
remain literal. Each direct/GOT target is bound independently to its selected
role, and every actual transfer is censused. The numeric `.llvm` suffix is only
a symbol-identity normalization; it cannot qualify a body. The family join
requires these actual authenticated operations together. A receiver-consuming
getter cannot be substituted where the caller omits the receiver, nor may an
unqualified heap-return ABI be used merely because its function is named
`fixed_tool_heap`. Existing admitted families and their controls remain.

Getter23 has no argument: its first RIP-relative operand addresses the exact
writable, initially unpublished, 32-byte `REGION` object, and its second
addresses that same object's ready byte at +24. It reads that byte, returns
NULL when clear, and otherwise returns the exact object address. The acquire
load is the existing x86-64 source/machine memory-order premise. There is no
call, allocation, wait, TLS access or write. Backing57 still rejects
`INITIALIZING`, requires a non-NULL getter result, and rejects the error tag
0xff at result+16 before returning the independently authenticated immutable
backing object. Only the now-unnecessary argument load is absent.

Heap79 takes a caller-owned 24-byte result slot. It writes NULL to the first
word if initialization is in progress, publication is absent, or the published
result is an error. Otherwise its three words are exactly metadata
0x600000023000, base 0x600010000000 and length 0x02000000. The latter base is
formed by adding the literal 0x0ffdd000 to the literal metadata address; there
is no data-dependent address selection. All four consumers use their own
stack slots and test the first word before reading the other two words.
Alloc85 preserves the original alignment/size, copies the complete span to its
allocation-call slot, and calls the already qualified allocate operation.
Require-owned75 and dealloc85 test base <= pointer < base+length; foreign or
unpublished inputs still reach the exact refusal. Dealloc85 reads the live
header at pointer-8 and supplies the actual metadata and base to the qualified
deallocator. Realloc222 retains the old/new Layout checks, NULL-on-failure,
copy of min(old size,new size), and deallocation of the genuine old header.
None of these paths gains a guest-heap or libc fallback.

The three outlined consumers retain checked-add overflow panic edges for
base+length. Those edges are not removed: the complete authenticated helper
returns the two literal bounds above, whose sum cannot carry. This premise is
limited to those exact consumers joined to that helper, under the original
intact-metadata/live-pointer contract. Their full Location data, panic target,
and transfer remain checked; no general panic edge is admitted.

Constructor/generic255 retain origin 1/2, the real saved stack pointer, the
complete native-owner admission, and the same 24-byte publication payload.
At their sole direct setter call +110, RDI is result storage and RSI is that
payload. Setter59 binds its own global `REGION`, preserves the payload on
already-published failure, and otherwise writes the value, release-publishes
ready+24, and returns success tag 0xfe. On success each adopter writes phase 3,
clears `INITIALIZING`, calls the specialized getter and checks its result and
error tag again. Every incoming setter edge must still be the exact
constructor/generic edge; a same-shaped foreign caller does not acquire this
proof. The aligned-record/owner pointer panic and EH-only cannot-unwind edges
retain their original premises and complete censuses.

From-native315 preserves the closed-origin record, gate, owned-stack, gettid,
version/body/stack-residue, unpublished-region, phase 1->2 and bitmap checks.
Its direct setup call is at +274. Like the previously admitted full-result
caller330, it compares the full EAX with 1 and takes EDX only on error; success
stores the authenticated owner. Therefore it must join the actual setup291
result ABI: every error returns EAX=1 with EDX=errno, and success EAX=0. The
existing three-band bitmap and raw-syscall closure is unchanged. The different
call offset is not a waiver of that join.

Retained machine samples and unit mutation controls establish the recipe and
its refusal boundaries; they do not establish source authority or execution.
The landing evidence must separately run the physical qualifier with fresh
locked source bindings and the real native producer. Unknown machine shapes,
changed source catalogs, altered receiver/data bindings, missing roles, mixed
ABIs and changed branch/constant bytes continue to refuse.

## Source-owned standalone dev scalar composition

The maintained standalone producer and its independent receipt consumer require
`dev` with opt0, debug2, debug assertions and overflow checks. The successful
Cargo artifact selection and subsequent receipt re-selection bind that exact
profile and feature set. An optimized profile is not a substitute. Machine
qualification still depends on full bytes, current compiled-source authority,
actual image kind, private symbols, loader mappings and complete transfer/data
censuses; compiler and profile names alone do not admit any machine body.

The separate dev composition has fourteen complete parents: mprotect55,
backing312, setup250, heap-metadata607, getter234, setter353, require-owned254,
alloc294, dealloc59, realloc255, gettid39, constructor1411, generic1411 and
owner301. Only setup250 and owner301 are byte/operand-identical to an existing
composition. The ordinary complete DEV51B/template106 transport and separately
qualified current bitmap520 and owned-heap267 remain required. Every other old optimized/native tuple and its
indices remains distinct. Any one dev body in an older tuple, a missing dev
member, native transport, or another image kind must refuse.

Mprotect55 establishes SYS10, the incoming address, literal32MiB and prot3 at
its sole direct raw call+27. It preserves the full64-bit result, restores its
24-byte frame and returns; the cannot-unwind landing+39 remains recorded.
Gettid39 supplies SYS186 and three zeros at direct raw+16, restores its frame
and returns the full result. It retains its own cannot-unwind landing. Owner301
checks origin, genuine record, owned stack, positive matching gettid, gate,
phase and bitmap, then calls setup250+257 and checks full EAX before publishing
its output. Setup250 retains both mprotect calls, metadata initialization and
all six literal bitmap updates with full errno propagation.

Getter234 accepts only the caller's aligned live publication slot. Its complete
null/alignment guards and literal acquire-order load remain. It returns null
when unpublished, otherwise that same slot address, through the exact const
pointer identity leaf. Setter353 checks the same slot before reading payload.
An already-published slot returns literal EAGAIN through the exact i32 identity
without reading or moving payload. Otherwise it copies all24 bytes, releases
publication at+24, and returns full zero through the same i32 leaf. The full
bodies retain their null/alignment and EH landing edges.

Backing312 and heap-metadata607 reject INITIALIZING, absent publication and a
published error before returning the immutable backing object or exact fixed
metadata. Their complete typed Option/Result operations are recursively
qualified. Require-owned254 and allocation294 additionally transport a
nullable metadata pointer through exact typed `as_ref` and Option branch
leaves. The allocator, live-header, foreign-pointer refusal, checked new Layout,
copy of min(old,new), and deallocation paths remain whole-qualified under the
original valid GlobalAlloc/live-pointer/intact locked metadata contract.

Each complete1411-byte entry retains origin1/2, saved stack and native owner
admission. It checks the returned status before reading the authenticated
owner, moves the entire24-byte payload through the same scalar setter and
checks its full Result, stores phase READY, releases INITIALIZING, then observes
the same publication independently. Its complete source Locations include the
unaltered803:14/820:14,785:18 and separate775:9 payload expression. Every full
24-byte Location and complete filename is independently source-bound, physically
read-only or sealed RELRO, with one exact RELATIVE pointer writer and no filename
writers. No line tolerance or ELF-derived expected filename is accepted.

The finite ten-helper composition includes four already qualified prepare
leaves and six additional exact typed bodies. The21-byte identity leaves return
RDI in RAX with bounded16-byte red-zone writes. The18-byte i32 leaf returns EDI
in EAX with eight red-zone bytes. Both43-byte pointer leaves return null for null
and otherwise the original pointer; the65-byte Option branch likewise preserves
the nullable pointer. These use at most24 red-zone bytes and no external call.
The78-byte metadata Option map tests null, returns null on that arm, or calls
one exact13-byte closure+47. That closure returns the independently authenticated
immutable backing object through its bound RIP operand. Its caller's40-byte
frame and complete branches remain literal.

These leaf bytes prove scalar transport, not validity of Rust references.
Audited caller source supplies live, aligned, correctly typed publication and
metadata objects with process lifetime and exclusive initialization ownership.
SysV stack validity covers the bounded red-zone writes; only preparation paths
also inherit the existing source-bound owned8MiB stack argument. There is no
new universal stack or pointer-validity claim.

The three dev getter/setter/heap-metadata EH landing edges acquire guarded
terminal treatment only from their exact private new whole-body witnesses and
the mandatory complete dev join. Their literal valid atomic orders, live
aligned references and finite already qualified callees have no normal unwind
path under the existing no-foreign-unwind premise. The edges remain in the
physical census. No old terminal table is changed and no arbitrary panic or
unwinder is admitted. A changed instruction, helper identity, operand, readonly
object, writer, incoming callsite or mixed composition continues to refuse.

All ten helpers and all protected parent incoming multisets are exact: actual
CALL PCs, direct/GOT modes, shared target addresses and complete private body
witnesses must agree. The previous prepare-entry definition/alternate-entry
proofs are retained independently. Pure machine mutation controls cover every
byte, full extent, typed target, GOT slot, Location/filename, census edge and
coherent-family boundary. Those controls do not establish physical ELF/source
qualification, a producer success, or guest integration; each remains a separate
required actual validation result.

The current standalone dev heap source has a separate complete profile of
32 measured `alloc.rs` Locations across allocation, deallocation, layout-end,
alignment and span containment. Each profile member fixes the entire24-byte
record and filename, keeps the sole exact RELATIVE writer, and has a distinct
`fixed_dev_scalar_alloc_<line>_<column>` evidence key. Historical Location
specs, heap recipes and their exact coordinate checks remain unchanged. There
is no permitted line delta. Only the complete fourteen-parent/ten-helper dev
shape, exact ordinary DEV51B transport, current bitmap520/owned-heap267 operations
and a fresh standalone workspace source factory can request these records. Every one of the32 current records and its filename
must join the final complete dev composition. Old operand request names are
internal physical aliases; they do not label current serialized evidence.

The six additional dev helpers also require one exact canonical raw definition
and refuse a differently named static or dynamic function/storage entry or
extent alias, including zero-sized aliases at their entry or interior. The
four existing prepare helpers retain their independent private identity
factory. Unrelated symbols outside these six extents do not acquire any new
role or permission.

The emitted dev EH classification has a matching final receipt-shape check.
Both producer and consumer take its source layout from freshly verified source
expectations, never from receipt labels. It requires all27 current functions
(fourteen parents, ten helpers, ordinary DEV51B transport and the current
bitmap520/owned-heap267 operations), all32 current alloc Location/file/writer records, and exactly the three retained EH CALL
PCs470/21/34 with their shared local GOT slot and selected terminal target.
Historical terminal tuples remain unchanged. Missing, old, mixed, native or
source-less shapes cannot use this classification. This structural check does
not recreate authenticated machine witnesses from serialized hashes: consumer
source equality and independent full physical ELF qualification/evidence
comparison remain mandatory. A real emitter-to-final-validator roundtrip
control covers the complete structural shape and mutations; its synthetic
layout and hashes are explicitly not source authority or runtime proof.


The ordinary DEV transport has its own complete51-byte template106. Historical
optimized raw26 remains23 bytes and is not a DEV positive. The independently
measured DEV body preserves number/arg0/arg1/arg2, zeroes arg3/arg4/arg5, uses
SUB40/ADD40 and one local-GOT CALL at+40, then returns the gate result in RAX.
Its sole displacement42 is resolved from nextPC46 to the independently selected
private44-byte trusted syscall gate. This is the same full scalar ABI already
proved by the unchanged `fixed_scalar_helper_recipe`; no new arbitrary syscall
or indirect-call classification is added. It has no REGION/INITIALIZING storage
operand. Both private factory variants retain that distinction.

The current DEV preselector, authenticated mode, full transfer census and
serialized evidence shape all require this exact51-byte transport and CALL40.
The two incoming raw callers remain mprotect+27 and gettid+16. The existing
whole-body gate/RX/no-writer/local-symbol and sole RELATIVE/RELRO target checks
still apply. A complete DEV family with optimized raw23, an old optimized or
native family with only the new raw51 identity, a shifted CALL, different GOT
callee, missing raw/gate transfer record or changed full extent must refuse.
The DEV positive fixtures use the independently retained actual51-byte body
and instruction boundaries, together with the complete current bitmap520 and
owned-heap267 bodies described below. Historical175-byte bitmap fixtures remain
separate and supply no current-family positive. A real emitter/final-validator control
also joins full51/44-byte raw/gate identities and the actual single CALL40.
No expected source Location, old template, historical terminal table, test
assertion, comparator or old transport recipe is changed by this correction.


## Complete current standalone DEV operation closure

The complete source-aware DEV selector now joins27 whole bodies: fourteen
parents, ten typed helpers, ordinary51-byte syscall transport, bitmap520 and
owned-heap267. The two current operations use a separate private recipe table
and opaque witness kind. Historical global template indices0..106, wrapper127,
bitmap175/495, optimized/native families and terminal tables remain unchanged.
A selected but unreached historical wrapper/bitmap definition keeps its old
factory; only the complete Standalone/ReverieWorkspace source-and-shape context
can select the new reached operations. Neither a name, isolated current body,
receipt label nor compiler/profile name supplies that context.

Bitmap520 has the exact measured248-byte balanced frame, Control/start/end/mark
ABI, null/alignment guards, Control+8 word base, bounded page loop, word-index
shift6 and bit-mask63. Its six setup250 callers remain exactly+16/+44/+72/+184/
+207/+230. GOT CALLs+98/+135 retain misaligned/null physical guards; direct
CALL+316 reaches the unchanged whole69-byte shift precondition. Its separate
complete24-byte143:27 Location has an exact46-byte tool_region_arena.rs filename,
sole local RELATIVE writer and full physical readonly/RELRO proof. Historical
128:27 is not an alias for this record. The unchanged1319:5 shift Location and
full369-byte panic text remain independently bound. The shift guard retains its
GOT panic CALL+58 and original premise. Source-safe fresh exclusive Control
initialization and the setup bands remain necessary; bytes do not prove an
arbitrary pointer safe.

Owned-heap267 preserves a balanced120-byte frame and the Rust sret ABI: incoming
RDI is the destination and RSI is the live allocation address. Metadata+24,
nullable metadata pointer+42 and typed Option branch+51 construct exactly the
three-word metadata/base0x600010000000/length0x2000000 span, or the existing None
residual+95. It refuses None, copies all24 result bytes, calls containment+236
with the original live address, refuses false+247/+252, then returns the original
sret pointer. Exactly dealloc+34 and realloc+47 call this operation. The original
GlobalAlloc/live-pointer/layout and locked metadata source premises remain.
No unqualified caller exemption is added: the three metadata/helper incoming
rows include only these measured new caller PCs, and the two new protected rows
retain complete opaque caller witnesses. Safe historical outside roots retain
their old whole-qualified source arguments without an empty-incoming claim.

Both final evidence consumers require every current function's exact private
identity and extent, all ten operation CALLs and typed direct/GOT targets, the
new complete data/file/writer identities, the unchanged three dev EH landings
and the three retained bitmap/shift terminal edges. The27 current code extents,
unchanged69-byte shift guard and44-byte private gate are pairwise disjoint.
The real physical emitter and real final validator/Serde controls use the full
current family. Byte, typed target, GOT, census, alias, source/data, old-family
substitution, missing/shifted/extra caller and overlap controls retain intended
refusal diagnostics. Synthetic structural layouts are not source authority or
producer/runtime evidence. Consumer source equality and complete independent
physical requalification remain mandatory after structural validation.

Identity checks have distinct scopes. The six additional typed DEV helpers
require static/dynamic entry/interior/extent closure even if selected but not
reached. The two new current operations use that same raw identity check only
when reached through the complete current context. The four older prepare
helpers retain their existing unique-role plus dynamic-name/same-entry checks;
there is no new global static-alias constraint or uniform alias-closure claim.

The contextual CALL RAX proof accepts the entire authenticated private prepare
helper set already allowed for FF15, including those complete helpers and the
exact scalar leaves. It does not accept arbitrary callees. Explicit XBEGIN and
prefixed-flow target classification applies to contextual prepare functions
containing CALL RAX; other preparers retain their older denylist. This section
clarifies the current proof and does not rewrite historical commit claims.

## Current DEV setter in the owned setup census

The allocator's later owned setup census must retain the same complete current
setter proof as its operation closure. The full353-byte setter has seven
physical calls. Its CALLs+180/+315 use the already authenticated whole18-byte
`fixed_dev_i32_identity`: load+168/MOV EDI,11+175/CALL+180, and
load+306/XOR EDI,EDI+313/CALL+315. Both loads have exact seven-byte RIP-relative
GOT form; each LOAD-PC+7 displacement, sole local writer and typed helper target
remain joined. The two calls must resolve through the same aligned private GOT
slot; a second valid slot to the same helper does not match this current case.
Entries after either defining load through its CALL refuse.
An entry at the defining load is allowed. The exact scalar helper saves and
reloads EDI/EAX in the SysV red zone, without another call or RSP adjustment;
it supplies no new reference-validity or memory-isolation claim.

Only the completed current27-body Standalone/ReverieWorkspace source family
can retain this private setter/helper witness. The later census rejoins the
same full function identity/hash, both precise argument windows, all seven
transfers and five CALL-based GOT edges. The generic constructor helper set,
register-call vocabulary and existing native setup extension are unchanged.
In particular, the two current calls do not add a globally allowed i32 callee
or XOR argument form. Current serialized evidence joins its owned setup vector
and GOT projection to the same allocator transfer vector, including source
attribution or instruction-range ownership. Those structural records cannot
construct the private physical witness; consumers still require fresh source
equality and complete independent physical requalification.
