// Dump the JITed-address -> source-line table the kernel keeps for a loaded
// BPF program, using only the bpf(2) uapi (no libbpf, no bpftool).
//
// Purpose: show exactly which uapi fields carry the mapping, and produce a
// portable artifact that can be run inside a VM guest (build with -static).
//
// Usage:  bpf_riplines <prog_id> [<prog_id> ...]
//         bpf_riplines --all-struct-ops
//
// Reads: BPF_OBJ_GET_INFO_BY_FD on a prog fd (struct bpf_prog_info):
//   jited_ksyms / nr_jited_ksyms      - per-subprogram JITed base addresses
//   jited_func_lens / nr_...          - per-subprogram JITed byte lengths
//   line_info / nr_line_info          - struct bpf_line_info[]: BPF insn index
//                                       -> BTF string offsets (file, line, col)
//   jited_line_info / nr_...          - __u64[]: native address for the i'th
//                                       line_info record
//   btf_id                            - BTF whose string table resolves the
//                                       file_name_off / line_off fields
#define _GNU_SOURCE
#include <errno.h>
#include <linux/bpf.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static int bpf(int cmd, union bpf_attr *attr, unsigned int size) {
  return (int)syscall(__NR_bpf, cmd, attr, size);
}

static int prog_fd_by_id(uint32_t id) {
  union bpf_attr a;
  memset(&a, 0, sizeof(a));
  a.prog_id = id;
  return bpf(BPF_PROG_GET_FD_BY_ID, &a, sizeof(a));
}

static int obj_get_info(int fd, void *info, uint32_t *len) {
  union bpf_attr a;
  memset(&a, 0, sizeof(a));
  a.info.bpf_fd = fd;
  a.info.info_len = *len;
  a.info.info = (uint64_t)(unsigned long)info;
  int err = bpf(BPF_OBJ_GET_INFO_BY_FD, &a, sizeof(a));
  *len = a.info.info_len;
  return err;
}

// Minimal BTF string-table reader.
struct btf_blob {
  char *raw;
  uint32_t size;
  const char *strs;
  uint32_t str_len;
};

struct btf_hdr_min {
  uint16_t magic;
  uint8_t version;
  uint8_t flags;
  uint32_t hdr_len;
  uint32_t type_off, type_len, str_off, str_len;
};

static int load_btf(uint32_t btf_id, struct btf_blob *out) {
  union bpf_attr a;
  memset(&a, 0, sizeof(a));
  a.btf_id = btf_id;
  int fd = bpf(BPF_BTF_GET_FD_BY_ID, &a, sizeof(a));
  if (fd < 0) return -1;

  struct bpf_btf_info bi;
  memset(&bi, 0, sizeof(bi));
  uint32_t len = sizeof(bi);
  if (obj_get_info(fd, &bi, &len)) { close(fd); return -1; }

  out->size = bi.btf_size;
  out->raw = calloc(1, out->size ? out->size : 1);
  memset(&bi, 0, sizeof(bi));
  bi.btf = (uint64_t)(unsigned long)out->raw;
  bi.btf_size = out->size;
  len = sizeof(bi);
  if (obj_get_info(fd, &bi, &len)) { close(fd); return -1; }
  close(fd);

  struct btf_hdr_min *h = (void *)out->raw;
  out->strs = out->raw + h->hdr_len + h->str_off;
  out->str_len = h->str_len;
  return 0;
}

static const char *btf_str(struct btf_blob *b, uint32_t off) {
  if (!b->raw || off >= b->str_len) return "<?>";
  return b->strs + off;
}

