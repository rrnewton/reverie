#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>

#if !defined(__x86_64__)
#error "the valid-absent vDSO control requires x86-64"
#endif
#if SYS_getrandom != 318
#error "the valid-absent vDSO control binds x86-64 syscall 318"
#endif

_Static_assert(sizeof(uintptr_t) == sizeof(uint64_t),
               "the valid-absent vDSO control requires 64-bit pointers");

enum {
  MAX_AUXV_ENTRIES = 128,
  MAX_ENV_POINTERS = 4096,
  MAX_MAPS_BYTES = 32768,
  MAX_VDSO_BYTES = 65536,
  MAX_PROGRAM_HEADERS = 64,
  MAX_SECTION_HEADERS = 128,
  MAX_DYNAMIC_SYMBOLS = 1024,
  MAX_HASH_BUCKETS = 1024,
  MAX_GNU_BLOOM_WORDS = 64,
  MAX_PATH_BYTES = 4096,
  MAX_RELEASE_POLLS = 50000000,
};

static const char canonical_name[] = "__vdso_getrandom";
static const char alias_name[] = "getrandom";
static const char renamed_canonical_name[] = "__vdso_absentaca";
static const char renamed_alias_name[] = "absentaie";
static const char vdso_permissions[] = "r-xp";
static const char vdso_path[] = "[vdso]";
static const char retained_armed_line[] = "retained-vdso-getrandom-armed\n";
static const char retained_success_line[] = "retained-vdso-getrandom-executed\n";
static const unsigned char child_identity_magic[16] = "LI-EXE-ID-V1";
static const unsigned char child_release_bytes[] =
    "release-retained-executable\n";
static const char proc_self_exe_path[] = "/proc/self/exe";
static const uint64_t ready_magic = UINT64_C(0x7664736f61627331);
static const uint64_t retained_entry_armed_magic = UINT64_C(0x72657461696e6564);

_Static_assert(sizeof(renamed_canonical_name) == sizeof(canonical_name),
               "the canonical replacement must preserve dynstr geometry");
_Static_assert(sizeof(renamed_alias_name) == sizeof(alias_name),
               "the weak replacement must preserve dynstr geometry");

static char maps_bytes[MAX_MAPS_BYTES];
static unsigned char post_rename_snapshot[MAX_VDSO_BYTES];
static uintptr_t saved_vdso_base;
static size_t saved_vdso_len;
static size_t saved_page_size;
static size_t saved_canonical_name_offset;
static size_t saved_alias_name_offset;
static size_t saved_canonical_symbol_index;
static size_t saved_alias_symbol_index;
static uintptr_t saved_retained_entry;
static char retained_armed_path[MAX_PATH_BYTES];
static char retained_success_path[MAX_PATH_BYTES];
static uint64_t saved_retained_entry_state;
static uint64_t saved_state;

struct vdso_map {
  uintptr_t start;
  uintptr_t end;
  size_t len;
  uintptr_t file_offset;
  uintptr_t device_major;
  uintptr_t device_minor;
  uint64_t inode;
  char permissions[sizeof(vdso_permissions)];
  char pathname[sizeof(vdso_path)];
};

static struct vdso_map saved_vdso_mapping;

struct dynamic_view {
  size_t string_offset;
  size_t string_size;
  size_t symbol_offset;
  size_t symbol_count;
  size_t section_offset;
  size_t section_count;
  size_t sysv_buckets_offset;
  size_t sysv_chains_offset;
  size_t sysv_bucket_count;
  size_t gnu_bloom_offset;
  size_t gnu_buckets_offset;
  size_t gnu_chains_offset;
  size_t gnu_bucket_count;
  size_t gnu_symbol_offset;
  size_t gnu_bloom_count;
  uint32_t gnu_bloom_shift;
  Elf64_Phdr load;
};

struct symbol_state {
  size_t canonical_offset;
  size_t alias_offset;
  size_t canonical_index;
  size_t alias_index;
  Elf64_Sym canonical;
  Elf64_Sym alias;
  size_t canonical_count;
  size_t alias_count;
  size_t renamed_canonical_offset;
  size_t renamed_alias_offset;
  size_t renamed_canonical_index;
  size_t renamed_alias_index;
  Elf64_Sym renamed_canonical;
  Elf64_Sym renamed_alias;
  size_t renamed_canonical_count;
  size_t renamed_alias_count;
};

struct rename_plan {
  size_t canonical_chain_offset;
  size_t alias_chain_offset;
  size_t bloom_offset;
  size_t bloom_size;
};

static long raw_syscall6(long number, long arg1, long arg2, long arg3,
                         long arg4, long arg5, long arg6) {
  register long r10 __asm__("r10") = arg4;
  register long r8 __asm__("r8") = arg5;
  register long r9 __asm__("r9") = arg6;
  long result;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "a"(number), "D"(arg1), "S"(arg2), "d"(arg3),
                     "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return result;
}

__attribute__((noreturn)) static void fail_preinit(int status) {
  (void)raw_syscall6(SYS_exit_group, status, 0, 0, 0, 0, 0);
  __builtin_unreachable();
}

static int bytes_equal(const unsigned char *left,
                       const unsigned char *right, size_t len) {
  for (size_t index = 0; index < len; ++index) {
    if (left[index] != right[index]) {
      return 0;
    }
  }
  return 1;
}

static void copy_bytes(unsigned char *destination,
                       const unsigned char *source, size_t len) {
  for (size_t index = 0; index < len; ++index) {
    destination[index] = source[index];
  }
}

static void copy_to_volatile(volatile unsigned char *destination,
                             const unsigned char *source, size_t len) {
  for (size_t index = 0; index < len; ++index) {
    destination[index] = source[index];
  }
}

static int copy_bounded_string(char *destination, size_t capacity,
                               const char *source) {
  if (source == NULL || capacity == 0) {
    return 0;
  }
  size_t len = 0;
  while (len < capacity && source[len] != 0) {
    destination[len] = source[len];
    ++len;
  }
  if (len == 0 || len == capacity) {
    return 0;
  }
  destination[len] = 0;
  return 1;
}

static int write_new_file(const char *path, const unsigned char *bytes,
                          size_t len) {
  long descriptor = raw_syscall6(SYS_openat, AT_FDCWD,
                                 (long)(uintptr_t)path,
                                 O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC,
                                 0600, 0, 0);
  if (descriptor < 0) {
    return 0;
  }
  long written = raw_syscall6(SYS_write, descriptor,
                              (long)(uintptr_t)bytes, (long)len, 0, 0, 0);
  long closed = raw_syscall6(SYS_close, descriptor, 0, 0, 0, 0, 0);
  return written == (long)len && closed == 0;
}

static int write_pid_file(const char *path) {
  long pid = raw_syscall6(SYS_getpid, 0, 0, 0, 0, 0, 0);
  if (pid <= 0) {
    return 0;
  }
  unsigned char reversed[32];
  size_t digits = 0;
  unsigned long value = (unsigned long)pid;
  do {
    reversed[digits++] = (unsigned char)('0' + value % 10);
    value /= 10;
  } while (value != 0 && digits < sizeof(reversed) - 1);
  if (value != 0) {
    return 0;
  }
  unsigned char text[32];
  for (size_t index = 0; index < digits; ++index) {
    text[index] = reversed[digits - index - 1];
  }
  text[digits] = '\n';
  return write_new_file(path, text, digits + 1);
}

static int parse_decimal_u64(const char **cursor, const char *end,
                             uint64_t *result);

static int parse_descriptor_number(const char *text, int *descriptor) {
  if (text == NULL) {
    return 0;
  }
  const char *end = text;
  while ((size_t)(end - text) < 32 && *end != 0) {
    ++end;
  }
  if (end == text || *end != 0) {
    return 0;
  }
  const char *cursor = text;
  uint64_t value = 0;
  if (!parse_decimal_u64(&cursor, end, &value) || cursor != end ||
      value < 3 || value > INT_MAX) {
    return 0;
  }
  *descriptor = (int)value;
  return 1;
}

static void put_u64_le(unsigned char *destination, uint64_t value) {
  for (size_t index = 0; index < sizeof(value); ++index) {
    destination[index] = (unsigned char)(value >> (index * 8));
  }
}

