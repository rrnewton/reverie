/* Enumerate every leaf in Hermit's deterministic CPUID tables at several
 * subleaves, so two backends' output can be diffed line for line. */
#include <cpuid.h>
#include <stdint.h>
#include <stdio.h>

int main(void) {
  static const uint32_t subleaves[] = {0, 1, 2, 3, 17, 18, 19, 0xFFFFFFFFu};
  static const uint32_t extra_leaves[] = {0x15, 0x1F, 0x80000000u + 11};

  for (uint32_t leaf = 0; leaf <= 0x14; ++leaf)
    for (unsigned s = 0; s < sizeof(subleaves) / sizeof(subleaves[0]); ++s) {
      uint32_t a, b, c, d;
      __cpuid_count(leaf, subleaves[s], a, b, c, d);
      printf("%08x %08x  %08x %08x %08x %08x\n", leaf, subleaves[s], a, b, c, d);
    }
  for (uint32_t off = 0; off <= 0x0A; ++off)
    for (unsigned s = 0; s < sizeof(subleaves) / sizeof(subleaves[0]); ++s) {
      uint32_t a, b, c, d, leaf = 0x80000000u + off;
      __cpuid_count(leaf, subleaves[s], a, b, c, d);
      printf("%08x %08x  %08x %08x %08x %08x\n", leaf, subleaves[s], a, b, c, d);
    }
  for (unsigned i = 0; i < sizeof(extra_leaves) / sizeof(extra_leaves[0]); ++i)
    for (unsigned s = 0; s < sizeof(subleaves) / sizeof(subleaves[0]); ++s) {
      uint32_t a, b, c, d;
      __cpuid_count(extra_leaves[i], subleaves[s], a, b, c, d);
      printf("%08x %08x  %08x %08x %08x %08x\n", extra_leaves[i], subleaves[s], a, b, c, d);
    }
  return 0;
}
