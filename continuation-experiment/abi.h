/* Standalone controlled native experiment; no product integration. */
#ifndef CONTINUATION_EXPERIMENT_ABI_H
#define CONTINUATION_EXPERIMENT_ABI_H
#define XSAVE_CAPACITY 65536
#define F_HOST_SP 0
#define F_HOST_BX 1
#define F_HOST_BP 2
#define F_HOST_R12 3
#define F_HOST_R13 4
#define F_HOST_R14 5
#define F_HOST_R15 6
#define F_XCR0 7
#define F_NR 8
#define F_ARGS 9
#define F_GUEST_SP 15
#define F_OBSERVER_SP 16
#define F_GUEST_PKRU 17
#define F_HOST_PKRU 18
#define F_AVX 19
#define F_AVX512 20
#define F_REGS 24
#define F_OBSERVED_PKRU 43
#define F_GUEST_REACHED 44
#define F_MXCSR 46
#define F_X87CW 47
#define SEED_OFFSET 448
#define XSAVE_OFFSET 512
#define OFF(n) (8*(n))
#endif