static int publish_child_executable_identity(const char *path,
                                             int retained_descriptor) {
  struct stat executable_status;
  struct stat retained_status;
  long pid = raw_syscall6(SYS_getpid, 0, 0, 0, 0, 0, 0);
  long executable_result =
      raw_syscall6(SYS_newfstatat, AT_FDCWD,
                   (long)(uintptr_t)proc_self_exe_path,
                   (long)(uintptr_t)&executable_status, 0, 0, 0);
  long retained_result =
      raw_syscall6(SYS_fstat, retained_descriptor,
                   (long)(uintptr_t)&retained_status, 0, 0, 0, 0);
  long descriptor_flags =
      raw_syscall6(SYS_fcntl, retained_descriptor, F_GETFD, 0, 0, 0, 0);
  if (pid <= 0 || executable_result != 0 || retained_result != 0 ||
      descriptor_flags != 0 || !S_ISREG(executable_status.st_mode) ||
      !S_ISREG(retained_status.st_mode) || executable_status.st_size <= 0 ||
      retained_status.st_size <= 0 ||
      executable_status.st_dev != retained_status.st_dev ||
      executable_status.st_ino != retained_status.st_ino ||
      executable_status.st_mode != retained_status.st_mode ||
      executable_status.st_size != retained_status.st_size) {
    return 0;
  }

  unsigned char record[16 + 11 * sizeof(uint64_t)];
  copy_bytes(record, child_identity_magic, sizeof(child_identity_magic));
  uint64_t fields[11] = {
      (uint64_t)pid,
      (uint64_t)executable_status.st_dev,
      (uint64_t)executable_status.st_ino,
      (uint64_t)executable_status.st_mode,
      (uint64_t)executable_status.st_size,
      (uint64_t)retained_status.st_dev,
      (uint64_t)retained_status.st_ino,
      (uint64_t)retained_status.st_mode,
      (uint64_t)retained_status.st_size,
      (uint64_t)(unsigned int)retained_descriptor,
      (uint64_t)descriptor_flags,
  };
  for (size_t index = 0; index < 11; ++index) {
    put_u64_le(record + sizeof(child_identity_magic) +
                   index * sizeof(uint64_t),
               fields[index]);
  }
  return write_new_file(path, record, sizeof(record));
}

static int system_preload_is_absent(void) {
  static const char system_preload_path[] = "/etc/ld.so.preload";
  long descriptor = raw_syscall6(SYS_openat, AT_FDCWD,
                                 (long)(uintptr_t)system_preload_path,
                                 O_RDONLY | O_CLOEXEC, 0, 0, 0);
  if (descriptor >= 0) {
    (void)raw_syscall6(SYS_close, descriptor, 0, 0, 0, 0, 0);
    return 0;
  }
  return descriptor == -ENOENT;
}

static int wait_for_parent_release(const char *path) {
  unsigned char expected[sizeof(child_release_bytes) - 1];
  for (size_t attempt = 0; attempt < MAX_RELEASE_POLLS; ++attempt) {
    long descriptor = raw_syscall6(SYS_openat, AT_FDCWD,
                                   (long)(uintptr_t)path,
                                   O_RDONLY | O_CLOEXEC, 0, 0, 0);
    if (descriptor == -ENOENT) {
      if (raw_syscall6(SYS_sched_yield, 0, 0, 0, 0, 0, 0) != 0) {
        return 0;
      }
      continue;
    }
    if (descriptor < 0) {
      return 0;
    }
    long count;
    do {
      count = raw_syscall6(SYS_read, descriptor,
                           (long)(uintptr_t)expected,
                           (long)sizeof(expected), 0, 0, 0);
    } while (count == -EINTR);
    unsigned char extra = 0;
    long trailing;
    do {
      trailing = raw_syscall6(SYS_read, descriptor,
                              (long)(uintptr_t)&extra, 1, 0, 0, 0);
    } while (trailing == -EINTR);
    long closed = raw_syscall6(SYS_close, descriptor, 0, 0, 0, 0, 0);
    return count == (long)sizeof(expected) && trailing == 0 && closed == 0 &&
           bytes_equal(expected, child_release_bytes, sizeof(expected));
  }
  return 0;
}

static uint32_t sysv_name_hash(const unsigned char *name, size_t len) {
  uint32_t hash = 0;
  for (size_t index = 0; index < len; ++index) {
    hash = (hash << 4) + name[index];
    uint32_t high = hash & UINT32_C(0xf0000000);
    if (high != 0) {
      hash ^= high >> 24;
    }
    hash &= ~high;
  }
  return hash;
}

static uint32_t gnu_name_hash(const unsigned char *name, size_t len) {
  uint32_t hash = 5381;
  for (size_t index = 0; index < len; ++index) {
    hash = hash * 33 + name[index];
  }
  return hash;
}

static int range_fits(size_t offset, size_t count, size_t element_size,
                      size_t limit) {
  if (element_size != 0 && count > (SIZE_MAX / element_size)) {
    return 0;
  }
  size_t bytes = count * element_size;
  return offset <= limit && bytes <= limit - offset;
}

static int u64_range_fits(uint64_t offset, uint64_t size, size_t limit) {
  return offset <= (uint64_t)limit && size <= (uint64_t)limit - offset;
}

static int round_up(size_t value, size_t alignment, size_t *result) {
  if (alignment == 0 || (alignment & (alignment - 1)) != 0 ||
      value > SIZE_MAX - (alignment - 1)) {
    return 0;
  }
  *result = (value + alignment - 1) & ~(alignment - 1);
  return 1;
}

static int read_object(const unsigned char *base, size_t len, size_t offset,
                       void *destination, size_t size) {
  if (!range_fits(offset, 1, size, len)) {
    return 0;
  }
  copy_bytes((unsigned char *)destination, base + offset, size);
  return 1;
}

static int parse_hex(const char **cursor, const char *end,
                     uintptr_t *result) {
  const char *position = *cursor;
  uintptr_t value = 0;
  size_t digits = 0;
  while (position < end) {
    unsigned int digit;
    if (*position >= '0' && *position <= '9') {
      digit = (unsigned int)(*position - '0');
    } else if (*position >= 'a' && *position <= 'f') {
      digit = (unsigned int)(*position - 'a') + 10;
    } else {
      break;
    }
    if (value > (UINTPTR_MAX - digit) / 16) {
      return 0;
    }
    value = value * 16 + digit;
    ++digits;
    ++position;
  }
  if (digits == 0) {
    return 0;
  }
  *cursor = position;
  *result = value;
  return 1;
}

static int parse_decimal_u64(const char **cursor, const char *end,
                             uint64_t *result) {
  const char *position = *cursor;
  uint64_t value = 0;
  size_t digits = 0;
  while (position < end && *position >= '0' && *position <= '9') {
    unsigned int digit = (unsigned int)(*position - '0');
    if (value > (UINT64_MAX - digit) / 10) {
      return 0;
    }
    value = value * 10 + digit;
    ++digits;
    ++position;
  }
  if (digits == 0) {
    return 0;
  }
  *cursor = position;
  *result = value;
  return 1;
}

static void skip_spaces(const char **cursor, const char *end) {
  while (*cursor < end && (**cursor == ' ' || **cursor == '\t')) {
    ++*cursor;
  }
}

static int parse_vdso_line(const char *line, const char *end,
                           struct vdso_map *mapping) {
  const char *cursor = line;
  uintptr_t start;
  uintptr_t finish;
  uintptr_t offset;
  uintptr_t device_major;
  uintptr_t device_minor;
  uint64_t inode;
  if (!parse_hex(&cursor, end, &start) || cursor == end || *cursor != '-') {
    return 0;
  }
  ++cursor;
  if (!parse_hex(&cursor, end, &finish)) {
    return 0;
  }
  const char *separator = cursor;
  skip_spaces(&cursor, end);
  if (separator == cursor || (size_t)(end - cursor) < 4 ||
      !bytes_equal((const unsigned char *)cursor,
                   (const unsigned char *)vdso_permissions,
                   sizeof(vdso_permissions) - 1)) {
    return 0;
  }
  cursor += 4;
  separator = cursor;
  skip_spaces(&cursor, end);
  if (separator == cursor || !parse_hex(&cursor, end, &offset) || offset != 0) {
    return 0;
  }
  separator = cursor;
  skip_spaces(&cursor, end);
  if (separator == cursor || !parse_hex(&cursor, end, &device_major) ||
      cursor == end || *cursor != ':') {
    return 0;
  }
  ++cursor;
  if (!parse_hex(&cursor, end, &device_minor)) {
    return 0;
  }
  separator = cursor;
  skip_spaces(&cursor, end);
  if (separator == cursor || !parse_decimal_u64(&cursor, end, &inode)) {
    return 0;
  }
  separator = cursor;
  skip_spaces(&cursor, end);
  if (separator == cursor) {
    return 0;
  }
  if ((size_t)(end - cursor) != sizeof(vdso_path) - 1 ||
      !bytes_equal((const unsigned char *)cursor,
                   (const unsigned char *)vdso_path,
                   sizeof(vdso_path) - 1) ||
      finish <= start || finish - start > SIZE_MAX) {
    return 0;
  }
  mapping->start = start;
  mapping->end = finish;
  mapping->len = (size_t)(finish - start);
  mapping->file_offset = offset;
  mapping->device_major = device_major;
  mapping->device_minor = device_minor;
  mapping->inode = inode;
  copy_bytes((unsigned char *)mapping->permissions,
             (const unsigned char *)vdso_permissions,
             sizeof(vdso_permissions));
  copy_bytes((unsigned char *)mapping->pathname,
             (const unsigned char *)vdso_path, sizeof(vdso_path));
  return 1;
}