static void dump_prog(uint32_t id) {
  int fd = prog_fd_by_id(id);
  if (fd < 0) { fprintf(stderr, "prog %u: get_fd_by_id: %s\n", id, strerror(errno)); return; }

  struct bpf_prog_info info;
  memset(&info, 0, sizeof(info));
  uint32_t len = sizeof(info);
  if (obj_get_info(fd, &info, &len)) {
    fprintf(stderr, "prog %u: get_info: %s\n", id, strerror(errno));
    close(fd);
    return;
  }

  printf("=== prog id=%u name=%s type=%u tag=", id, info.name, info.type);
  for (int i = 0; i < 8; i++) printf("%02x", info.tag[i]);
  printf(" btf_id=%u\n", info.btf_id);
  printf("    jited_prog_len=%u xlated_prog_len=%u nr_jited_ksyms=%u "
         "nr_line_info=%u nr_jited_line_info=%u line_info_rec_size=%u "
         "jited_line_info_rec_size=%u\n",
         info.jited_prog_len, info.xlated_prog_len, info.nr_jited_ksyms,
         info.nr_line_info, info.nr_jited_line_info, info.line_info_rec_size,
         info.jited_line_info_rec_size);

  uint32_t nr_ksyms = info.nr_jited_ksyms;
  uint32_t nr_flens = info.nr_jited_func_lens;
  uint32_t nr_li = info.nr_line_info;
  uint32_t nr_jli = info.nr_jited_line_info;
  uint32_t li_rec = info.line_info_rec_size ? info.line_info_rec_size
                                            : sizeof(struct bpf_line_info);
  uint32_t jli_rec = info.jited_line_info_rec_size ? info.jited_line_info_rec_size : 8;

  uint64_t *ksyms = calloc(nr_ksyms ? nr_ksyms : 1, 8);
  uint32_t *flens = calloc(nr_flens ? nr_flens : 1, 4);
  char *li = calloc(nr_li ? nr_li : 1, li_rec);
  char *jli = calloc(nr_jli ? nr_jli : 1, jli_rec);

  struct bpf_prog_info q;
  memset(&q, 0, sizeof(q));
  q.nr_jited_ksyms = nr_ksyms;
  q.jited_ksyms = (uint64_t)(unsigned long)ksyms;
  q.nr_jited_func_lens = nr_flens;
  q.jited_func_lens = (uint64_t)(unsigned long)flens;
  q.nr_line_info = nr_li;
  q.line_info = (uint64_t)(unsigned long)li;
  q.line_info_rec_size = li_rec;
  q.nr_jited_line_info = nr_jli;
  q.jited_line_info = (uint64_t)(unsigned long)jli;
  q.jited_line_info_rec_size = jli_rec;
  len = sizeof(q);
  if (obj_get_info(fd, &q, &len)) {
    fprintf(stderr, "prog %u: get_info(arrays): %s\n", id, strerror(errno));
    goto out;
  }

  struct btf_blob btf = {0};
  if (info.btf_id) load_btf(info.btf_id, &btf);

  for (uint32_t i = 0; i < q.nr_jited_ksyms; i++)
    printf("    subprog[%u] ksym=0x%016llx len=%u\n", i,
           (unsigned long long)ksyms[i], i < nr_flens ? flens[i] : 0);

  printf("    %-18s %-10s %s\n", "jited_addr", "bpf_insn", "source");
  for (uint32_t i = 0; i < q.nr_line_info; i++) {
    struct bpf_line_info *r = (void *)(li + (size_t)i * li_rec);
    uint64_t addr = 0;
    if (i < q.nr_jited_line_info) memcpy(&addr, jli + (size_t)i * jli_rec, 8);
    printf("    0x%016llx %-10u %s:%u:%u\n", (unsigned long long)addr,
           r->insn_off, btf_str(&btf, r->file_name_off),
           r->line_col >> 10, r->line_col & 0x3ff);
  }
  free(btf.raw);

out:
  free(ksyms); free(flens); free(li); free(jli);
  close(fd);
}

int main(int argc, char **argv) {
  if (argc >= 2 && strcmp(argv[1], "--all-struct-ops") == 0) {
    uint32_t id = 0;
    for (;;) {
      union bpf_attr a;
      memset(&a, 0, sizeof(a));
      a.start_id = id;
      if (bpf(BPF_PROG_GET_NEXT_ID, &a, sizeof(a))) break;
      id = a.next_id;
      int fd = prog_fd_by_id(id);
      if (fd < 0) continue;
      struct bpf_prog_info info;
      memset(&info, 0, sizeof(info));
      uint32_t len = sizeof(info);
      if (!obj_get_info(fd, &info, &len) && info.type == BPF_PROG_TYPE_STRUCT_OPS)
        { close(fd); dump_prog(id); continue; }
      close(fd);
    }
    return 0;
  }
  for (int i = 1; i < argc; i++) dump_prog((uint32_t)strtoul(argv[i], NULL, 0));
  return 0;
}
