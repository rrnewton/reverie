#include <cpuid.h>
#include <stdint.h>
#include <stdio.h>
int main(void) {
  static const uint32_t leaves[] = {0x04, 0x07, 0x0b, 0x0d};
  for (unsigned i = 0; i < sizeof(leaves)/sizeof(leaves[0]); ++i) {
    for (uint32_t sub = 0; sub <= 1; ++sub) {
      uint32_t a,b,c,d;
      __cpuid_count(leaves[i], sub, a, b, c, d);
      printf("leaf=%02x sub=%u eax=%08x ebx=%08x ecx=%08x edx=%08x\n",
             leaves[i], sub, a, b, c, d);
    }
  }
  return 0;
}