static int mappings_equal(const struct vdso_map *left,
                          const struct vdso_map *right) {
  return left->start == right->start && left->end == right->end &&
         left->len == right->len &&
         left->file_offset == right->file_offset &&
         left->device_major == right->device_major &&
         left->device_minor == right->device_minor &&
         left->inode == right->inode &&
         bytes_equal((const unsigned char *)left->permissions,
                     (const unsigned char *)right->permissions,
                     sizeof(left->permissions)) &&
         bytes_equal((const unsigned char *)left->pathname,
                     (const unsigned char *)right->pathname,
                     sizeof(left->pathname));
}

static int find_vdso_mapping(uintptr_t expected_base, size_t page_size,
                             struct vdso_map *mapping) {
  static const char maps_path[] = "/proc/self/maps";
  long descriptor = raw_syscall6(SYS_openat, AT_FDCWD,
                                 (long)(uintptr_t)maps_path,
                                 O_RDONLY | O_CLOEXEC, 0, 0, 0);
  if (descriptor < 0) {
    return 0;
  }
  size_t used = 0;
  int complete = 0;
  while (used < sizeof(maps_bytes)) {
    long count = raw_syscall6(SYS_read, descriptor,
                              (long)(uintptr_t)(maps_bytes + used),
                              (long)(sizeof(maps_bytes) - used), 0, 0, 0);
    if (count < 0) {
      (void)raw_syscall6(SYS_close, descriptor, 0, 0, 0, 0, 0);
      return 0;
    }
    if (count == 0) {
      complete = 1;
      break;
    }
    used += (size_t)count;
  }
  if (raw_syscall6(SYS_close, descriptor, 0, 0, 0, 0, 0) != 0 ||
      !complete) {
    return 0;
  }

  size_t matches = 0;
  size_t start = 0;
  for (size_t index = 0; index <= used; ++index) {
    if (index != used && maps_bytes[index] != '\n') {
      continue;
    }
    const char *line = maps_bytes + start;
    const char *end = maps_bytes + index;
    if ((size_t)(end - line) >= sizeof(vdso_path) - 1 &&
        bytes_equal((const unsigned char *)(end - (sizeof(vdso_path) - 1)),
                    (const unsigned char *)vdso_path,
                    sizeof(vdso_path) - 1)) {
      struct vdso_map candidate;
      if (!parse_vdso_line(line, end, &candidate)) {
        return 0;
      }
      *mapping = candidate;
      ++matches;
    }
    start = index + 1;
  }

  return matches == 1 && mapping->start == expected_base &&
         mapping->len != 0 && mapping->len <= MAX_VDSO_BYTES &&
         mapping->start % page_size == 0 && mapping->len % page_size == 0;
}

static int find_initial_auxv(char **environment, uintptr_t *vdso_base,
                             size_t *page_size) {
  if (environment == NULL) {
    return 0;
  }
  size_t environment_count = 0;
  while (environment_count < MAX_ENV_POINTERS &&
         environment[environment_count] != NULL) {
    ++environment_count;
  }
  if (environment_count == MAX_ENV_POINTERS) {
    return 0;
  }
  const Elf64_auxv_t *auxv =
      (const Elf64_auxv_t *)(environment + environment_count + 1);
  size_t vdso_count = 0;
  size_t page_count = 0;
  int terminated = 0;
  for (size_t index = 0; index < MAX_AUXV_ENTRIES; ++index) {
    if (auxv[index].a_type == AT_NULL) {
      terminated = 1;
      break;
    }
    if (auxv[index].a_type == AT_SYSINFO_EHDR) {
      *vdso_base = (uintptr_t)auxv[index].a_un.a_val;
      ++vdso_count;
    } else if (auxv[index].a_type == AT_PAGESZ) {
      if (auxv[index].a_un.a_val > SIZE_MAX) {
        return 0;
      }
      *page_size = (size_t)auxv[index].a_un.a_val;
      ++page_count;
    }
  }
  return terminated && vdso_count == 1 && page_count == 1 &&
         *vdso_base != 0 && *page_size >= 4096 &&
         *page_size <= MAX_VDSO_BYTES &&
         (*page_size & (*page_size - 1)) == 0;
}

static int vaddr_to_offset(const Elf64_Phdr *load, uint64_t address,
                           uint64_t size, size_t image_len,
                           size_t *offset) {
  if (address < load->p_vaddr ||
      address - load->p_vaddr > load->p_filesz ||
      size > load->p_filesz - (address - load->p_vaddr)) {
    return 0;
  }
  uint64_t relative = address - load->p_vaddr;
  if (relative > UINT64_MAX - load->p_offset) {
    return 0;
  }
  uint64_t file_offset = load->p_offset + relative;
  if (file_offset > SIZE_MAX ||
      !u64_range_fits(file_offset, size, image_len)) {
    return 0;
  }
  *offset = (size_t)file_offset;
  return 1;
}

