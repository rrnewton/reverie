#define _GNU_SOURCE
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

__asm__(".section .liteinst_footprint,\"ax\",@progbits\n"
        ".p2align 12\n"
        ".global reverie_liteinst_footprint_getpid\n"
        ".type reverie_liteinst_footprint_getpid,@function\n"
        "reverie_liteinst_footprint_getpid:\n"
        "mov $39, %eax\n"
        ".global reverie_liteinst_footprint_site\n"
        "reverie_liteinst_footprint_site:\n"
        "syscall\n"
        ".rept 24\n"
        "nop\n"
        ".endr\n"
        "ret\n"
        ".size reverie_liteinst_footprint_getpid, .-reverie_liteinst_footprint_getpid\n"
        ".text\n");

extern long reverie_liteinst_footprint_getpid(void);
extern unsigned char reverie_liteinst_footprint_site;

static uintptr_t trampoline_address(void) {
  const unsigned char *site = &reverie_liteinst_footprint_site;
  if (site[0] != 0xe9) {
    return 0;
  }
  int32_t displacement = 0;
  memcpy(&displacement, site + 1, sizeof(displacement));
  return (uintptr_t)(site + 5) + (intptr_t)displacement;
}

static uintptr_t writable_alias(uintptr_t trampoline) {
  FILE *maps = fopen("/proc/self/maps", "r");
  if (maps == NULL) {
    return 0;
  }
  char line[512];
  char executable_device[32] = {0};
  unsigned long executable_inode = 0;
  while (fgets(line, sizeof(line), maps) != NULL) {
    unsigned long start = 0, end = 0, offset = 0, inode = 0;
    char permissions[8] = {0}, device[32] = {0};
    if (sscanf(line, "%lx-%lx %7s %lx %31s %lu", &start, &end,
               permissions, &offset, device, &inode) == 6 &&
        start <= trampoline && trampoline < end && permissions[2] == 'x') {
      memcpy(executable_device, device, sizeof(executable_device));
      executable_inode = inode;
      break;
    }
  }
  rewind(maps);
  uintptr_t result = 0;
  while (fgets(line, sizeof(line), maps) != NULL) {
    unsigned long start = 0, end = 0, offset = 0, inode = 0;
    char permissions[8] = {0}, device[32] = {0};
    if (sscanf(line, "%lx-%lx %7s %lx %31s %lu", &start, &end,
               permissions, &offset, device, &inode) == 6 &&
        executable_inode != 0 && inode == executable_inode &&
        strcmp(device, executable_device) == 0 && permissions[1] == 'w' &&
        permissions[2] != 'x') {
      result = (uintptr_t)start;
      break;
    }
  }
  fclose(maps);
  return result;
}

int main(int argc, char **argv) {
  if (argc != 2) {
    return 9;
  }
  long expected = reverie_liteinst_footprint_getpid();
  if (reverie_liteinst_footprint_getpid() != expected) {
    return 10;
  }
  size_t page = (size_t)sysconf(_SC_PAGESIZE);
  uintptr_t site = (uintptr_t)&reverie_liteinst_footprint_site;
  uintptr_t site_page = site & ~(page - 1);

  if (strcmp(argv[1], "noop") == 0) {
    if (mprotect((void *)site_page, page, PROT_READ | PROT_EXEC) != 0 ||
        reverie_liteinst_footprint_getpid() != expected) {
      return 11;
    }
    puts("active no-op protection preserved");
    return 0;
  }
  if (strcmp(argv[1], "site") == 0) {
    return mprotect((void *)site_page, page, PROT_NONE) == 0 ? 12 : 13;
  }

  uintptr_t trampoline = trampoline_address();
  if (trampoline == 0) {
    return 14;
  }
  if (strcmp(argv[1], "trampoline") == 0) {
    uintptr_t trampoline_page = trampoline & ~(page - 1);
    return mprotect((void *)trampoline_page, page, PROT_NONE) == 0 ? 15 : 16;
  }
  if (strcmp(argv[1], "arena-rw") == 0) {
    uintptr_t writable = writable_alias(trampoline);
    if (writable == 0) {
      return 17;
    }
    return mprotect((void *)writable, page, PROT_READ) == 0 ? 18 : 19;
  }
  return 20;
}