static int validate_elf(const unsigned char *base, size_t len,
                        size_t page_size, struct dynamic_view *view) {
  Elf64_Ehdr header;
  if (!read_object(base, len, 0, &header, sizeof(header)) ||
      header.e_ident[EI_MAG0] != ELFMAG0 ||
      header.e_ident[EI_MAG1] != ELFMAG1 ||
      header.e_ident[EI_MAG2] != ELFMAG2 ||
      header.e_ident[EI_MAG3] != ELFMAG3 ||
      header.e_ident[EI_CLASS] != ELFCLASS64 ||
      header.e_ident[EI_DATA] != ELFDATA2LSB ||
      header.e_ident[EI_VERSION] != EV_CURRENT ||
      header.e_version != EV_CURRENT || header.e_type != ET_DYN ||
      header.e_machine != EM_X86_64 ||
      header.e_ehsize != sizeof(Elf64_Ehdr) ||
      header.e_phentsize != sizeof(Elf64_Phdr) || header.e_phnum == 0 ||
      header.e_phnum > MAX_PROGRAM_HEADERS ||
      header.e_shentsize != sizeof(Elf64_Shdr) || header.e_shnum == 0 ||
      header.e_shnum > MAX_SECTION_HEADERS ||
      header.e_shstrndx >= header.e_shnum ||
      !range_fits((size_t)header.e_phoff, header.e_phnum,
                  sizeof(Elf64_Phdr), len) ||
      !range_fits((size_t)header.e_shoff, header.e_shnum,
                  sizeof(Elf64_Shdr), len)) {
    return 0;
  }

  Elf64_Phdr headers[MAX_PROGRAM_HEADERS];
  size_t load_count = 0;
  size_t dynamic_count = 0;
  Elf64_Phdr load = {0};
  Elf64_Phdr dynamic = {0};
  for (size_t index = 0; index < header.e_phnum; ++index) {
    if (!read_object(base, len,
                     (size_t)header.e_phoff + index * sizeof(Elf64_Phdr),
                     &headers[index], sizeof(Elf64_Phdr)) ||
        !u64_range_fits(headers[index].p_offset, headers[index].p_filesz,
                        len)) {
      return 0;
    }
    if (headers[index].p_type == PT_LOAD) {
      load = headers[index];
      ++load_count;
    } else if (headers[index].p_type == PT_DYNAMIC) {
      dynamic = headers[index];
      ++dynamic_count;
    }
  }
  size_t rounded_load;
  if (load_count != 1 || dynamic_count != 1 || load.p_offset != 0 ||
      load.p_vaddr != 0 || load.p_filesz == 0 ||
      load.p_filesz != load.p_memsz || load.p_filesz > SIZE_MAX ||
      load.p_align != page_size || load.p_flags != (PF_R | PF_X) ||
      !round_up((size_t)load.p_memsz, page_size, &rounded_load) ||
      rounded_load != len || dynamic.p_offset != dynamic.p_vaddr ||
      dynamic.p_filesz == 0 || dynamic.p_filesz != dynamic.p_memsz ||
      dynamic.p_filesz % sizeof(Elf64_Dyn) != 0 ||
      !u64_range_fits(dynamic.p_vaddr, dynamic.p_memsz, len)) {
    return 0;
  }
  size_t dynamic_load_offset;
  if (!vaddr_to_offset(&load, dynamic.p_vaddr, dynamic.p_memsz, len,
                       &dynamic_load_offset) ||
      dynamic_load_offset != dynamic.p_offset) {
    return 0;
  }

  Elf64_Shdr dynamic_symbols = {0};
  Elf64_Shdr sysv_hash_section = {0};
  Elf64_Shdr gnu_hash_section = {0};
  size_t dynamic_symbol_sections = 0;
  size_t sysv_hash_sections = 0;
  size_t gnu_hash_sections = 0;
  size_t dynamic_symbol_section_index = 0;
  for (size_t index = 0; index < header.e_shnum; ++index) {
    Elf64_Shdr section;
    if (!read_object(base, len,
                     (size_t)header.e_shoff + index * sizeof(Elf64_Shdr),
                     &section, sizeof(section))) {
      return 0;
    }
    if (section.sh_type != SHT_NOBITS &&
        !u64_range_fits(section.sh_offset, section.sh_size, len)) {
      return 0;
    }
    if ((section.sh_flags & SHF_ALLOC) != 0 &&
        (!u64_range_fits(section.sh_addr, section.sh_size, len) ||
         section.sh_addr != section.sh_offset)) {
      return 0;
    }
    if ((section.sh_flags & SHF_EXECINSTR) != 0 &&
        ((section.sh_flags & SHF_ALLOC) == 0 ||
         (section.sh_flags & SHF_WRITE) != 0)) {
      return 0;
    }
    if (section.sh_type == SHT_DYNSYM) {
      dynamic_symbols = section;
      dynamic_symbol_section_index = index;
      ++dynamic_symbol_sections;
    } else if (section.sh_type == SHT_HASH) {
      sysv_hash_section = section;
      ++sysv_hash_sections;
    } else if (section.sh_type == SHT_GNU_HASH) {
      gnu_hash_section = section;
      ++gnu_hash_sections;
    }
  }
  Elf64_Shdr section_names;
  if (!read_object(base, len,
                   (size_t)header.e_shoff +
                       (size_t)header.e_shstrndx * sizeof(Elf64_Shdr),
                   &section_names, sizeof(section_names)) ||
      section_names.sh_type != SHT_STRTAB ||
      !u64_range_fits(section_names.sh_offset, section_names.sh_size, len) ||
      dynamic_symbol_sections != 1 || sysv_hash_sections != 1 ||
      gnu_hash_sections != 1 ||
      dynamic_symbols.sh_entsize != sizeof(Elf64_Sym) ||
      dynamic_symbols.sh_link >= header.e_shnum) {
    return 0;
  }

  uint64_t string_address = 0;
  uint64_t string_size = 0;
  uint64_t symbol_address = 0;
  uint64_t symbol_entry_size = 0;
  uint64_t hash_address = 0;
  uint64_t gnu_hash_address = 0;
  size_t string_tags = 0;
  size_t string_size_tags = 0;
  size_t symbol_tags = 0;
  size_t symbol_entry_tags = 0;
  size_t hash_tags = 0;
  size_t gnu_hash_tags = 0;
  int dynamic_terminated = 0;
  size_t dynamic_entries = (size_t)(dynamic.p_filesz / sizeof(Elf64_Dyn));
  for (size_t index = 0; index < dynamic_entries; ++index) {
    Elf64_Dyn entry;
    if (!read_object(base, len,
                     (size_t)dynamic.p_offset + index * sizeof(Elf64_Dyn),
                     &entry, sizeof(entry))) {
      return 0;
    }
    if (entry.d_tag == DT_NULL) {
      dynamic_terminated = 1;
      break;
    }
    switch (entry.d_tag) {
    case DT_STRTAB:
      string_address = entry.d_un.d_ptr;
      ++string_tags;
      break;
    case DT_STRSZ:
      string_size = entry.d_un.d_val;
      ++string_size_tags;
      break;
    case DT_SYMTAB:
      symbol_address = entry.d_un.d_ptr;
      ++symbol_tags;
      break;
    case DT_SYMENT:
      symbol_entry_size = entry.d_un.d_val;
      ++symbol_entry_tags;
      break;
    case DT_HASH:
      hash_address = entry.d_un.d_ptr;
      ++hash_tags;
      break;
    case DT_GNU_HASH:
      gnu_hash_address = entry.d_un.d_ptr;
      ++gnu_hash_tags;
      break;
    default:
      break;
    }
  }
  if (!dynamic_terminated || string_tags != 1 || string_size_tags != 1 ||
      symbol_tags != 1 || symbol_entry_tags != 1 || hash_tags != 1 ||
      gnu_hash_tags != 1 || string_size == 0 || string_size > len ||
      symbol_entry_size != sizeof(Elf64_Sym)) {
    return 0;
  }

  size_t hash_offset;
  uint32_t hash_header[2];
  if (!vaddr_to_offset(&load, hash_address, sizeof(hash_header), len,
                       &hash_offset) ||
      !read_object(base, len, hash_offset, hash_header,
                   sizeof(hash_header)) ||
      hash_header[0] == 0 || hash_header[0] > MAX_HASH_BUCKETS ||
      hash_header[1] == 0 || hash_header[1] > MAX_DYNAMIC_SYMBOLS) {
    return 0;
  }
  uint64_t hash_words = UINT64_C(2) + hash_header[0] + hash_header[1];
  if (hash_words > UINT64_MAX / sizeof(uint32_t)) {
    return 0;
  }
  uint64_t hash_bytes = hash_words * sizeof(uint32_t);
  if (!vaddr_to_offset(&load, hash_address, hash_bytes, len, &hash_offset)) {
    return 0;
  }

  size_t gnu_hash_offset;
  uint32_t gnu_header[4];
  if (!vaddr_to_offset(&load, gnu_hash_address, sizeof(gnu_header), len,
                       &gnu_hash_offset) ||
      !read_object(base, len, gnu_hash_offset, gnu_header,
                   sizeof(gnu_header)) ||
      gnu_header[0] == 0 || gnu_header[0] > MAX_HASH_BUCKETS ||
      gnu_header[1] == 0 || gnu_header[1] > hash_header[1] ||
      gnu_header[2] == 0 || gnu_header[2] > MAX_GNU_BLOOM_WORDS ||
      (gnu_header[2] & (gnu_header[2] - 1)) != 0 ||
      gnu_header[3] >= 64) {
    return 0;
  }
  uint64_t gnu_chain_count = hash_header[1] - gnu_header[1];
  uint64_t gnu_hash_bytes = sizeof(gnu_header) +
                            (uint64_t)gnu_header[2] * sizeof(uint64_t) +
                            (uint64_t)gnu_header[0] * sizeof(uint32_t) +
                            gnu_chain_count * sizeof(uint32_t);
  if (!vaddr_to_offset(&load, gnu_hash_address, gnu_hash_bytes, len,
                       &gnu_hash_offset)) {
    return 0;
  }

  size_t string_offset;
  size_t symbol_offset;
  uint64_t symbol_bytes = (uint64_t)hash_header[1] * sizeof(Elf64_Sym);
  if (!vaddr_to_offset(&load, string_address, string_size, len,
                       &string_offset) ||
      !vaddr_to_offset(&load, symbol_address, symbol_bytes, len,
                       &symbol_offset) ||
      dynamic_symbols.sh_addr != symbol_address ||
      dynamic_symbols.sh_offset != symbol_offset ||
      dynamic_symbols.sh_size != symbol_bytes) {
    return 0;
  }
  Elf64_Shdr linked_strings;
  if (!read_object(base, len,
                   (size_t)header.e_shoff +
                       (size_t)dynamic_symbols.sh_link * sizeof(Elf64_Shdr),
                   &linked_strings, sizeof(linked_strings)) ||
      linked_strings.sh_type != SHT_STRTAB ||
      (dynamic_symbols.sh_flags & SHF_ALLOC) == 0 ||
      (dynamic_symbols.sh_flags & (SHF_WRITE | SHF_EXECINSTR)) != 0 ||
      (linked_strings.sh_flags & SHF_ALLOC) == 0 ||
      (linked_strings.sh_flags & (SHF_WRITE | SHF_EXECINSTR)) != 0 ||
      linked_strings.sh_addr != string_address ||
      linked_strings.sh_offset != string_offset ||
      linked_strings.sh_size != string_size ||
      sysv_hash_section.sh_addr != hash_address ||
      sysv_hash_section.sh_offset != hash_offset ||
      sysv_hash_section.sh_size != hash_bytes ||
      (sysv_hash_section.sh_flags & SHF_ALLOC) == 0 ||
      (sysv_hash_section.sh_flags & (SHF_WRITE | SHF_EXECINSTR)) != 0 ||
      sysv_hash_section.sh_link != dynamic_symbol_section_index ||
      gnu_hash_section.sh_addr != gnu_hash_address ||
      gnu_hash_section.sh_offset != gnu_hash_offset ||
      gnu_hash_section.sh_size != gnu_hash_bytes ||
      (gnu_hash_section.sh_flags & SHF_ALLOC) == 0 ||
      (gnu_hash_section.sh_flags & (SHF_WRITE | SHF_EXECINSTR)) != 0 ||
      gnu_hash_section.sh_link != dynamic_symbol_section_index) {
    return 0;
  }
  Elf64_Shdr sysv_link;
  Elf64_Shdr gnu_link;
  if (!read_object(base, len,
                   (size_t)header.e_shoff +
                       (size_t)sysv_hash_section.sh_link * sizeof(Elf64_Shdr),
                   &sysv_link, sizeof(sysv_link)) ||
      !read_object(base, len,
                   (size_t)header.e_shoff +
                       (size_t)gnu_hash_section.sh_link * sizeof(Elf64_Shdr),
                   &gnu_link, sizeof(gnu_link)) ||
      sysv_link.sh_offset != dynamic_symbols.sh_offset ||
      gnu_link.sh_offset != dynamic_symbols.sh_offset) {
    return 0;
  }

  view->string_offset = string_offset;
  view->string_size = (size_t)string_size;
  view->symbol_offset = symbol_offset;
  view->symbol_count = hash_header[1];
  view->section_offset = (size_t)header.e_shoff;
  view->section_count = header.e_shnum;
  view->sysv_bucket_count = hash_header[0];
  view->sysv_buckets_offset =
      hash_offset + 2 * sizeof(uint32_t);
  view->sysv_chains_offset =
      view->sysv_buckets_offset + hash_header[0] * sizeof(uint32_t);
  view->gnu_bucket_count = gnu_header[0];
  view->gnu_symbol_offset = gnu_header[1];
  view->gnu_bloom_count = gnu_header[2];
  view->gnu_bloom_shift = gnu_header[3];
  view->gnu_bloom_offset = gnu_hash_offset + sizeof(gnu_header);
  view->gnu_buckets_offset =
      view->gnu_bloom_offset + gnu_header[2] * sizeof(uint64_t);
  view->gnu_chains_offset =
      view->gnu_buckets_offset + gnu_header[0] * sizeof(uint32_t);
  view->load = load;
  return 1;
}

static int symbol_name(const unsigned char *base,
                       const struct dynamic_view *view, uint32_t name_offset,
                       const unsigned char **name, size_t *name_len) {
  if (name_offset >= view->string_size) {
    return 0;
  }
  const unsigned char *start =
      base + view->string_offset + (size_t)name_offset;
  size_t limit = view->string_size - (size_t)name_offset;
  size_t len = 0;
  while (len < limit && start[len] != 0) {
    ++len;
  }
  if (len == limit) {
    return 0;
  }
  *name = start;
  *name_len = len;
  return 1;
}

static int name_matches(const unsigned char *name, size_t name_len,
                        const char *expected, size_t expected_len) {
  return name_len == expected_len &&
         bytes_equal(name, (const unsigned char *)expected, expected_len);
}

static int validate_hash_tables(const unsigned char *base, size_t len,
                                const struct dynamic_view *view) {
  unsigned char sysv_seen[MAX_DYNAMIC_SYMBOLS] = {0};
  uint32_t chain_zero;
  if (!read_object(base, len, view->sysv_chains_offset, &chain_zero,
                   sizeof(chain_zero)) ||
      chain_zero != STN_UNDEF) {
    return 0;
  }
  for (size_t bucket = 0; bucket < view->sysv_bucket_count; ++bucket) {
    uint32_t symbol;
    if (!read_object(base, len,
                     view->sysv_buckets_offset + bucket * sizeof(uint32_t),
                     &symbol, sizeof(symbol))) {
      return 0;
    }
    size_t steps = 0;
    while (symbol != STN_UNDEF) {
      if (symbol >= view->symbol_count || sysv_seen[symbol] != 0 ||
          ++steps > view->symbol_count) {
        return 0;
      }
      const unsigned char *name;
      size_t name_len;
      Elf64_Sym entry;
      if (!read_object(base, len,
                       view->symbol_offset + symbol * sizeof(Elf64_Sym),
                       &entry, sizeof(entry)) ||
          !symbol_name(base, view, entry.st_name, &name, &name_len) ||
          sysv_name_hash(name, name_len) % view->sysv_bucket_count != bucket) {
        return 0;
      }
      sysv_seen[symbol] = 1;
      if (!read_object(base, len,
                       view->sysv_chains_offset + symbol * sizeof(uint32_t),
                       &symbol, sizeof(symbol))) {
        return 0;
      }
    }
  }
  for (size_t symbol = 1; symbol < view->symbol_count; ++symbol) {
    if (sysv_seen[symbol] != 1) {
      return 0;
    }
  }

  unsigned char gnu_seen[MAX_DYNAMIC_SYMBOLS] = {0};
  uint64_t expected_bloom[MAX_GNU_BLOOM_WORDS] = {0};
  for (size_t bucket = 0; bucket < view->gnu_bucket_count; ++bucket) {
    uint32_t symbol;
    if (!read_object(base, len,
                     view->gnu_buckets_offset + bucket * sizeof(uint32_t),
                     &symbol, sizeof(symbol))) {
      return 0;
    }
    if (symbol == STN_UNDEF) {
      continue;
    }
    if (symbol < view->gnu_symbol_offset) {
      return 0;
    }
    size_t steps = 0;
    for (;;) {
      if (symbol >= view->symbol_count || gnu_seen[symbol] != 0 ||
          ++steps > view->symbol_count) {
        return 0;
      }
      Elf64_Sym entry;
      const unsigned char *name;
      size_t name_len;
      uint32_t chain;
      if (!read_object(base, len,
                       view->symbol_offset + symbol * sizeof(Elf64_Sym),
                       &entry, sizeof(entry)) ||
          !symbol_name(base, view, entry.st_name, &name, &name_len) ||
          !read_object(base, len,
                       view->gnu_chains_offset +
                           (symbol - view->gnu_symbol_offset) *
                               sizeof(uint32_t),
                       &chain, sizeof(chain))) {
        return 0;
      }
      uint32_t hash = gnu_name_hash(name, name_len);
      if (hash % view->gnu_bucket_count != bucket ||
          (chain & ~UINT32_C(1)) != (hash & ~UINT32_C(1))) {
        return 0;
      }
      expected_bloom[(hash / 64) % view->gnu_bloom_count] |=
          UINT64_C(1) << (hash % 64);
      expected_bloom[(hash / 64) % view->gnu_bloom_count] |=
          UINT64_C(1) << ((hash >> view->gnu_bloom_shift) % 64);
      gnu_seen[symbol] = 1;
      if ((chain & UINT32_C(1)) != 0) {
        break;
      }
      ++symbol;
    }
  }
  for (size_t symbol = view->gnu_symbol_offset;
       symbol < view->symbol_count; ++symbol) {
    if (gnu_seen[symbol] != 1) {
      return 0;
    }
  }
  for (size_t index = 0; index < view->gnu_bloom_count; ++index) {
    uint64_t observed;
    if (!read_object(base, len,
                     view->gnu_bloom_offset + index * sizeof(uint64_t),
                     &observed, sizeof(observed)) ||
        observed != expected_bloom[index]) {
      return 0;
    }
  }
  return 1;
}

static int sysv_lookup(const unsigned char *base, size_t len,
                       const struct dynamic_view *view,
                       const unsigned char *wanted, size_t wanted_len,
                       size_t *result) {
  uint32_t hash = sysv_name_hash(wanted, wanted_len);
  uint32_t symbol;
  if (!read_object(base, len,
                   view->sysv_buckets_offset +
                       (hash % view->sysv_bucket_count) * sizeof(uint32_t),
                   &symbol, sizeof(symbol))) {
    return 0;
  }
  for (size_t steps = 0; symbol != STN_UNDEF; ++steps) {
    if (steps >= view->symbol_count || symbol >= view->symbol_count) {
      return 0;
    }
    Elf64_Sym entry;
    const unsigned char *name;
    size_t name_len;
    if (!read_object(base, len,
                     view->symbol_offset + symbol * sizeof(Elf64_Sym), &entry,
                     sizeof(entry)) ||
        !symbol_name(base, view, entry.st_name, &name, &name_len)) {
      return 0;
    }
    if (name_len == wanted_len && bytes_equal(name, wanted, wanted_len)) {
      *result = symbol;
      return 1;
    }
    if (!read_object(base, len,
                     view->sysv_chains_offset + symbol * sizeof(uint32_t),
                     &symbol, sizeof(symbol))) {
      return 0;
    }
  }
  *result = SIZE_MAX;
  return 1;
}

static int gnu_lookup(const unsigned char *base, size_t len,
                      const struct dynamic_view *view,
                      const unsigned char *wanted, size_t wanted_len,
                      size_t *result) {
  uint32_t hash = gnu_name_hash(wanted, wanted_len);
  uint64_t bloom;
  if (!read_object(base, len,
                   view->gnu_bloom_offset +
                       ((hash / 64) % view->gnu_bloom_count) * sizeof(uint64_t),
                   &bloom, sizeof(bloom))) {
    return 0;
  }
  uint64_t required = (UINT64_C(1) << (hash % 64)) |
                      (UINT64_C(1)
                       << ((hash >> view->gnu_bloom_shift) % 64));
  if ((bloom & required) != required) {
    *result = SIZE_MAX;
    return 1;
  }

  uint32_t symbol;
  if (!read_object(base, len,
                   view->gnu_buckets_offset +
                       (hash % view->gnu_bucket_count) * sizeof(uint32_t),
                   &symbol, sizeof(symbol))) {
    return 0;
  }
  if (symbol == STN_UNDEF) {
    *result = SIZE_MAX;
    return 1;
  }
  if (symbol < view->gnu_symbol_offset) {
    return 0;
  }
  for (size_t steps = 0;; ++steps, ++symbol) {
    uint32_t chain;
    Elf64_Sym entry;
    const unsigned char *name;
    size_t name_len;
    if (steps >= view->symbol_count || symbol >= view->symbol_count ||
        !read_object(base, len,
                     view->gnu_chains_offset +
                         (symbol - view->gnu_symbol_offset) * sizeof(uint32_t),
                     &chain, sizeof(chain)) ||
        !read_object(base, len,
                     view->symbol_offset + symbol * sizeof(Elf64_Sym), &entry,
                     sizeof(entry)) ||
        !symbol_name(base, view, entry.st_name, &name, &name_len)) {
      return 0;
    }
    if ((chain | UINT32_C(1)) == (hash | UINT32_C(1)) &&
        name_len == wanted_len && bytes_equal(name, wanted, wanted_len)) {
      *result = symbol;
      return 1;
    }
    if ((chain & UINT32_C(1)) != 0) {
      *result = SIZE_MAX;
      return 1;
    }
  }
}

static int validate_named_lookups(const unsigned char *base, size_t len,
                                  const struct dynamic_view *view,
                                  const struct symbol_state *symbols,
                                  int require_present) {
  const unsigned char *original_names[] = {
      (const unsigned char *)canonical_name,
      (const unsigned char *)alias_name,
  };
  const size_t original_lengths[] = {
      sizeof(canonical_name) - 1,
      sizeof(alias_name) - 1,
  };
  const unsigned char *replacement_names[] = {
      (const unsigned char *)renamed_canonical_name,
      (const unsigned char *)renamed_alias_name,
  };
  const size_t replacement_lengths[] = {
      sizeof(renamed_canonical_name) - 1,
      sizeof(renamed_alias_name) - 1,
  };
  const size_t present_indices[] = {
      require_present ? symbols->canonical_index
                      : symbols->renamed_canonical_index,
      require_present ? symbols->alias_index : symbols->renamed_alias_index,
  };
  for (size_t index = 0; index < 2; ++index) {
    const unsigned char *present_name =
        require_present ? original_names[index] : replacement_names[index];
    size_t present_len =
        require_present ? original_lengths[index] : replacement_lengths[index];
    const unsigned char *absent_name =
        require_present ? replacement_names[index] : original_names[index];
    size_t absent_len =
        require_present ? replacement_lengths[index] : original_lengths[index];
    size_t sysv_result;
    size_t gnu_result;
    if (!sysv_lookup(base, len, view, present_name, present_len, &sysv_result) ||
        !gnu_lookup(base, len, view, present_name, present_len, &gnu_result) ||
        sysv_result != present_indices[index] ||
        gnu_result != present_indices[index] ||
        !sysv_lookup(base, len, view, absent_name, absent_len, &sysv_result) ||
        !gnu_lookup(base, len, view, absent_name, absent_len, &gnu_result) ||
        sysv_result != SIZE_MAX || gnu_result != SIZE_MAX) {
      return 0;
    }
  }
  return 1;
}

static int symbol_is_valid_function(const unsigned char *base, size_t len,
                                    const struct dynamic_view *view,
                                    const Elf64_Sym *symbol,
                                    unsigned int expected_binding) {
  if (ELF64_ST_BIND(symbol->st_info) != expected_binding ||
      ELF64_ST_TYPE(symbol->st_info) != STT_FUNC ||
      ELF64_ST_VISIBILITY(symbol->st_other) != STV_DEFAULT ||
      symbol->st_shndx == SHN_UNDEF ||
      symbol->st_shndx >= view->section_count || symbol->st_value == 0 ||
      symbol->st_size == 0 || symbol->st_value < view->load.p_vaddr ||
      symbol->st_value - view->load.p_vaddr > view->load.p_memsz ||
      symbol->st_size >
          view->load.p_memsz - (symbol->st_value - view->load.p_vaddr)) {
    return 0;
  }
  Elf64_Shdr section;
  return read_object(base, len,
                     view->section_offset +
                         (size_t)symbol->st_shndx * sizeof(Elf64_Shdr),
                     &section, sizeof(section)) &&
         (section.sh_flags & (SHF_ALLOC | SHF_EXECINSTR)) ==
             (SHF_ALLOC | SHF_EXECINSTR) &&
         (section.sh_flags & SHF_WRITE) == 0 &&
         section.sh_addr <= symbol->st_value &&
         symbol->st_value <= section.sh_addr + section.sh_size &&
         symbol->st_size <=
             section.sh_addr + section.sh_size - symbol->st_value;
}

static int inspect_symbols(const unsigned char *base, size_t len,
                           const struct dynamic_view *view,
                           int require_present, struct symbol_state *state) {
  struct symbol_state found = {0};
  for (size_t index = 0; index < view->symbol_count; ++index) {
    Elf64_Sym symbol;
    if (!read_object(base, len,
                     view->symbol_offset + index * sizeof(Elf64_Sym),
                     &symbol, sizeof(symbol))) {
      return 0;
    }
    const unsigned char *name;
    size_t name_len;
    if (!symbol_name(base, view, symbol.st_name, &name, &name_len)) {
      return 0;
    }
    if (name_matches(name, name_len, canonical_name,
                     sizeof(canonical_name) - 1)) {
      ++found.canonical_count;
      found.canonical = symbol;
      found.canonical_index = index;
      found.canonical_offset = view->string_offset + symbol.st_name;
    }
    if (name_matches(name, name_len, alias_name, sizeof(alias_name) - 1)) {
      ++found.alias_count;
      found.alias = symbol;
      found.alias_index = index;
      found.alias_offset = view->string_offset + symbol.st_name;
    }
    if (name_matches(name, name_len, renamed_canonical_name,
                     sizeof(renamed_canonical_name) - 1)) {
      ++found.renamed_canonical_count;
      found.renamed_canonical = symbol;
      found.renamed_canonical_index = index;
      found.renamed_canonical_offset = view->string_offset + symbol.st_name;
    }
    if (name_matches(name, name_len, renamed_alias_name,
                     sizeof(renamed_alias_name) - 1)) {
      ++found.renamed_alias_count;
      found.renamed_alias = symbol;
      found.renamed_alias_index = index;
      found.renamed_alias_offset = view->string_offset + symbol.st_name;
    }
  }

  Elf64_Sym first;
  Elf64_Sym second;
  size_t first_index;
  size_t second_index;
  size_t first_offset;
  size_t second_offset;
  size_t first_size;
  size_t second_size;
  if (require_present) {
    if (found.canonical_count != 1 || found.alias_count != 1 ||
        found.renamed_canonical_count != 0 ||
        found.renamed_alias_count != 0) {
      return 0;
    }
    first = found.canonical;
    second = found.alias;
    first_index = found.canonical_index;
    second_index = found.alias_index;
    first_offset = found.canonical_offset;
    second_offset = found.alias_offset;
    first_size = sizeof(canonical_name);
    second_size = sizeof(alias_name);
  } else {
    if (found.canonical_count != 0 || found.alias_count != 0 ||
        found.renamed_canonical_count != 1 ||
        found.renamed_alias_count != 1) {
      return 0;
    }
    first = found.renamed_canonical;
    second = found.renamed_alias;
    first_index = found.renamed_canonical_index;
    second_index = found.renamed_alias_index;
    first_offset = found.renamed_canonical_offset;
    second_offset = found.renamed_alias_offset;
    first_size = sizeof(renamed_canonical_name);
    second_size = sizeof(renamed_alias_name);
  }
  if (!symbol_is_valid_function(base, len, view, &first, STB_GLOBAL) ||
      !symbol_is_valid_function(base, len, view, &second, STB_WEAK) ||
      first.st_value != second.st_value || first.st_size != second.st_size ||
      !range_fits(first_offset, 1, first_size, len) ||
      !range_fits(second_offset, 1, second_size, len) ||
      (first.st_name != 0 && base[first_offset - 1] != 0) ||
      (second.st_name != 0 && base[second_offset - 1] != 0)) {
    return 0;
  }
  size_t first_end = first_offset + first_size;
  size_t second_end = second_offset + second_size;
  if (first_offset < second_end && second_offset < first_end) {
    return 0;
  }
  for (size_t index = 0; index < view->symbol_count; ++index) {
    if (index == first_index || index == second_index) {
      continue;
    }
    Elf64_Sym symbol;
    if (!read_object(base, len,
                     view->symbol_offset + index * sizeof(Elf64_Sym),
                     &symbol, sizeof(symbol))) {
      return 0;
    }
    size_t offset = view->string_offset + symbol.st_name;
    if ((first_offset <= offset && offset < first_end) ||
        (second_offset <= offset && offset < second_end)) {
      return 0;
    }
  }
  *state = found;
  return 1;
}

static int dynamic_views_equal(const struct dynamic_view *left,
                               const struct dynamic_view *right) {
  return left->string_offset == right->string_offset &&
         left->string_size == right->string_size &&
         left->symbol_offset == right->symbol_offset &&
         left->symbol_count == right->symbol_count &&
         left->section_offset == right->section_offset &&
         left->section_count == right->section_count &&
         left->sysv_buckets_offset == right->sysv_buckets_offset &&
         left->sysv_chains_offset == right->sysv_chains_offset &&
         left->sysv_bucket_count == right->sysv_bucket_count &&
         left->gnu_bloom_offset == right->gnu_bloom_offset &&
         left->gnu_buckets_offset == right->gnu_buckets_offset &&
         left->gnu_chains_offset == right->gnu_chains_offset &&
         left->gnu_bucket_count == right->gnu_bucket_count &&
         left->gnu_symbol_offset == right->gnu_symbol_offset &&
         left->gnu_bloom_count == right->gnu_bloom_count &&
         left->gnu_bloom_shift == right->gnu_bloom_shift &&
         left->load.p_type == right->load.p_type &&
         left->load.p_flags == right->load.p_flags &&
         left->load.p_offset == right->load.p_offset &&
         left->load.p_vaddr == right->load.p_vaddr &&
         left->load.p_paddr == right->load.p_paddr &&
         left->load.p_filesz == right->load.p_filesz &&
         left->load.p_memsz == right->load.p_memsz &&
         left->load.p_align == right->load.p_align;
}

static int rebuild_gnu_bloom(unsigned char *base, size_t len,
                             const struct dynamic_view *view) {
  uint64_t bloom[MAX_GNU_BLOOM_WORDS] = {0};
  for (size_t index = view->gnu_symbol_offset; index < view->symbol_count;
       ++index) {
    Elf64_Sym symbol;
    const unsigned char *name;
    size_t name_len;
    if (!read_object(base, len,
                     view->symbol_offset + index * sizeof(Elf64_Sym), &symbol,
                     sizeof(symbol)) ||
        !symbol_name(base, view, symbol.st_name, &name, &name_len)) {
      return 0;
    }
    uint32_t hash = gnu_name_hash(name, name_len);
    bloom[(hash / 64) % view->gnu_bloom_count] |=
        UINT64_C(1) << (hash % 64);
    bloom[(hash / 64) % view->gnu_bloom_count] |=
        UINT64_C(1) << ((hash >> view->gnu_bloom_shift) % 64);
  }
  copy_bytes(base + view->gnu_bloom_offset, (const unsigned char *)bloom,
             view->gnu_bloom_count * sizeof(uint64_t));
  return 1;
}

static int prepare_post_rename_snapshot(
    const unsigned char *base, size_t len, size_t page_size,
    const struct dynamic_view *view, const struct symbol_state *symbols,
    struct rename_plan *plan) {
  if (symbols->canonical_index == symbols->alias_index ||
      symbols->canonical_index < view->gnu_symbol_offset ||
      symbols->alias_index < view->gnu_symbol_offset ||
      symbols->canonical_index >= view->symbol_count ||
      symbols->alias_index >= view->symbol_count ||
      !validate_hash_tables(base, len, view)) {
    return 0;
  }

  uint32_t old_canonical_gnu =
      gnu_name_hash((const unsigned char *)canonical_name,
                    sizeof(canonical_name) - 1);
  uint32_t new_canonical_gnu =
      gnu_name_hash((const unsigned char *)renamed_canonical_name,
                    sizeof(renamed_canonical_name) - 1);
  uint32_t old_alias_gnu =
      gnu_name_hash((const unsigned char *)alias_name, sizeof(alias_name) - 1);
  uint32_t new_alias_gnu = gnu_name_hash(
      (const unsigned char *)renamed_alias_name, sizeof(renamed_alias_name) - 1);
  uint32_t old_canonical_sysv =
      sysv_name_hash((const unsigned char *)canonical_name,
                     sizeof(canonical_name) - 1);
  uint32_t new_canonical_sysv =
      sysv_name_hash((const unsigned char *)renamed_canonical_name,
                     sizeof(renamed_canonical_name) - 1);
  uint32_t old_alias_sysv =
      sysv_name_hash((const unsigned char *)alias_name, sizeof(alias_name) - 1);
  uint32_t new_alias_sysv = sysv_name_hash(
      (const unsigned char *)renamed_alias_name, sizeof(renamed_alias_name) - 1);
  if (old_canonical_gnu % view->gnu_bucket_count !=
          new_canonical_gnu % view->gnu_bucket_count ||
      old_alias_gnu % view->gnu_bucket_count !=
          new_alias_gnu % view->gnu_bucket_count ||
      old_canonical_sysv % view->sysv_bucket_count !=
          new_canonical_sysv % view->sysv_bucket_count ||
      old_alias_sysv % view->sysv_bucket_count !=
          new_alias_sysv % view->sysv_bucket_count) {
    return 0;
  }

  size_t canonical_chain_offset =
      view->gnu_chains_offset +
      (symbols->canonical_index - view->gnu_symbol_offset) * sizeof(uint32_t);
  size_t alias_chain_offset =
      view->gnu_chains_offset +
      (symbols->alias_index - view->gnu_symbol_offset) * sizeof(uint32_t);
  uint32_t canonical_chain;
  uint32_t alias_chain;
  if (canonical_chain_offset == alias_chain_offset ||
      !read_object(base, len, canonical_chain_offset, &canonical_chain,
                   sizeof(canonical_chain)) ||
      !read_object(base, len, alias_chain_offset, &alias_chain,
                   sizeof(alias_chain))) {
    return 0;
  }

  copy_bytes(post_rename_snapshot, base, len);
  copy_bytes(post_rename_snapshot + symbols->canonical_offset,
             (const unsigned char *)renamed_canonical_name,
             sizeof(renamed_canonical_name));
  copy_bytes(post_rename_snapshot + symbols->alias_offset,
             (const unsigned char *)renamed_alias_name,
             sizeof(renamed_alias_name));

  uint32_t renamed_canonical_chain =
      (new_canonical_gnu & ~UINT32_C(1)) |
      (canonical_chain & UINT32_C(1));
  uint32_t renamed_alias_chain =
      (new_alias_gnu & ~UINT32_C(1)) | (alias_chain & UINT32_C(1));
  copy_bytes(post_rename_snapshot + canonical_chain_offset,
             (const unsigned char *)&renamed_canonical_chain,
             sizeof(renamed_canonical_chain));
  copy_bytes(post_rename_snapshot + alias_chain_offset,
             (const unsigned char *)&renamed_alias_chain,
             sizeof(renamed_alias_chain));

  /* The replacement names remain in their original SysV and GNU buckets.
     Thus the bucket arrays, SysV chains, GNU symbol order, and GNU chain end
     markers stay byte-identical.  Only the two GNU chain hashes and the exact
     bloom filter derived from every hashed symbol need new bytes. */
  if (!rebuild_gnu_bloom(post_rename_snapshot, len, view)) {
    return 0;
  }

  struct dynamic_view updated_view;
  struct symbol_state updated_symbols;
  if (!validate_elf(post_rename_snapshot, len, page_size, &updated_view) ||
      !dynamic_views_equal(view, &updated_view) ||
      !inspect_symbols(post_rename_snapshot, len, &updated_view, 0,
                       &updated_symbols) ||
      updated_symbols.renamed_canonical_offset != symbols->canonical_offset ||
      updated_symbols.renamed_alias_offset != symbols->alias_offset ||
      updated_symbols.renamed_canonical_index != symbols->canonical_index ||
      updated_symbols.renamed_alias_index != symbols->alias_index ||
      !validate_hash_tables(post_rename_snapshot, len, &updated_view) ||
      !validate_named_lookups(post_rename_snapshot, len, &updated_view,
                              &updated_symbols, 0)) {
    return 0;
  }

  plan->canonical_chain_offset = canonical_chain_offset;
  plan->alias_chain_offset = alias_chain_offset;
  plan->bloom_offset = view->gnu_bloom_offset;
  plan->bloom_size = view->gnu_bloom_count * sizeof(uint64_t);
  return 1;
}

static int validate_absent_image(const unsigned char *base, size_t len,
                                 size_t page_size,
                                 size_t expected_canonical_offset,
                                 size_t expected_alias_offset,
                                 size_t expected_canonical_index,
                                 size_t expected_alias_index) {
  struct dynamic_view view;
  struct symbol_state state;
  return validate_elf(base, len, page_size, &view) &&
         inspect_symbols(base, len, &view, 0, &state) &&
         validate_hash_tables(base, len, &view) &&
         validate_named_lookups(base, len, &view, &state, 0) &&
         state.renamed_canonical_offset == expected_canonical_offset &&
         state.renamed_alias_offset == expected_alias_offset &&
         state.renamed_canonical_index == expected_canonical_index &&
         state.renamed_alias_index == expected_alias_index;
}

static void rename_vdso_before_constructors(int argc, char **argv,
                                            char **environment) {
  char pid_path[MAX_PATH_BYTES];
  char identity_path[MAX_PATH_BYTES];
  char release_path[MAX_PATH_BYTES];
  int retained_descriptor = -1;
  if (argc != 7 || argv == NULL ||
      !copy_bounded_string(pid_path, sizeof(pid_path), argv[1]) ||
      !copy_bounded_string(retained_armed_path, sizeof(retained_armed_path),
                           argv[2]) ||
      !copy_bounded_string(retained_success_path,
                           sizeof(retained_success_path), argv[3]) ||
      !copy_bounded_string(identity_path, sizeof(identity_path), argv[4]) ||
      !copy_bounded_string(release_path, sizeof(release_path), argv[5]) ||
      !parse_descriptor_number(argv[6], &retained_descriptor)) {
    fail_preinit(10);
  }
  if (!write_pid_file(pid_path)) {
    fail_preinit(11);
  }
  if (!publish_child_executable_identity(identity_path, retained_descriptor)) {
    fail_preinit(12);
  }
  if (!system_preload_is_absent()) {
    fail_preinit(13);
  }
  if (!wait_for_parent_release(release_path)) {
    fail_preinit(14);
  }
  if (raw_syscall6(SYS_close, retained_descriptor, 0, 0, 0, 0, 0) != 0) {
    fail_preinit(15);
  }
  uintptr_t vdso_base = 0;
  size_t page_size = 0;
  if (!find_initial_auxv(environment, &vdso_base, &page_size)) {
    fail_preinit(20);
  }
  struct vdso_map mapping;
  if (!find_vdso_mapping(vdso_base, page_size, &mapping)) {
    fail_preinit(21);
  }
  struct dynamic_view view;
  if (!validate_elf((const unsigned char *)vdso_base, mapping.len, page_size,
                    &view)) {
    fail_preinit(22);
  }
  struct symbol_state symbols;
  if (!inspect_symbols((const unsigned char *)vdso_base, mapping.len, &view, 1,
                       &symbols)) {
    fail_preinit(23);
  }
  if (!validate_hash_tables((const unsigned char *)vdso_base, mapping.len,
                            &view) ||
      !validate_named_lookups((const unsigned char *)vdso_base, mapping.len,
                              &view, &symbols, 1)) {
    fail_preinit(24);
  }
  if (symbols.canonical.st_value > UINTPTR_MAX - vdso_base) {
    fail_preinit(25);
  }
  uintptr_t retained_entry = vdso_base + (uintptr_t)symbols.canonical.st_value;
  struct rename_plan plan;
  if (!prepare_post_rename_snapshot((const unsigned char *)vdso_base,
                                    mapping.len, page_size, &view, &symbols,
                                    &plan)) {
    fail_preinit(26);
  }

  /* All parsing, lookup-table reconstruction, and post-rename validation above
     operate on the private snapshot.  No live vDSO byte changes unless every
     precondition and the complete proposed image have already passed. */
  if (raw_syscall6(SYS_mprotect, (long)vdso_base, (long)mapping.len,
                   PROT_READ | PROT_WRITE | PROT_EXEC, 0, 0, 0) != 0) {
    fail_preinit(27);
  }
  volatile unsigned char *writable = (volatile unsigned char *)vdso_base;
  copy_to_volatile(writable + symbols.canonical_offset,
                   post_rename_snapshot + symbols.canonical_offset,
                   sizeof(renamed_canonical_name));
  copy_to_volatile(writable + symbols.alias_offset,
                   post_rename_snapshot + symbols.alias_offset,
                   sizeof(renamed_alias_name));
  copy_to_volatile(writable + plan.canonical_chain_offset,
                   post_rename_snapshot + plan.canonical_chain_offset,
                   sizeof(uint32_t));
  copy_to_volatile(writable + plan.alias_chain_offset,
                   post_rename_snapshot + plan.alias_chain_offset,
                   sizeof(uint32_t));
  copy_to_volatile(writable + plan.bloom_offset,
                   post_rename_snapshot + plan.bloom_offset, plan.bloom_size);
  if (raw_syscall6(SYS_mprotect, (long)vdso_base, (long)mapping.len,
                   PROT_READ | PROT_EXEC, 0, 0, 0) != 0) {
    fail_preinit(28);
  }
  struct vdso_map restored;
  if (!find_vdso_mapping(vdso_base, page_size, &restored) ||
      !mappings_equal(&restored, &mapping)) {
    fail_preinit(29);
  }
  if (!bytes_equal((const unsigned char *)vdso_base, post_rename_snapshot,
                   mapping.len)) {
    fail_preinit(30);
  }
  if (!validate_absent_image(
          (const unsigned char *)vdso_base, mapping.len, page_size,
          symbols.canonical_offset, symbols.alias_offset,
          symbols.canonical_index, symbols.alias_index)) {
    fail_preinit(31);
  }
  saved_canonical_name_offset = symbols.canonical_offset;
  saved_alias_name_offset = symbols.alias_offset;
  saved_canonical_symbol_index = symbols.canonical_index;
  saved_alias_symbol_index = symbols.alias_index;
  saved_vdso_base = vdso_base;
  saved_vdso_len = mapping.len;
  saved_page_size = page_size;
  saved_vdso_mapping = mapping;
  saved_retained_entry = retained_entry;
  saved_retained_entry_state = retained_entry_armed_magic;
  if (!write_new_file(retained_armed_path,
                      (const unsigned char *)retained_armed_line,
                      sizeof(retained_armed_line) - 1)) {
    fail_preinit(32);
  }
  saved_state = ready_magic;
}

/* ELF executable preinit entries run after relocation and before every
   preload DSO constructor, so LiteInst observes the renamed live image. */
__attribute__((section(".preinit_array"), used)) static void (*const
    before_preload_constructors)(int, char **, char **) =
    rename_vdso_before_constructors;

typedef long (*vdso_getrandom_function)(void *, size_t, unsigned int, void *,
                                        size_t);

int main(void) {
  if (saved_state != ready_magic || saved_vdso_base == 0 ||
      saved_vdso_len == 0 || saved_vdso_len > sizeof(post_rename_snapshot) ||
      saved_retained_entry == 0 ||
      saved_retained_entry_state != retained_entry_armed_magic ||
      retained_success_path[0] == 0) {
    return 40;
  }
  struct vdso_map mapping;
  if (!find_vdso_mapping(saved_vdso_base, saved_page_size, &mapping) ||
      !mappings_equal(&mapping, &saved_vdso_mapping) ||
      !validate_absent_image((const unsigned char *)saved_vdso_base,
                             saved_vdso_len, saved_page_size,
                             saved_canonical_name_offset,
                             saved_alias_name_offset,
                             saved_canonical_symbol_index,
                             saved_alias_symbol_index)) {
    return 41;
  }
  if (!bytes_equal((const unsigned char *)saved_vdso_base,
                   post_rename_snapshot, saved_vdso_len)) {
    return 42;
  }

  unsigned char random_bytes[32];
  /* If the loader wrongly admits this image, the first main-time action calls
     the executable entry saved before both names disappeared. A null opaque
     state forces the admitted function's ordinary syscall fallback. */
  vdso_getrandom_function retained =
      (vdso_getrandom_function)(uintptr_t)saved_retained_entry;
  if (retained(random_bytes, sizeof(random_bytes), 0, NULL, 0) !=
      (long)sizeof(random_bytes)) {
    return 43;
  }
  struct vdso_map after_syscall;
  if (!bytes_equal((const unsigned char *)saved_vdso_base,
                   post_rename_snapshot, saved_vdso_len) ||
      !find_vdso_mapping(saved_vdso_base, saved_page_size, &after_syscall) ||
      !mappings_equal(&after_syscall, &saved_vdso_mapping) ||
      !validate_absent_image((const unsigned char *)saved_vdso_base,
                             saved_vdso_len, saved_page_size,
                             saved_canonical_name_offset,
                             saved_alias_name_offset,
                             saved_canonical_symbol_index,
                             saved_alias_symbol_index)) {
    return 44;
  }
  if (!write_new_file(retained_success_path,
                      (const unsigned char *)retained_success_line,
                      sizeof(retained_success_line) - 1)) {
    return 45;
  }
  return 0;
}
